//! Native ABI shared by PHP, Swift, C# and Go. No upstream core changes.
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    ffi::{c_char, CStr, CString},
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

struct Database {
    // Connection drops before its owning database.
    inner: Mutex<Option<(romedb::Connection, romedb::Database)>>,
    interrupt: romedb::InterruptHandle,
}
#[derive(Default)]
struct Registry {
    next: u64,
    entries: HashMap<u64, Arc<Database>>,
}
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}
fn invalid(message: &str) -> romedb::Error {
    romedb_protocol::invalid(message)
}
fn lookup(handle: u64) -> romedb::Result<Arc<Database>> {
    registry()
        .lock()
        .map_err(|_| invalid("registry poisoned"))?
        .entries
        .get(&handle)
        .cloned()
        .ok_or_else(|| invalid("database is closed or handle is invalid"))
}
fn execution(result: romedb::Result<Value>) -> Value {
    match result {
        Ok(result) => json!({"result":result}),
        Err(error) => json!({"error":romedb_protocol::diagnostic(&error)}),
    }
}
fn boundary(f: impl FnOnce() -> romedb::Result<Value>) -> *mut c_char {
    let value = match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => json!({"version":1,"execution":execution(Err(error))}),
        Err(_) => {
            json!({"version":1,"execution":{"error":{"code":"FDB_INTERNAL","message":"native panic; close this handle"}}})
        }
    };
    // JSON serialization escapes embedded NUL bytes.
    CString::new(value.to_string())
        .expect("JSON contains no NUL")
        .into_raw()
}
fn success(value: Value) -> Value {
    json!({"version":1,"execution":{"result":value}})
}
unsafe fn input<'a>(value: *const c_char) -> romedb::Result<&'a str> {
    if value.is_null() {
        return Err(invalid("null input"));
    }
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .map_err(|_| invalid("input must be UTF-8"))
}
#[no_mangle]
pub extern "C" fn fdb_abi_version() -> u32 {
    1
}

/// Open one native connection.
/// # Safety
/// `path` must be null or a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn fdb_open(path: *const c_char) -> *mut c_char {
    boundary(|| {
        let db = romedb::Database::open(unsafe { input(path)? })?;
        let conn = db.connect()?;
        let database = Arc::new(Database {
            interrupt: conn.interrupt_handle(),
            inner: Mutex::new(Some((conn, db))),
        });
        let mut registry = registry()
            .lock()
            .map_err(|_| invalid("registry poisoned"))?;
        registry.next = registry
            .next
            .checked_add(1)
            .filter(|next| *next <= i64::MAX as u64)
            .ok_or_else(|| invalid("handle space exhausted"))?;
        let handle = registry.next;
        registry.entries.insert(handle, database);
        Ok(success(json!(handle.to_string())))
    })
}

/// Execute a shared-protocol request, preserving positional typed results.
/// # Safety
/// `request` must be null or a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn fdb_call(
    handle: u64,
    request: *const c_char,
    timeout_ms: i64,
) -> *mut c_char {
    boundary(|| {
        let token = match timeout_ms {
            -1 => romedb::CancellationToken::new(),
            ms if ms >= 0 => romedb::CancellationToken::with_deadline(
                Instant::now()
                    .checked_add(Duration::from_millis(ms as u64))
                    .ok_or_else(|| invalid("deadline overflow"))?,
            ),
            _ => return Err(invalid("timeout must be -1 or nonnegative")),
        };
        let request = unsafe { input(request)? };
        let db = lookup(handle)?;
        let guard = db
            .inner
            .lock()
            .map_err(|_| invalid("database poisoned; close it"))?;
        let (conn, _) = guard
            .as_ref()
            .ok_or_else(|| invalid("database is closed"))?;
        let before = conn.transaction_state();
        let result = if token.is_cancelled() {
            json!({"error":{"code":"FDB_CANCELLED","message":"deadline elapsed"}})
        } else {
            execution(
                romedb::decode_wire_json(request)
                    .and_then(|request| romedb_protocol::execute(conn, request, &token)),
            )
        };
        Ok(
            json!({"version":1,"transaction":{"before":before,"after":conn.transaction_state()},"execution":result}),
        )
    })
}

#[no_mangle]
pub extern "C" fn fdb_interrupt(handle: u64) -> *mut c_char {
    boundary(|| Ok(success(json!(lookup(handle)?.interrupt.interrupt()))))
}

#[no_mangle]
pub extern "C" fn fdb_close(handle: u64) -> *mut c_char {
    boundary(|| {
        let db = registry()
            .lock()
            .map_err(|_| invalid("registry poisoned"))?
            .entries
            .get(&handle)
            .cloned();
        if let Some(db) = db {
            // Keep the handle reachable for interruption while close waits.
            // Recover poison only to dispose of a failed connection, never reuse it.
            db.inner.lock().unwrap_or_else(|e| e.into_inner()).take();
            registry()
                .lock()
                .map_err(|_| invalid("registry poisoned"))?
                .entries
                .remove(&handle);
        }
        Ok(success(Value::Null))
    })
}

/// Release a response, or do nothing for null.
/// # Safety
/// The pointer must be null or an unfreed response returned by this library.
#[no_mangle]
pub unsafe extern "C" fn fdb_free(response: *mut c_char) {
    if !response.is_null() {
        drop(unsafe { CString::from_raw(response) });
    }
}
