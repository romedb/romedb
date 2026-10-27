//! Persisted typed scalar functions, evaluated in fresh bounded QuickJS runtimes.
use crate::{text, Connection, Error, Result, Value};
use rquickjs::{context::intrinsic, Context, Function, Runtime};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc, Weak,
};
use std::time::{Duration, Instant};

pub(crate) const DDL: &str =
    "CREATE TABLE IF NOT EXISTS __romedb_functions (name TEXT PRIMARY KEY, metadata TEXT NOT NULL)";
const MAX_BYTES: usize = 65_536;
const BRIDGE: &str = include_str!("../bundled/user-functions.js");
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Definition {
    version: u32,
    pub(crate) name: String,
    parameters: Vec<(String, String)>,
    returns: String,
    source: String,
    digest: String,
}
fn valid_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit()))
}
fn function_name(name: &str) -> Result<String> {
    let name = name.to_ascii_lowercase();
    let Some((namespace, function)) = name.split_once("::") else {
        return Err(Error::Validation(
            "function requires namespace::name".into(),
        ));
    };
    if !valid_identifier(namespace)
        || !valid_identifier(function)
        || namespace.starts_with("__")
        || matches!(
            namespace,
            "geo" | "string" | "record" | "relation" | "search" | "type" | "array" | "doc"
        )
    {
        return Err(Error::Validation(
            "invalid or reserved function namespace".into(),
        ));
    }
    Ok(name)
}
fn valid_type(kind: &str) -> bool {
    matches!(
        kind.strip_suffix('?').unwrap_or(kind),
        "any" | "string" | "integer" | "number" | "boolean" | "object" | "array"
    )
}
fn matches_type(kind: &str, value: &Value) -> bool {
    if kind == "any" || kind == "any?" {
        return true;
    }
    if matches!(value, Value::Null) {
        return kind.ends_with('?');
    }
    matches!(
        (kind.trim_end_matches('?'), value),
        ("string", Value::String(_))
            | ("integer", Value::Integer(_))
            | ("number", Value::Number(_))
            | ("boolean", Value::Boolean(_))
            | ("object", Value::Object(_))
            | ("array", Value::Array(_))
    )
}
impl Definition {
    fn digest(&self) -> Result<String> {
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                self.version,
                &self.name,
                &self.parameters,
                &self.returns,
                &self.source
            ))?)
        ))
    }
    fn validate(&self) -> Result<()> {
        if self.version != 1
            || function_name(&self.name)? != self.name
            || self.parameters.len() > 32
            || !valid_type(&self.returns)
        {
            return Err(Error::Validation(
                "invalid JavaScript function signature/version".into(),
            ));
        }
        if self.source.len() > MAX_BYTES {
            return Err(Error::Limit("JavaScript source exceeds 65536 bytes".into()));
        }
        let mut names = std::collections::BTreeSet::new();
        for (name, kind) in &self.parameters {
            if !valid_identifier(name) || !valid_type(kind) || !names.insert(name) {
                return Err(Error::Validation(
                    "invalid or duplicate function parameter".into(),
                ));
            }
        }
        if self.digest != self.digest()? {
            return Err(Error::Storage(
                "function definition checksum mismatch".into(),
            ));
        }
        Ok(())
    }
    fn javascript(&self) -> String {
        let args = self
            .parameters
            .iter()
            .map(|p| p.0.clone())
            .chain(std::iter::once(format!("\"use strict\";\n{}", self.source)))
            .map(|s| serde_json::to_string(&s).expect("string encoding"))
            .collect::<Vec<_>>()
            .join(",");
        format!("new Function({args})")
    }
}
impl Connection {
    /// Create a persisted, typed JavaScript scalar function. Replacement is atomic.
    pub fn create_function(
        &self,
        name: &str,
        parameters: Vec<(String, String)>,
        returns: &str,
        source: &str,
        replace: bool,
    ) -> Result<()> {
        let mut definition = Definition {
            version: 1,
            name: function_name(name)?,
            parameters,
            returns: returns.into(),
            source: source.into(),
            digest: String::new(),
        };
        definition.digest = definition.digest()?;
        definition.validate()?;
        run(&definition, None, Arc::downgrade(&self.engine))?;
        self.atomic(|| {
            let exists = !self.run("SELECT name FROM __romedb_functions WHERE name=?1", &[text(&definition.name)])?.is_empty();
            if exists && !replace { return Err(Error::AlreadyExists(definition.name.clone())); }
            self.run("INSERT INTO __romedb_functions(name,metadata) VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET metadata=excluded.metadata", &[text(&definition.name), text(&serde_json::to_string(&definition)?)])?;
            Ok(())
        })
    }
    pub fn drop_function(&self, name: &str, if_exists: bool) -> Result<()> {
        let name = function_name(name)?;
        self.atomic(|| {
            if self
                .run(
                    "SELECT name FROM __romedb_functions WHERE name=?1",
                    &[text(&name)],
                )?
                .is_empty()
                && !if_exists
            {
                return Err(Error::NotFound(name.clone()));
            }
            self.run(
                "DELETE FROM __romedb_functions WHERE name=?1",
                &[text(&name)],
            )?;
            Ok(())
        })
    }
    pub(crate) fn user_function(&self, name: &str) -> Result<Definition> {
        let name = function_name(name)?;
        let rows = self.run(
            "SELECT metadata FROM __romedb_functions WHERE name=?1",
            &[text(&name)],
        )?;
        let Some(row) = rows.first() else {
            return Err(Error::NotFound(format!("function {name}")));
        };
        let turso_core::Value::Text(metadata) = &row[0] else {
            return Err(Error::Storage("function metadata".into()));
        };
        let definition: Definition = serde_json::from_str(metadata.as_str())?;
        definition.validate()?;
        if definition.name != name {
            return Err(Error::Storage("function catalog key mismatch".into()));
        }
        Ok(definition)
    }
    pub(crate) fn validate_functions(&self) -> Result<()> {
        for row in self.run("SELECT name FROM __romedb_functions", &[])? {
            let turso_core::Value::Text(name) = &row[0] else {
                return Err(Error::Storage("function name".into()));
            };
            self.user_function(name.as_str())?;
        }
        Ok(())
    }
    pub(crate) fn function_info(&self, name: &str) -> Result<Value> {
        let d = self.user_function(name)?;
        Ok(Value::Object(
            [
                ("name".into(), Value::String(d.name)),
                ("language".into(), Value::String("javascript".into())),
                (
                    "runtime".into(),
                    Value::String("quickjs-ng-0.16.2-rquickjs-0.13.0".into()),
                ),
                ("version".into(), Value::Integer(i64::from(d.version))),
                ("digest".into(), Value::String(d.digest)),
                ("returns".into(), Value::String(d.returns)),
                ("source".into(), Value::String(d.source)),
                (
                    "parameters".into(),
                    Value::Array(
                        d.parameters
                            .into_iter()
                            .map(|(name, kind)| {
                                Value::Object(
                                    [
                                        ("name".into(), Value::String(name)),
                                        ("type".into(), Value::String(kind)),
                                    ]
                                    .into(),
                                )
                            })
                            .collect(),
                    ),
                ),
            ]
            .into(),
        ))
    }
    pub(crate) fn call_user_function(&self, name: &str, args: &[Value]) -> Result<Value> {
        call(
            &self.user_function(name)?,
            args,
            Arc::downgrade(&self.engine),
        )
    }
}
fn input(value: &Value) -> Result<serde_json::Value> {
    use serde_json::json;
    Ok(match value {
        Value::Integer(v) => json!({"type":"Integer","value":v.to_string()}),
        Value::Array(values) => json!({"type":"Array","value":values.iter().map(input).collect::<Result<Vec<_>>>()?}),
        Value::Object(values) => json!({"type":"Object","value":values.iter().map(|(k,v)| Ok((k.clone(),input(v)?))).collect::<Result<serde_json::Map<_,_>>>()?}),
        Value::Binary(_) | Value::Vector(_) | Value::Record(_) => return Err(Error::Validation("JavaScript accepts scalar, object and array values; record/binary/vector values require explicit conversion".into())),
        _ => serde_json::to_value(value)?,
    })
}
fn output(mut value: serde_json::Value) -> Result<Value> {
    match value.get("type").and_then(|v| v.as_str()) {
        Some("Integer") => {
            let n = value["value"]
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
                .ok_or_else(|| Error::Validation("JavaScript integer exceeds int64".into()))?;
            value["value"] = n.into();
        }
        Some("Number") if value["value"].as_str() == Some("-0") => return Ok(Value::Number(-0.0)),
        Some("Object") => {
            return Ok(Value::Object(
                value["value"]
                    .as_object()
                    .ok_or_else(|| Error::Validation("JavaScript object".into()))?
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), output(v.clone())?)))
                    .collect::<Result<_>>()?,
            ))
        }
        Some("Array") => {
            return Ok(Value::Array(
                value["value"]
                    .as_array()
                    .ok_or_else(|| Error::Validation("JavaScript array".into()))?
                    .iter()
                    .cloned()
                    .map(output)
                    .collect::<Result<_>>()?,
            ))
        }
        _ => {}
    }
    Ok(serde_json::from_value(value)?)
}
pub(crate) fn call(
    definition: &Definition,
    args: &[Value],
    engine: Weak<turso_core::Connection>,
) -> Result<Value> {
    definition.validate()?;
    if definition.parameters.len() != args.len()
        || definition
            .parameters
            .iter()
            .zip(args)
            .any(|((_, kind), v)| !matches_type(kind, v))
    {
        return Err(Error::Validation(format!(
            "{} argument types/arity",
            definition.name
        )));
    }
    Value::Array(args.to_vec()).encode_with_limit(Some(MAX_BYTES))?;
    let result = run(definition, Some(args), engine)?.expect("invocation result");
    result.validate()?;
    result.encode_with_limit(Some(MAX_BYTES))?;
    if !matches_type(&definition.returns, &result) {
        return Err(Error::Validation(format!(
            "{} return type {}",
            definition.name, definition.returns
        )));
    }
    Ok(result)
}
fn run(
    definition: &Definition,
    args: Option<&[Value]>,
    engine: Weak<turso_core::Connection>,
) -> Result<Option<Value>> {
    let runtime =
        Runtime::new().map_err(|e| Error::Storage(format!("QuickJS initialization: {e}")))?;
    runtime.set_memory_limit(8 * 1024 * 1024);
    runtime.set_max_stack_size(256 * 1024);
    let stopped = Arc::new(AtomicU8::new(0));
    let signal = stopped.clone();
    let deadline = Instant::now() + Duration::from_millis(100);
    let mut polls = 0;
    runtime.set_interrupt_handler(Some(Box::new(move || {
        if signal.load(Ordering::Relaxed) != 0 {
            return true;
        }
        polls += 1;
        let cancelled = engine
            .upgrade()
            .is_some_and(|e| e.is_interrupted() || e.should_interrupt_for_progress(0, 1000));
        let reason = if cancelled {
            2
        } else if polls > 1000 || Instant::now() >= deadline {
            1
        } else {
            0
        };
        if reason != 0 {
            signal.store(reason, Ordering::Relaxed);
        }
        reason != 0
    })));
    let context = Context::custom::<(
        intrinsic::Eval,
        intrinsic::RegExp,
        intrinsic::Json,
        intrinsic::Proxy,
        intrinsic::MapSet,
        intrinsic::TypedArrays,
        intrinsic::Promise,
    )>(&runtime)
    .map_err(|e| Error::Storage(format!("QuickJS context: {e}")))?;
    let encoded = args
        .map(|args| {
            args.iter()
                .map(input)
                .collect::<Result<Vec<_>>>()
                .and_then(|v| Ok(serde_json::to_string(&v)?))
        })
        .transpose()?;
    context.with(|ctx| {
        let result = (|| -> rquickjs::Result<Option<String>> {
            let bridge: Function = ctx.eval(BRIDGE)?;
            let function: Function = ctx.eval(definition.javascript())?;
            encoded
                .as_deref()
                .map(|input| bridge.call((function, input)))
                .transpose()
        })();
        match stopped.load(Ordering::Relaxed) {
            2 => return Err(Error::Engine(turso_core::LimboError::Interrupt)),
            1 => return Err(Error::Limit("JavaScript execution budget exceeded".into())),
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(Error::Limit("JavaScript execution budget exceeded".into()));
        }
        match result {
            Ok(Some(json)) => {
                if json.len() > MAX_BYTES {
                    return Err(Error::Limit("JavaScript result exceeds 65536 bytes".into()));
                }
                let mut decoder = serde_json::Deserializer::from_str(&json);
                decoder.disable_recursion_limit();
                Ok(Some(output(serde_json::Value::deserialize(&mut decoder)?)?))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                match stopped.load(Ordering::Relaxed) {
                    2 => return Err(Error::Engine(turso_core::LimboError::Interrupt)),
                    1 => return Err(Error::Limit("JavaScript execution budget exceeded".into())),
                    _ => {}
                }
                let exception = ctx.catch();
                let message = exception
                    .as_object()
                    .and_then(|o| o.get::<_, String>("message").ok())
                    .unwrap_or_else(|| error.to_string());
                match stopped.load(Ordering::Relaxed) {
                    2 => return Err(Error::Engine(turso_core::LimboError::Interrupt)),
                    1 => return Err(Error::Limit("JavaScript execution budget exceeded".into())),
                    _ => {}
                }
                if matches!(error, rquickjs::Error::Allocation)
                    || message.contains("out of memory")
                    || message.contains("stack overflow")
                    || message.contains("Maximum call stack")
                {
                    Err(Error::Limit(
                        "JavaScript memory or stack limit exceeded".into(),
                    ))
                } else {
                    Err(Error::Validation(format!(
                        "JavaScript failure: {}",
                        message.chars().take(1024).collect::<String>()
                    )))
                }
            }
        }
    })
}

