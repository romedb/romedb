//! Python owns public objects; operations use the shared lossless client protocol.
use pyo3::{exceptions::PyRuntimeError, prelude::*, types::PyBytes};
use romedb_protocol::{diagnostic, execute, Limits};
use serde_json::json;
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

fn py_error(error: romedb::Error) -> PyErr {
    PyRuntimeError::new_err(diagnostic(&error).to_string())
}
#[pyclass(frozen, module = "romedb._native")]
struct NativeCancellation {
    inner: romedb::CancellationToken,
}
#[pymethods]
impl NativeCancellation {
    #[new]
    #[pyo3(signature=(timeout_ms=None))]
    fn new(timeout_ms: Option<u32>) -> PyResult<Self> {
        let inner = if let Some(ms) = timeout_ms {
            let deadline = Instant::now()
                .checked_add(Duration::from_millis(u64::from(ms)))
                .ok_or_else(|| PyRuntimeError::new_err("deadline overflow"))?;
            romedb::CancellationToken::with_deadline(deadline)
        } else {
            romedb::CancellationToken::new()
        };
        Ok(Self { inner })
    }
    fn cancel(&self) {
        self.inner.cancel();
    }
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
}
#[pyclass(frozen, module = "romedb._native")]
struct NativeDatabase {
    inner: Mutex<Option<(romedb::Connection, romedb::Database)>>,
    interrupt: romedb::InterruptHandle,
}
#[pymethods]
impl NativeDatabase {
    #[new]
    #[pyo3(signature=(path, write_limits=None))]
    fn new(py: Python<'_>, path: String, write_limits: Option<String>) -> PyResult<Self> {
        py.detach(move || {
            let write_limits = write_limits
                .map(|limits| romedb::decode_wire_json::<Limits>(&limits))
                .transpose()
                .map_err(py_error)?;
            let db = romedb::Database::open(&path).map_err(py_error)?;
            let conn = db.connect().map_err(py_error)?;
            let conn = if let Some(limits) = write_limits {
                conn.with_write_buffer_limits(limits.into())
            } else {
                conn
            };
            let interrupt = conn.interrupt_handle();
            Ok(Self {
                inner: Mutex::new(Some((conn, db))),
                interrupt,
            })
        })
    }
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            self.inner
                .lock()
                .map_err(|_| PyRuntimeError::new_err("database lock poisoned"))?
                .take();
            Ok(())
        })
    }
    fn interrupt(&self) -> bool {
        self.interrupt.interrupt()
    }
    fn call(
        &self,
        py: Python<'_>,
        request: String,
        token: &NativeCancellation,
    ) -> PyResult<String> {
        let token = token.inner.clone();
        py.detach(|| {
            let guard = self
                .inner
                .lock()
                .map_err(|_| PyRuntimeError::new_err("database lock poisoned"))?;
            let (conn, _) = guard
                .as_ref()
                .ok_or_else(|| PyRuntimeError::new_err("database is closed"))?;
            let before = conn.transaction_state();
            let result = romedb::decode_wire_json(&request)
                .and_then(|request| execute(conn, request, &token));
            let execution = match result {
                Ok(result) => json!({"result": result}),
                Err(error) => json!({"error": diagnostic(&error)}),
            };
            serde_json::to_string(&json!({
                "version": 1,
                "transaction": {"before": before, "after": conn.transaction_state()},
                "execution": execution,
            }))
            .map_err(|error| PyRuntimeError::new_err(error.to_string()))
        })
    }
}
#[pyfunction]
fn vector(py: Python<'_>, encoding: &str, components: Vec<f64>) -> PyResult<Py<PyBytes>> {
    let value = romedb_protocol::vector(encoding, &components).map_err(py_error)?;
    let romedb::Value::Vector(bytes) = value else {
        unreachable!("vector constructor")
    };
    Ok(PyBytes::new(py, &bytes).unbind())
}
#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<NativeDatabase>()?;
    module.add_class::<NativeCancellation>()?;
    module.add_function(wrap_pyfunction!(vector, module)?)?;
    Ok(())
}
