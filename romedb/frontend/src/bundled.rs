//! Fixed, pure bundled functions in isolated, bounded QuickJS runtimes.
use crate::{Error, Result, Value};
use rquickjs::{Context, Function, Object, Runtime};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
const SOURCE: &str = include_str!("../bundled/strings.js");
const MAX_BYTES: usize = 65_536;
pub(crate) fn call(name: &str, args: &[Value]) -> Result<Value> {
    let (text, form) = match (name, args) {
        ("slugify", [Value::String(text)]) => (text.as_str(), ""),
        ("normalize", [Value::String(text), Value::String(form)])
            if matches!(form.as_str(), "NFC" | "NFD" | "NFKC" | "NFKD") =>
        {
            (text.as_str(), form.as_str())
        }
        _ => {
            return Err(Error::Validation(
                "invalid bundled string function arguments".into(),
            ))
        }
    };
    if text.len() > MAX_BYTES {
        return Err(Error::Limit(
            "bundled string input exceeds 65536 bytes".into(),
        ));
    }
    let output = run(SOURCE, name, text, form, Duration::from_millis(100))?;
    if output.len() > MAX_BYTES {
        return Err(Error::Limit(
            "bundled string output exceeds 65536 bytes".into(),
        ));
    }
    Ok(Value::String(output))
}
fn run(
    source: &'static str,
    name: &str,
    text: &str,
    form: &str,
    budget: Duration,
) -> Result<String> {
    let runtime =
        Runtime::new().map_err(|e| Error::Storage(format!("QuickJS initialization: {e}")))?;
    runtime.set_memory_limit(8 * 1024 * 1024);
    runtime.set_max_stack_size(256 * 1024);
    let interrupted = Arc::new(AtomicBool::new(false));
    let signal = interrupted.clone();
    let deadline = Instant::now() + budget;
    let mut polls = 0usize;
    runtime.set_interrupt_handler(Some(Box::new(move || {
        polls += 1;
        let stop = polls > 1000 || Instant::now() >= deadline;
        if stop {
            signal.store(true, Ordering::Relaxed);
        }
        stop
    })));
    let context =
        Context::full(&runtime).map_err(|e| Error::Storage(format!("QuickJS context: {e}")))?;
    context.with(|ctx| {
        let result = (|| -> rquickjs::Result<String> {
            ctx.eval::<(), _>(source)?;
            let bundle: Object = ctx.globals().get("__romedb_bundle")?;
            let function: Function = bundle.get(name)?;
            function.call((text, form))
        })();
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                if interrupted.load(Ordering::Relaxed) {
                    return Err(Error::Limit("bundled execution budget exceeded".into()));
                }
                let exception = ctx.catch();
                let message = exception
                    .as_object()
                    .and_then(|o| o.get::<_, String>("message").ok())
                    .unwrap_or_else(|| error.to_string());
                if matches!(error, rquickjs::Error::Allocation)
                    || message.contains("out of memory")
                    || message.contains("stack overflow")
                    || message.contains("Maximum call stack size exceeded")
                {
                    Err(Error::Limit(
                        "bundled memory or stack limit exceeded".into(),
                    ))
                } else {
                    Err(Error::Storage(format!(
                        "bundled function failure: {message}"
                    )))
                }
            }
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupt_stops_fixed_runaway_fixture() {
        let err = run(
            "globalThis.__romedb_bundle={slugify(){while(true){}}}",
            "slugify",
            "",
            "",
            Duration::from_millis(10),
        )
        .unwrap_err();
        assert_eq!(err.code(), "FDB_LIMIT");
        assert_eq!(
            call("slugify", &[Value::String("Still works".into())]).unwrap(),
            Value::String("still-works".into())
        );
    }
    #[test]
    fn runtime_has_no_host_io_bindings() {
        let result=run("globalThis.__romedb_bundle={slugify(){return [typeof process,typeof require,typeof fetch,typeof std,typeof os].join(',')}}", "slugify","","",Duration::from_millis(100)).unwrap();
        assert_eq!(result, "undefined,undefined,undefined,undefined,undefined");
    }
}

#[cfg(test)]
mod allocation_tests {
    use super::*;
    #[test]
    fn memory_and_output_limits_are_enforced() {
        let err=run("globalThis.__romedb_bundle={slugify(){let a=[];while(true){a.push(new Array(100000).fill(1))}}}","slugify","","",Duration::from_secs(1)).unwrap_err();
        assert_eq!(err.code(), "FDB_LIMIT");
        let stack = run("globalThis.__romedb_bundle={slugify(){function recur(){return 1+recur()} return String(recur())}}", "slugify", "", "", Duration::from_secs(1)).unwrap_err();
        assert_eq!(stack.code(), "FDB_LIMIT", "{stack:?}");
        let err = call(
            "normalize",
            &[
                Value::String("ﷺ".repeat(6000)),
                Value::String("NFKD".into()),
            ],
        )
        .unwrap_err();
        assert_eq!(err.code(), "FDB_LIMIT");
    }
}