pub(crate) fn has_calls(sql: &str) -> Result<bool> {
    let tokens = romeql_parser::tokenize(sql)?;
    Ok(tokens.windows(5).any(|t| {
        t[0].kind == romeql_parser::Kind::Word
            && t[1].text == ":"
            && t[2].text == ":"
            && t[3].kind == romeql_parser::Kind::Word
            && t[4].text == "("
            && !matches!(
                t[0].text.to_ascii_lowercase().as_str(),
                "geo" | "string" | "record" | "relation" | "search" | "type" | "array" | "doc"
            )
    }))
}
pub(crate) fn register(connection: &Connection) -> Result<()> {
    // This pinned API registers in the connection's own symbol table. The weak
    // pointer permits cancellation polling without retaining or re-entering it.
    unsafe {
        let context = Box::into_raw(Box::new(Arc::downgrade(&connection.engine))) as usize;
        let api = connection.engine._build_turso_ext();
        let code = (api.register_scalar_function)(
            api.ctx,
            c"__romedb_user_function".as_ptr(),
            -1,
            false,
            context,
            callback,
            Some(destroy),
            None,
        );
        connection.engine._free_extension_ctx(api);
        if code != turso_ext::ResultCode::OK {
            destroy(context);
            return Err(Error::Storage(
                "JavaScript callback registration failed".into(),
            ));
        }
    }
    Ok(())
}
unsafe extern "C" fn destroy(context: usize) {
    unsafe {
        drop(Box::from_raw(context as *mut Weak<turso_core::Connection>));
    }
}
unsafe extern "C" fn callback(
    context: usize,
    argc: i32,
    argv: *const turso_ext::Value,
    _: Option<turso_ext::ContextDestructor>,
    _: Option<turso_ext::ValueDestructor>,
) -> turso_ext::Value {
    use turso_ext::Value as ExtValue;
    let result = (|| -> Result<Value> {
        if !(1..=33).contains(&argc) || argv.is_null() {
            return Err(Error::Validation("JavaScript callback arity".into()));
        }
        let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
        let definition: Definition = serde_json::from_str(
            args[0]
                .to_text()
                .ok_or_else(|| Error::Validation("JavaScript definition".into()))?,
        )?;
        let values = args[1..]
            .iter()
            .map(crate::functions::decode_arg)
            .collect::<Result<Vec<_>>>()?;
        let engine = unsafe { &*(context as *const Weak<turso_core::Connection>) };
        call(&definition, &values, engine.clone())
    })()
    .and_then(|value| value.encode());
    match result {
        Ok(value) => ExtValue::from_blob(value),
        Err(error) => {
            let code = match error.code() {
                "FDB_LIMIT" => "limit",
                "FDB_CANCELLED" => "cancelled",
                _ => "validation",
            };
            ExtValue::error_with_message(format!("__romedb_udf_{code}:{error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn definition_checksum_and_native_cancellation_boundary() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.create_function("app::runaway", vec![], "any", "while(true){}", false)
            .unwrap();
        let d = c.user_function("app::runaway").unwrap();
        let mut changed = d.clone();
        changed.source = "return 1;".into();
        assert!(changed
            .validate()
            .unwrap_err()
            .to_string()
            .contains("checksum"));
        let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = ticks.clone();
        c.engine.set_progress_handler(
            1,
            Some(Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst) >= 10
            })),
        );
        let error = call(&d, &[], Arc::downgrade(&c.engine)).unwrap_err();
        c.engine.set_progress_handler(0, None);
        assert_eq!(error.code(), "FDB_CANCELLED");
        assert!(ticks.load(Ordering::SeqCst) >= 11);
    }
}
