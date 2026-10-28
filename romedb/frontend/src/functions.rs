//! Statically linked document accessors. No database callbacks or external I/O.
use crate::{index_scalar, Connection, Error, Result, Value};
use turso_ext::{scalar, ResultCode, Value as ExtValue, ValueType};

pub(crate) fn register(connection: &Connection) -> Result<()> {
    // The engine connection has not escaped Database::connect yet. Registration
    // is exclusive, and the temporary extension context is freed on every path.
    unsafe {
        let api = connection.engine._build_turso_ext();
        let result = [
            (
                c"__romedb_geo_distance",
                geo_distance as turso_ext::ScalarFunction,
                4,
            ),
            (
                c"__romedb_geo_bbox",
                geo_bbox as turso_ext::ScalarFunction,
                6,
            ),
            (
                c"__romedb_geo_locate",
                geo_locate as turso_ext::ScalarFunction,
                3,
            ),
            (
                c"__romedb_geo_shape_locate",
                geo_shape_locate as turso_ext::ScalarFunction,
                3,
            ),
            (
                c"__romedb_vector_field",
                vector_field as turso_ext::ScalarFunction,
                2,
            ),
            (
                c"__romedb_vector_concat",
                vector_concat as turso_ext::ScalarFunction,
                2,
            ),
            (
                c"__romedb_vector_input",
                vector_input as turso_ext::ScalarFunction,
                1,
            ),
            (
                c"__romedb_vector_value",
                vector_value as turso_ext::ScalarFunction,
                1,
            ),
            (c"__romedb_pack", pack as turso_ext::ScalarFunction, 1),
            (
                c"__romedb_array_key",
                array_key as turso_ext::ScalarFunction,
                1,
            ),
            (
                c"__romedb_unnest_json",
                unnest_json as turso_ext::ScalarFunction,
                1,
            ),
            (
                c"__romedb_pagination_value",
                pagination_value as turso_ext::ScalarFunction,
                1,
            ),
            (c"__romedb_compare", compare as turso_ext::ScalarFunction, 2),
            (
                c"__romedb_range_scalar",
                range_scalar as turso_ext::ScalarFunction,
                2,
            ),
            (c"__romedb_between", between as turso_ext::ScalarFunction, 3),
            (
                c"__romedb_nullable",
                nullable as turso_ext::ScalarFunction,
                1,
            ),
            (
                c"__romedb_count_value",
                count_value as turso_ext::ScalarFunction,
                1,
            ),
            (c"__romedb_unwrap", unwrap as turso_ext::ScalarFunction, 1),
            (
                c"__romedb_equality_key",
                equality_key as turso_ext::ScalarFunction,
                1,
            ),
            (
                c"__romedb_sql_scalar",
                sql_scalar as turso_ext::ScalarFunction,
                1,
            ),
            // Same implementations, distinct expression identities for HAVING
            // on the pinned engine's nonprojected function group keys.
            (
                c"__romedb_having_sql_scalar",
                sql_scalar as turso_ext::ScalarFunction,
                1,
            ),
            (
                c"__romedb_having_scalar",
                document_scalar as turso_ext::ScalarFunction,
                2,
            ),
            (c"__romedb_helper", helper as turso_ext::ScalarFunction, -1),
            (
                c"__romedb_scalar",
                document_scalar as turso_ext::ScalarFunction,
                2,
            ),
            (
                c"__romedb_nested_value",
                nested_value as turso_ext::ScalarFunction,
                2,
            ),
            (
                c"__romedb_value",
                document_value as turso_ext::ScalarFunction,
                2,
            ),
            (
                c"__romedb_sort",
                document_sort as turso_ext::ScalarFunction,
                2,
            ),
            (
                c"__romedb_record_value",
                record_value as turso_ext::ScalarFunction,
                2,
            ),
            (
                c"__romedb_sort_encoded",
                sort_encoded as turso_ext::ScalarFunction,
                1,
            ),
            (
                c"romedb_version",
                romedb_version as turso_ext::ScalarFunction,
                0,
            ),
            (
                c"__romedb_text_record",
                text_record as turso_ext::ScalarFunction,
                1,
            ),
        ]
        .into_iter()
        .try_for_each(|(name, callback, argc)| {
            let code = (api.register_scalar_function)(
                api.ctx,
                name.as_ptr(),
                argc,
                name != c"__romedb_pagination_value"
                    && name != c"__romedb_unnest_json"
                    && name != c"romedb_version",
                0,
                callback,
                None,
                None,
            );
            if code == ResultCode::OK {
                Ok(())
            } else {
                Err(Error::Storage(format!(
                    "accessor registration failed: {code:?}"
                )))
            }
        });
        connection.engine._free_extension_ctx(api);
        result.and_then(|_| crate::udf::register(connection))
    }
}
#[scalar(name = "romedb_version")]
fn romedb_version(_args: &[ExtValue]) -> ExtValue {
    ExtValue::from_text(crate::VERSION.into())
}

#[scalar(name = "__romedb_text_record")]
fn text_record(args: &[ExtValue]) -> ExtValue {
    match args
        .first()
        .and_then(ExtValue::to_text)
        .and_then(crate::value::record_from_text)
    {
        Some(record) => scalar_result(&Value::Record(record))
            .unwrap_or_else(|e| ExtValue::error_with_message(e.to_string())),
        None => ExtValue::null(),
    }
}

fn get(args: &[ExtValue], mode: u8) -> Result<ExtValue> {
    if args.len() != 2 {
        return Err(Error::Validation("document accessor arity".into()));
    }
    // Outer joins produce a NULL document for the unmatched side.
    if args[0].value_type() == ValueType::Null {
        return if mode == 4 {
            vector_input_value(Value::Null)
        } else {
            Ok(ExtValue::null())
        };
    }
    let bytes = args[0]
        .blob_ref()
        .ok_or_else(|| Error::Storage("expected stored document".into()))?;
    let path = field_path(
        args[1]
            .to_text()
            .ok_or_else(|| Error::Storage("expected field path".into()))?,
    )?;
    if (mode == 1 || mode == 3) && !path.is_empty() {
        return Ok(match crate::value::encoded_field(bytes, &path)? {
            crate::value::EncodedField::Found(encoded) => ExtValue::from_blob(encoded),
            crate::value::EncodedField::Missing => ExtValue::from_blob(Value::Null.encode()?),
            crate::value::EncodedField::RootNotObject if mode == 3 => {
                ExtValue::from_blob(Value::Null.encode()?)
            }
            crate::value::EncodedField::RootNotObject => {
                return Err(Error::Storage("expected object".into()))
            }
        });
    }
    let field = if path.is_empty() {
        match Value::decode(bytes)? {
            value @ Value::Object(_) => crate::value::Field::Found(value),
            _ => crate::value::Field::RootNotObject,
        }
    } else {
        crate::value::decode_field(bytes, &path)?
    };
    let value = match field {
        crate::value::Field::Found(value) => value,
        crate::value::Field::Missing => Value::Null,
        // Derived columns may contain any logical value. A non-object parent
        // has no nested object fields; stored document roots remain strict.
        crate::value::Field::RootNotObject if mode == 3 => Value::Null,
        crate::value::Field::RootNotObject => return Err(Error::Storage("expected object".into())),
    };
    if mode == 4 {
        return vector_input_value(value);
    }
    if mode == 1 || mode == 3 {
        return Ok(ExtValue::from_blob(value.encode()?));
    }
    if mode == 2 {
        return ordered(&value);
    }
    scalar_result(&value)
}
fn ordered(value: &Value) -> Result<ExtValue> {
    if let Value::Binary(bytes) = value {
        let mut key = Vec::with_capacity(bytes.len() + 1);
        key.push(0);
        key.extend(bytes);
        return Ok(ExtValue::from_blob(key));
    }
    if let Value::Record(record) = &value {
        let mut key = vec![1];
        key.extend(record.table.to_ascii_lowercase().into_bytes());
        key.push(0);
        match &record.key {
            crate::Key::Integer(i) => {
                key.push(0);
                key.extend(((*i as u64) ^ (1u64 << 63)).to_be_bytes());
            }
            crate::Key::String(s) => {
                key.push(1);
                key.extend(s.as_bytes());
            }
        }
        return Ok(ExtValue::from_blob(key));
    }
    scalar_result(value)
}
fn scalar_result(value: &Value) -> Result<ExtValue> {
    let value = index_scalar(value)?;
    use turso_core::{Numeric, Value as EngineValue};
    Ok(match value {
        EngineValue::Null => ExtValue::null(),
        EngineValue::Numeric(Numeric::Integer(i)) => ExtValue::from_integer(i),
        EngineValue::Numeric(Numeric::Float(n)) => ExtValue::from_float(n.into()),
        EngineValue::Text(t) => ExtValue::from_text(t.as_str().into()),
        EngineValue::Blob(b) => ExtValue::from_blob(b),
    })
}
/// Distinct field paths kept parsed; a statement usually reads a few fields per row.
const FIELD_PATHS: usize = 8;

/// Path text and its parsed steps.
type ParsedPath = (String, std::rc::Rc<Vec<String>>);

thread_local! {
    /// Next slot to replace, and the parsed paths.
    static FIELD_PATH: std::cell::RefCell<(usize, Vec<ParsedPath>)> =
        const { std::cell::RefCell::new((0, Vec::new())) };
}

/// Parsed field path, reused while rows read the same few paths.
fn field_path(text: &str) -> Result<std::rc::Rc<Vec<String>>> {
    if let Some(path) = FIELD_PATH.with(|cached| {
        cached
            .borrow()
            .1
            .iter()
            .find(|(cached, _)| cached == text)
            .map(|(_, path)| path.clone())
    }) {
        return Ok(path);
    }
    let path = std::rc::Rc::new(serde_json::from_str::<Vec<String>>(text)?);
    FIELD_PATH.with(|cached| {
        let (next, entries) = &mut *cached.borrow_mut();
        let entry = (text.to_owned(), path.clone());
        if entries.len() < FIELD_PATHS {
            entries.push(entry);
        } else {
            entries[*next] = entry;
            *next = (*next + 1) % FIELD_PATHS;
        }
    });
    Ok(path)
}

#[scalar(name = "__romedb_scalar")]
fn document_scalar(args: &[ExtValue]) -> ExtValue {
    get(args, 0).unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}
#[scalar(name = "__romedb_nested_value")]
fn nested_value(args: &[ExtValue]) -> ExtValue {
    get(args, 3).unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}
#[scalar(name = "__romedb_value")]
fn document_value(args: &[ExtValue]) -> ExtValue {
    get(args, 1).unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

#[scalar(name = "__romedb_sort")]
fn document_sort(args: &[ExtValue]) -> ExtValue {
    get(args, 2).unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

#[scalar(name = "__romedb_record_value")]
fn record_value(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        if args.len() != 2 {
            return Err(Error::Validation(
                "record constructor expects target and key".into(),
            ));
        }
        let target = args[0]
            .to_text()
            .ok_or_else(|| Error::Validation("record target must be text".into()))?;
        let table = crate::canonical(target)?;
        let key = match args[1].value_type() {
            ValueType::Integer => crate::Key::Integer(args[1].to_integer().expect("integer value")),
            ValueType::Text => crate::Key::String(args[1].to_text().expect("text value").into()),
            _ => return Err(Error::Validation("record key must be text or int64".into())),
        };
        Ok(ExtValue::from_blob(
            Value::Record(crate::Record { table, key }).encode()?,
        ))
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

#[scalar(name = "__romedb_sort_encoded")]
fn sort_encoded(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        if args.len() != 1 {
            return Err(Error::Validation("sort arity".into()));
        }
        if args[0].value_type() == ValueType::Null {
            return Ok(ExtValue::null());
        }
        let bytes = args[0]
            .to_blob()
            .ok_or_else(|| Error::Validation("expected typed value".into()))?;
        ordered(&Value::decode(&bytes)?)
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

pub(crate) fn decode_arg(arg: &ExtValue) -> Result<Value> {
    if arg.value_type() == ValueType::Null {
        return Ok(Value::Null);
    }
    let bytes = arg
        .blob_ref()
        .ok_or_else(|| Error::Storage("expected encoded helper argument".into()))?;
    match crate::value::decode_simple(bytes) {
        Some(value) => Ok(value),
        None => Value::decode(bytes),
    }
}
#[scalar(name = "__romedb_pack")]
fn pack(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [v] = args else {
            return Err(Error::Validation("pack arity".into()));
        };
        let value = match v.value_type() {
            ValueType::Null => Value::Null,
            ValueType::Integer => Value::Integer(v.to_integer().expect("integer")),
            ValueType::Float => Value::Number(v.to_float().expect("float")),
            ValueType::Text => Value::String(v.to_text().expect("text").into()),
            ValueType::Blob => Value::Binary(v.to_blob().expect("blob")),
            _ => return Err(Error::Validation("unsupported scalar value".into())),
        };
        Ok(ExtValue::from_blob(value.encode()?))
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}
pub(crate) fn compare_values(a: &Value, b: &Value) -> Result<Option<std::cmp::Ordering>> {
    if matches!(a, Value::Null) || matches!(b, Value::Null) {
        return Ok(None);
    }
    let order = match (a, b) {
        (Value::Binary(a), Value::Binary(b)) => a.cmp(b),
        (Value::Record(a), Value::Record(b)) => a
            .table
            .to_ascii_lowercase()
            .cmp(&b.table.to_ascii_lowercase())
            .then_with(|| match (&a.key, &b.key) {
                (crate::Key::Integer(a), crate::Key::Integer(b)) => a.cmp(b),
                (crate::Key::String(a), crate::Key::String(b)) => a.cmp(b),
                (crate::Key::Integer(_), crate::Key::String(_)) => std::cmp::Ordering::Less,
                (crate::Key::String(_), crate::Key::Integer(_)) => std::cmp::Ordering::Greater,
            }),
        (Value::Record(_), _) | (_, Value::Record(_)) => {
            return Err(Error::Validation(
                "mixed record/scalar ordering is unsupported".into(),
            ))
        }
        _ => index_scalar(a)?.cmp(&index_scalar(b)?),
    };
    Ok(Some(order))
}

#[scalar(name = "__romedb_between")]
fn between(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [value, start, end] = args else {
            return Err(Error::Validation("BETWEEN arity".into()));
        };
        let (value, start, end) = (decode_arg(value)?, decode_arg(start)?, decode_arg(end)?);
        let lower = compare_values(&value, &start)?.map(|order| !order.is_lt());
        let upper = compare_values(&value, &end)?.map(|order| !order.is_gt());
        // SQL three-valued AND: false dominates null, otherwise unknown stays
        // unknown. Each argument is evaluated exactly once by the engine.
        Ok(match (lower, upper) {
            (Some(false), _) | (_, Some(false)) => ExtValue::from_integer(0),
            (Some(true), Some(true)) => ExtValue::from_integer(1),
            _ => ExtValue::null(),
        })
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

#[scalar(name = "__romedb_compare")]
fn compare(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [a, b] = args else {
            return Err(Error::Validation("comparison arity".into()));
        };
        let (a, b) = (decode_arg(a)?, decode_arg(b)?);
        let Some(order) = compare_values(&a, &b)? else {
            return Ok(ExtValue::null());
        };
        Ok(ExtValue::from_integer(match order {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }))
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

// Native SQL functions and casts consume binary payloads, while predicates
// retain tagged binary keys to prevent collisions with record identities.
// Keep the mutable pagination counter out of the engine's constant registers.
// Preserve raw SQL values so MustBeInt retains responsibility for conversion.
#[scalar(name = "__romedb_pagination_value")]
fn pagination_value(args: &[ExtValue]) -> ExtValue {
    let [value] = args else {
        return ExtValue::error_with_message("pagination arity".into());
    };
    match value.value_type() {
        ValueType::Null => ExtValue::null(),
        ValueType::Integer => ExtValue::from_integer(value.to_integer().unwrap()),
        ValueType::Float => ExtValue::from_float(value.to_float().unwrap()),
        ValueType::Text => ExtValue::from_text(value.to_text().unwrap().to_owned()),
        ValueType::Blob => ExtValue::from_blob(value.to_blob().unwrap()),
        ValueType::Error => ExtValue::error_with_message("pagination argument error".into()),
    }
}

#[scalar(name = "__romedb_sql_scalar")]
fn sql_scalar(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [value] = args else {
            return Err(Error::Validation("SQL scalar arity".into()));
        };
        match decode_arg(value)? {
            Value::Binary(bytes) => Ok(ExtValue::from_blob(bytes)),
            value => scalar_result(&value),
        }
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

// A native column retains its engine affinity/collation in the comparison.
// Convert only the logical operand, without allowing encoded record bytes to
// acquire an accidental order relative to ordinary SQL values.
#[scalar(name = "__romedb_range_scalar")]
fn range_scalar(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [value, native] = args else {
            return Err(Error::Validation("range scalar arity".into()));
        };
        let value = decode_arg(value)?;
        if native.value_type() == ValueType::Null || matches!(value, Value::Null) {
            return Ok(ExtValue::null());
        }
        match value {
            Value::Record(_) => Err(Error::Validation(
                "mixed record/scalar ordering is unsupported".into(),
            )),
            Value::Binary(bytes) => Ok(ExtValue::from_blob(bytes)),
            value => scalar_result(&value),
        }
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

#[scalar(name = "__romedb_unwrap")]
fn unwrap(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [v] = args else {
            return Err(Error::Validation("unwrap arity".into()));
        };
        scalar_result(&decode_arg(v)?)
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}
#[scalar(name = "__romedb_equality_key")]
fn equality_key(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [value] = args else {
            return Err(Error::Validation("equality key arity".into()));
        };
        let value = decode_arg(value)?;
        if matches!(value, Value::Array(_) | Value::Object(_) | Value::Vector(_)) {
            value.encode_with_limit(Some(64 * 1024 * 1024))?;
            return Ok(ExtValue::from_blob(crate::collections::identity(&value)?));
        }
        scalar_result(&value)
    })();
    result.unwrap_or_else(|error| {
        let prefix = match &error {
            Error::Limit(_) => "__romedb_udf_limit:",
            Error::Validation(_) => "__romedb_udf_validation:",
            _ => "",
        };
        ExtValue::error_with_message(format!("{prefix}{error}"))
    })
}
#[scalar(name = "__romedb_array_key")]
fn array_key(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [value] = args else {
            return Err(Error::Validation("array key arity".into()));
        };
        let value = decode_arg(value)?;
        value.encode_with_limit(Some(1024 * 1024))?;
        Ok(ExtValue::from_blob(crate::collections::identity(&value)?))
    })();
    result.unwrap_or_else(|error| {
        let prefix = match &error {
            Error::Limit(_) => "__romedb_udf_limit:",
            Error::Validation(_) => "__romedb_udf_validation:",
            _ => "",
        };
        ExtValue::error_with_message(format!("{prefix}{error}"))
    })
}
#[scalar(name = "__romedb_helper")]
fn helper(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let (name, args) = args
            .split_first()
            .ok_or_else(|| Error::Validation("helper arity".into()))?;
        let name = name
            .to_text()
            .ok_or_else(|| Error::Validation("helper name".into()))?;
        let args = args.iter().map(decode_arg).collect::<Result<Vec<_>>>()?;
        let value = match (name, args.as_slice()) {
            ("doc_before" | "doc_after" | "doc_diff", _) => {
                return Err(Error::Validation(
                    "mutation snapshot helpers require collection RETURNING".into(),
                ))
            }
            ("mutation_diff", _) => crate::mutation_result::call_diff(&args)?,
            ("search_analyze", _) => crate::analyzer::call(&args)?,
            (name, _) if crate::spatial::public_name(name).is_some() => {
                crate::spatial::call(name, &args)?
            }
            ("string_slugify", _) => crate::bundled::call("slugify", &args)?,
            ("string_normalize", _) => crate::bundled::call("normalize", &args)?,
            ("array_new", _) => Value::Array(args),
            (
                "array_len"
                | "array_contains"
                | "array_distinct"
                | "array_flatten"
                | "object_keys"
                | "object_values"
                | "object_entries"
                | "object_from_entries",
                _,
            ) => crate::collections::call(name, &args)?,
            ("array_path_contains", [value, Value::String(path), needle]) => {
                crate::collections::path_contains(value, path, needle)?
            }
            ("doc_omit", [Value::Object(document), Value::String(paths)]) => {
                crate::omit::apply(document, paths)?
            }
            ("array_append", [Value::Array(array), element]) => {
                let mut array = array.clone();
                array.push(element.clone());
                Value::Array(array)
            }
            ("record_id", [Value::Record(record)]) => match &record.key {
                crate::Key::Integer(i) => Value::Integer(*i),
                crate::Key::String(s) => Value::String(s.clone()),
            },
            ("record_table", [Value::Record(record)]) => {
                Value::String(record.table.to_ascii_lowercase())
            }
            ("doc_get" | "doc_has", [value, Value::String(path)]) => {
                let found = crate::path::get(value, path)?;
                if name == "doc_has" {
                    Value::Boolean(found.is_some())
                } else {
                    found.cloned().unwrap_or(Value::Null)
                }
            }
            _ => {
                return Err(Error::Validation(
                    "invalid document helper arguments".into(),
                ))
            }
        };
        Ok(ExtValue::from_blob(value.encode()?))
    })();
    result.unwrap_or_else(|e| {
        let helper = args.first().and_then(ExtValue::to_text);
        let prefix = if matches!(
            helper,
            Some(
                "search_analyze"
                    | "mutation_diff"
                    | "doc_before"
                    | "doc_after"
                    | "doc_diff"
                    | "array_contains"
                    | "array_path_contains"
            )
        ) || helper.is_some_and(crate::spatial::categorized_errors)
        {
            match e {
                Error::Limit(_) => "__romedb_udf_limit:",
                Error::Validation(_) => "__romedb_udf_validation:",
                _ => "",
            }
        } else {
            ""
        };
        ExtValue::error_with_message(format!("{prefix}{e}"))
    })
}

#[scalar(name = "__romedb_count_value")]
fn count_value(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [value] = args else {
            return Err(Error::Validation("count value arity".into()));
        };
        // Validate the full value, but COUNT only needs null presence. Avoid
        // serializing a potentially large composite back into a result blob.
        if matches!(decode_arg(value)?, Value::Null) {
            Ok(ExtValue::null())
        } else {
            Ok(ExtValue::from_integer(1))
        }
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

#[scalar(name = "__romedb_nullable")]
fn nullable(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [v] = args else {
            return Err(Error::Validation("nullable arity".into()));
        };
        let value = decode_arg(v)?;
        if matches!(value, Value::Null) {
            Ok(ExtValue::null())
        } else {
            Ok(ExtValue::from_blob(value.encode()?))
        }
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

#[scalar(name = "__romedb_vector_field")]
fn vector_field(args: &[ExtValue]) -> ExtValue {
    get(args, 4).unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

fn vector_input_value(value: Value) -> Result<ExtValue> {
    match value {
        Value::Vector(bytes) | Value::Binary(bytes) => {
            crate::vectors::dimensions(&bytes)?;
            Ok(ExtValue::from_blob(bytes))
        }
        Value::String(text) => Ok(ExtValue::from_text(text)),
        _ => Err(Error::Validation(
            "vector input requires vector, blob or engine vector text".into(),
        )),
    }
}

#[scalar(name = "__romedb_vector_input")]
fn vector_input(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [arg] = args else {
            return Err(Error::Validation("vector input arity".into()));
        };
        vector_input_value(decode_arg(arg)?)
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}
#[scalar(name = "__romedb_vector_value")]
fn vector_value(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [arg] = args else {
            return Err(Error::Validation("vector constructor arity".into()));
        };
        let bytes = arg
            .to_blob()
            .ok_or_else(|| Error::Storage("vector constructor returned non-blob".into()))?;
        Ok(ExtValue::from_blob(Value::Vector(bytes).encode()?))
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

#[scalar(name = "__romedb_vector_concat")]
fn vector_concat(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [a, b] = args else {
            return Err(Error::Validation("vector concat arity".into()));
        };
        let input = |v: &ExtValue| -> Result<turso_core::Value> {
            if let Some(bytes) = v.to_blob() {
                crate::vectors::dimensions(&bytes)?;
                return Ok(turso_core::Value::Blob(bytes));
            }
            if let Some(text) = v.to_text() {
                return Ok(crate::text(text));
            }
            Err(Error::Validation("invalid vector concat input".into()))
        };
        let turso_core::Value::Blob(bytes) = crate::vectors::concat(input(a)?, input(b)?)? else {
            unreachable!("vector result");
        };
        Ok(ExtValue::from_blob(bytes))
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

#[scalar(name = "__romedb_geo_locate")]
fn geo_locate(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [token, longitude, latitude] = args else {
            return Err(Error::Storage("region test arity".into()));
        };
        let coordinate = |value: &ExtValue| match value.value_type() {
            ValueType::Integer => Ok(value.to_integer().expect("integer") as f64),
            ValueType::Float => Ok(value.to_float().expect("float")),
            _ => Err(Error::Storage("invalid spatial index coordinate".into())),
        };
        let token = token
            .to_integer()
            .ok_or_else(|| Error::Storage("invalid spatial region token".into()))?;
        Ok(ExtValue::from_integer(crate::spatial::locate_registered(
            token,
            (coordinate(longitude)?, coordinate(latitude)?),
        )?))
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

#[scalar(name = "__romedb_geo_shape_locate")]
fn geo_shape_locate(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [shape, longitude, latitude] = args else {
            return Err(Error::Storage("shape test arity".into()));
        };
        let coordinate = |value: &ExtValue| match value.value_type() {
            ValueType::Integer => Ok(value.to_integer().expect("integer") as f64),
            ValueType::Float => Ok(value.to_float().expect("float")),
            _ => Err(Error::Storage("invalid point coordinate".into())),
        };
        let bytes = shape
            .blob_ref()
            .ok_or_else(|| Error::Storage("invalid stored region shape".into()))?;
        Ok(ExtValue::from_integer(crate::spatial::locate_shape(
            bytes,
            (coordinate(longitude)?, coordinate(latitude)?),
        )?))
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

#[scalar(name = "__romedb_geo_bbox")]
fn geo_bbox(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let values = args
            .iter()
            .map(|value| match value.value_type() {
                ValueType::Integer => Ok(value.to_integer().expect("integer") as f64),
                ValueType::Float => Ok(value.to_float().expect("float")),
                _ => Err(Error::Storage("invalid spatial index coordinate".into())),
            })
            .collect::<Result<Vec<_>>>()?;
        let [longitude, latitude, west, south, east, north] = values.as_slice() else {
            return Err(Error::Storage("viewport test arity".into()));
        };
        Ok(ExtValue::from_integer(i64::from(crate::spatial::in_bbox(
            (*longitude, *latitude),
            *west,
            *south,
            *east,
            *north,
        ))))
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

#[scalar(name = "__romedb_geo_distance")]
fn geo_distance(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let args = args
            .iter()
            .map(|value| match value.value_type() {
                ValueType::Integer => Ok(Value::Integer(value.to_integer().expect("integer"))),
                ValueType::Float => Ok(Value::Number(value.to_float().expect("float"))),
                _ => Err(Error::Storage("invalid spatial index coordinate".into())),
            })
            .collect::<Result<Vec<_>>>()?;
        scalar_result(&crate::spatial::distance_coordinates(&args)?)
    })();
    result.unwrap_or_else(|e| ExtValue::error_with_message(e.to_string()))
}

#[cfg(test)]
mod between_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CALLS: AtomicUsize = AtomicUsize::new(0);

    #[scalar(name = "between_tick")]
    fn tick(_: &[ExtValue]) -> ExtValue {
        CALLS.fetch_add(1, Ordering::SeqCst);
        ExtValue::from_integer(7)
    }

    #[test]
    fn typed_between_evaluates_volatile_left_operand_once() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        // Test-only scalar counts actual engine evaluation, without performing
        // database work inside its callback.
        unsafe {
            let api = c.engine._build_turso_ext();
            let code = (api.register_scalar_function)(
                api.ctx,
                c"between_tick".as_ptr(),
                0,
                false,
                0,
                tick,
                None,
                None,
            );
            c.engine._free_extension_ctx(api);
            assert_eq!(code, ResultCode::OK);
        }
        for sql in [
            "CREATE TABLE collated_inputs",
            "CREATE TABLE collated_baseline(lo,hi)",
            "INSERT INTO collated_inputs(lo,hi) VALUES(1,10),(2,10)",
            "INSERT INTO collated_baseline VALUES(1,10),(2,10)",
        ] {
            c.execute(sql, &crate::Parameters::new()).unwrap();
        }
        for negate in ["", "NOT "] {
            for collation in ["BINARY", "NOCASE", "RTRIM"] {
                for tail in ["", " LIMIT 0"] {
                    for (lhs, lower, upper, per_row) in [
                        (
                            "coalesce(NULL,between_tick()+d.lo)".to_owned(),
                            format!("d.lo COLLATE {collation}"),
                            "d.hi".to_owned(),
                            1,
                        ),
                        (
                            format!("d.lo COLLATE {collation}"),
                            "coalesce(NULL,between_tick()+d.lo)".to_owned(),
                            "d.hi".to_owned(),
                            1,
                        ),
                        (
                            format!("d.lo COLLATE {collation}"),
                            "d.lo".to_owned(),
                            "coalesce(NULL,between_tick()+d.hi)".to_owned(),
                            1,
                        ),
                        (
                            "coalesce(NULL,between_tick()+d.lo)".to_owned(),
                            "coalesce(NULL,between_tick()+d.lo-1)".to_owned(),
                            format!("d.hi COLLATE {collation}"),
                            2,
                        ),
                        (
                            "coalesce(NULL,between_tick()+d.lo)".to_owned(),
                            "coalesce(NULL,between_tick()+d.lo)".to_owned(),
                            format!("d.hi COLLATE {collation}"),
                            1,
                        ),
                    ] {
                        let predicate = format!("{lhs} {negate}BETWEEN {lower} AND {upper}");
                        let native =
                            format!("SELECT {predicate} AS value FROM collated_baseline d{tail}");
                        let sql = format!("SELECT (SELECT {predicate} UNION ALL SELECT NULL LIMIT 1) AS value FROM collated_inputs d{tail}");
                        CALLS.store(0, Ordering::SeqCst);
                        let expected = c.execute(&native, &crate::Parameters::new()).unwrap().rows;
                        let calls = CALLS.load(Ordering::SeqCst);
                        assert_eq!(
                            calls,
                            if tail.is_empty() { 2 * per_row } else { 0 },
                            "{native}"
                        );
                        CALLS.store(0, Ordering::SeqCst);
                        assert_eq!(
                            c.execute(&sql, &crate::Parameters::new()).unwrap().rows,
                            expected,
                            "{sql}"
                        );
                        assert_eq!(CALLS.load(Ordering::SeqCst), calls, "{sql}");
                        CALLS.store(0, Ordering::SeqCst);
                        assert_eq!(
                            c.profile_select(&sql, &crate::Parameters::new())
                                .unwrap()
                                .result
                                .rows,
                            expected,
                            "{sql}"
                        );
                        assert_eq!(CALLS.load(Ordering::SeqCst), calls, "profile: {sql}");
                        CALLS.store(0, Ordering::SeqCst);
                        c.execute(
                            &format!("EXPLAIN QUERY PLAN {sql}"),
                            &crate::Parameters::new(),
                        )
                        .unwrap();
                        assert_eq!(CALLS.load(Ordering::SeqCst), 0, "plan: {sql}");
                    }
                }
            }
        }
        for negate in ["", "NOT "] {
            CALLS.store(0, Ordering::SeqCst);
            let rows=c.execute(&format!("SELECT type::record('docs',between_tick()) {negate}BETWEEN docs:2 AND docs:10 AS value"), &crate::Parameters::new()).unwrap().rows;
            assert_eq!(
                rows,
                vec![vec![Value::Integer(i64::from(negate.is_empty()))]]
            );
            assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        }
        CALLS.store(0, Ordering::SeqCst);
        let rows = c.execute("WITH v(id) AS MATERIALIZED (VALUES (type::record('docs',between_tick())),(type::record('docs',between_tick()))) SELECT record::id(a.id),record::id(b.id) FROM v a JOIN v b ON 1", &crate::Parameters::new()).unwrap().rows;
        assert_eq!(rows, vec![vec![Value::Integer(7), Value::Integer(7)]; 4]);
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        CALLS.store(0, Ordering::SeqCst);
        let rows = c.execute("WITH v(id) AS MATERIALIZED (VALUES (type::record('docs',between_tick())),(type::record('docs',between_tick()))) SELECT record::id(id) FROM v UNION ALL SELECT record::id(id) FROM v", &crate::Parameters::new()).unwrap().rows;
        assert_eq!(rows, vec![vec![Value::Integer(7)]; 4]);
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        c.execute("CREATE TABLE scalar_inputs", &crate::Parameters::new())
            .unwrap();
        c.execute("INSERT INTO scalar_inputs {n:1}", &crate::Parameters::new())
            .unwrap();
        c.execute("INSERT INTO scalar_inputs {n:2}", &crate::Parameters::new())
            .unwrap();
        CALLS.store(0, Ordering::SeqCst);
        let rows = c
            .execute(
                "SELECT (SELECT between_tick() FROM scalar_inputs) AS v FROM scalar_inputs",
                &crate::Parameters::new(),
            )
            .unwrap()
            .rows;
        assert_eq!(rows, vec![vec![Value::Integer(7)]; 2]);
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        CALLS.store(0, Ordering::SeqCst);
        let rows = c.execute("SELECT CASE WHEN 0 THEN (SELECT between_tick() FROM scalar_inputs) ELSE 3 END AS v", &crate::Parameters::new()).unwrap().rows;
        assert_eq!(rows, vec![vec![Value::Integer(3)]]);
        let logical_calls = CALLS.load(Ordering::SeqCst);
        CALLS.store(0, Ordering::SeqCst);
        let rows = c
            .execute(
                "SELECT CASE WHEN 0 THEN (SELECT between_tick()) ELSE 3 END AS v",
                &crate::Parameters::new(),
            )
            .unwrap()
            .rows;
        assert_eq!(rows, vec![vec![Value::Integer(3)]]);
        assert_eq!(CALLS.load(Ordering::SeqCst), logical_calls);
        assert_eq!(logical_calls, 1);
        for (source, expected) in [
            ("SELECT n FROM scalar_inputs", 2),
            ("SELECT DISTINCT n FROM scalar_inputs", 2),
            (
                "SELECT n FROM scalar_inputs UNION ALL SELECT n FROM scalar_inputs",
                4,
            ),
            (
                "SELECT n FROM scalar_inputs UNION SELECT n FROM scalar_inputs",
                2,
            ),
        ] {
            CALLS.store(0, Ordering::SeqCst);
            let rows = c.execute(
                &format!("{source} LIMIT (SELECT between_tick() FROM scalar_inputs) OFFSET (SELECT between_tick()-7 FROM scalar_inputs)"),
                &crate::Parameters::new(),
            ).unwrap().rows;
            assert_eq!(rows.len(), expected, "{source}");
            assert_eq!(CALLS.load(Ordering::SeqCst), 2, "{source}");
        }
        c.execute(
            "CREATE TABLE exists_native(n INTEGER)",
            &crate::Parameters::new(),
        )
        .unwrap();
        c.execute(
            "INSERT INTO exists_native VALUES (1),(2)",
            &crate::Parameters::new(),
        )
        .unwrap();
        for comparison in [
            "n=(SELECT between_tick() FROM exists_native)",
            "(SELECT between_tick() FROM exists_native)>n",
            "n=((SELECT between_tick() FROM exists_native) COLLATE NOCASE)",
        ] {
            CALLS.store(0, Ordering::SeqCst);
            c.execute(
                &format!("SELECT {comparison} AS v FROM scalar_inputs"),
                &crate::Parameters::new(),
            )
            .unwrap();
            assert_eq!(CALLS.load(Ordering::SeqCst), 1, "{comparison}");
        }
        CALLS.store(0, Ordering::SeqCst);
        let rows = c.execute("SELECT EXISTS (SELECT between_tick() FROM exists_native) AS present FROM scalar_inputs", &crate::Parameters::new()).unwrap().rows;
        assert_eq!(rows, vec![vec![Value::Integer(1)]; 2]);
        assert_eq!(CALLS.load(Ordering::SeqCst), 0);
        for inner in [
            "between_tick() FROM scalar_inputs",
            "n FROM scalar_inputs WHERE between_tick()=7",
        ] {
            CALLS.store(0, Ordering::SeqCst);
            let rows = c
                .execute(
                    &format!("SELECT EXISTS (SELECT {inner}) AS v"),
                    &crate::Parameters::new(),
                )
                .unwrap()
                .rows;
            assert_eq!(rows, vec![vec![Value::Integer(1)]]);
            let logical_calls = CALLS.load(Ordering::SeqCst);
            CALLS.store(0, Ordering::SeqCst);
            let native_inner = if inner.starts_with("between_tick") {
                "between_tick()"
            } else {
                "1 WHERE between_tick()=7"
            };
            let native = c
                .execute(
                    &format!("SELECT EXISTS (SELECT {native_inner}) AS v"),
                    &crate::Parameters::new(),
                )
                .unwrap()
                .rows;
            assert_eq!(rows, native);
            assert_eq!(CALLS.load(Ordering::SeqCst), logical_calls);
            assert_eq!(
                logical_calls,
                usize::from(!inner.starts_with("between_tick"))
            );
        }
        c.execute(
            "CREATE TABLE membership_native(n INTEGER)",
            &crate::Parameters::new(),
        )
        .unwrap();
        c.execute(
            "INSERT INTO membership_native VALUES (7),(8)",
            &crate::Parameters::new(),
        )
        .unwrap();
        CALLS.store(0, Ordering::SeqCst);
        let rows = c.execute("SELECT n IN (SELECT between_tick() FROM scalar_inputs) AS v FROM membership_native ORDER BY n", &crate::Parameters::new()).unwrap().rows;
        assert_eq!(rows, vec![vec![Value::Integer(1)], vec![Value::Integer(0)]]);
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        for outer in ["membership_native", "scalar_inputs"] {
            for not in ["", "NOT "] {
                CALLS.store(0, Ordering::SeqCst);
                let rows = c.execute(&format!("SELECT n {not}IN (SELECT between_tick() FROM membership_native) FROM {outer}"), &crate::Parameters::new()).unwrap().rows;
                assert_eq!(rows.len(), 2);
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    2,
                    "native membership source calls: {outer}, {not}"
                );
            }
        }
        for outer in ["membership_native", "scalar_inputs"] {
            CALLS.store(0, Ordering::SeqCst);
            let rows = c.execute(&format!("SELECT (between_tick()+n) IN (SELECT n FROM membership_native) FROM {outer}"), &crate::Parameters::new()).unwrap().rows;
            assert_eq!(rows.len(), 2);
            assert_eq!(
                CALLS.load(Ordering::SeqCst),
                2,
                "membership LHS calls: {outer}"
            );
        }
        for (operator, count) in [("UNION", 1), ("INTERSECT", 1), ("EXCEPT", 0)] {
            CALLS.store(0, Ordering::SeqCst);
            let rows = c.execute(&format!("SELECT type::record('docs',between_tick()) AS id {operator} SELECT type::record('docs',between_tick())"), &crate::Parameters::new()).unwrap().rows;
            assert_eq!(rows.len(), count);
            assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        }
        c.execute(
            "CREATE TABLE native_between(value INTEGER)",
            &crate::Parameters::new(),
        )
        .unwrap();
        c.execute(
            "INSERT INTO native_between VALUES (7)",
            &crate::Parameters::new(),
        )
        .unwrap();
        for negate in ["", "NOT "] {
            CALLS.store(0, Ordering::SeqCst);
            let rows = c.execute(&format!(
                "SELECT record::id(type::record('docs',between_tick())) {negate}BETWEEN value AND value FROM native_between"
            ), &crate::Parameters::new()).unwrap().rows;
            assert_eq!(
                rows,
                vec![vec![Value::Integer(i64::from(negate.is_empty()))]]
            );
            assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        }
        for negate in ["", "NOT "] {
            for (lower, upper, calls) in [
                ("record::id(type::record('docs',between_tick()))", "10", 1),
                ("1", "record::id(type::record('docs',between_tick()))", 1),
                (
                    "record::id(type::record('docs',between_tick()))",
                    "record::id(type::record('docs',between_tick()))",
                    2,
                ),
            ] {
                CALLS.store(0, Ordering::SeqCst);
                let rows = c
                    .execute(
                        &format!(
                            "SELECT value {negate}BETWEEN {lower} AND {upper} FROM native_between"
                        ),
                        &crate::Parameters::new(),
                    )
                    .unwrap()
                    .rows;
                assert_eq!(
                    rows,
                    vec![vec![Value::Integer(i64::from(negate.is_empty()))]]
                );
                assert_eq!(CALLS.load(Ordering::SeqCst), calls);
            }
        }
        c.execute(
            "CREATE TABLE correlation_inputs(n INTEGER)",
            &crate::Parameters::new(),
        )
        .unwrap();
        c.execute(
            "INSERT INTO correlation_inputs VALUES(1),(2)",
            &crate::Parameters::new(),
        )
        .unwrap();
        for (projection, expected_calls) in [
            ("(SELECT DISTINCT CASE WHEN d.n>0 THEN between_tick() ELSE d.n END AS x FROM correlation_inputs ORDER BY x DESC,n LIMIT 0)", 0),
            ("(SELECT DISTINCT CASE WHEN d.n>0 THEN between_tick() ELSE d.n END AS x FROM correlation_inputs ORDER BY x DESC,n LIMIT 1)", 4),
            ("(SELECT DISTINCT CASE WHEN d.n>0 THEN 1 ELSE d.n END AS x FROM correlation_inputs ORDER BY x DESC,between_tick() LIMIT 1)", 4),
            ("(SELECT CASE WHEN d.n>0 THEN between_tick() ELSE d.n END AS x FROM correlation_inputs ORDER BY x+0 DESC LIMIT 1)", 8),
            ("(SELECT CASE WHEN d.n>0 THEN between_tick() ELSE d.n END AS x FROM correlation_inputs ORDER BY x DESC LIMIT 0)", 0),
            ("(SELECT CASE WHEN d.n>0 THEN between_tick() ELSE d.n END AS x FROM correlation_inputs ORDER BY x DESC,n LIMIT 1)", 4),
            ("(SELECT CASE WHEN d.n>0 THEN between_tick() ELSE d.n END AS x FROM correlation_inputs ORDER BY x DESC LIMIT 1)", 4),
            (
                "(SELECT between_tick()+d.n AS x FROM correlation_inputs ORDER BY x DESC LIMIT 1)",
                4,
            ),
            ("(SELECT between_tick() WHERE d.n>0)", 2),
            (
                "d.n IN (SELECT between_tick() FROM correlation_inputs WHERE n<=d.n)",
                3,
            ),
            (
                "d.n NOT IN (SELECT between_tick() FROM correlation_inputs WHERE n<=d.n)",
                3,
            ),
            ("(SELECT between_tick() WHERE d.n<2)", 1),
            ("(SELECT between_tick() WHERE d.n<0)", 0),
            ("(SELECT between_tick() WHERE d.n>0)+1", 2),
            ("(SELECT between_tick() WHERE d.n>0)=d.n", 2),
            ("d.n<(SELECT between_tick() WHERE d.n>0)", 2),
            (
                "(SELECT between_tick() WHERE d.n>0)=(SELECT between_tick() WHERE d.n>0)",
                4,
            ),
        ] {
            let native = format!("SELECT {projection} FROM correlation_inputs AS d ORDER BY d.n");
            CALLS.store(0, Ordering::SeqCst);
            let expected = c.execute(&native, &crate::Parameters::new()).unwrap().rows;
            assert_eq!(
                CALLS.load(Ordering::SeqCst),
                expected_calls,
                "native: {projection}"
            );
            let sql = native.replace("FROM correlation_inputs AS d", "FROM scalar_inputs AS d");
            for profile in [false, true] {
                CALLS.store(0, Ordering::SeqCst);
                let actual = if profile {
                    c.profile_select(&sql, &crate::Parameters::new())
                        .unwrap()
                        .result
                        .rows
                } else {
                    c.execute(&sql, &crate::Parameters::new()).unwrap().rows
                };
                assert_eq!(actual, expected, "{sql}, profile={profile}");
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    expected_calls,
                    "{sql}, profile={profile}"
                );
            }
        }
    }
}

#[cfg(test)]
mod nested_accessor_tests {
    use super::*;
    #[test]
    fn derived_accessors_allow_scalar_parents_without_weakening_stored_roots() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for bytes in [
            Value::Integer(7).encode().unwrap(),
            Value::Null.encode().unwrap(),
            Value::Array(vec![]).encode().unwrap(),
            vec![0xff],
        ] {
            for function in ["__romedb_nested_value", "__romedb_value", "__romedb_scalar"] {
                let mut statement = c
                    .prepare(format!("SELECT {function}(?1,'[\"n\"]')"))
                    .unwrap();
                statement
                    .bind_at(
                        std::num::NonZeroUsize::new(1).unwrap(),
                        turso_core::Value::Blob(bytes.clone()),
                    )
                    .unwrap();
                let result = crate::collect_rows(&mut statement);
                if function == "__romedb_nested_value" && bytes != [0xff] {
                    let rows = result.unwrap();
                    let turso_core::Value::Blob(value) = &rows[0][0] else {
                        panic!("typed nested value")
                    };
                    assert_eq!(Value::decode(value).unwrap(), Value::Null);
                } else {
                    assert!(result.is_err(), "{function}, {bytes:?}");
                }
            }
        }
    }
}

#[cfg(test)]
mod vector_field_tests {
    use super::*;
    #[test]
    fn fused_vector_errors_preserve_native_transaction_disposition() {
        for input in [
            turso_core::Value::Null,
            turso_core::Value::Blob(vec![255]),
            turso_core::Value::Blob(
                Value::Object(crate::Document::from([("v".into(), Value::Null)]))
                    .encode()
                    .unwrap(),
            ),
        ] {
            for outer in [false, true] {
                let run = |expression: &str| {
                    let db = crate::Database::open(":memory:").unwrap();
                    let c = db.connect().unwrap();
                    let sql = |sql: &str| c.execute(sql, &crate::Parameters::new()).unwrap();
                    sql("CREATE TABLE prior(n INTEGER)");
                    sql("INSERT INTO prior VALUES(1)");
                    if outer {
                        sql("BEGIN");
                        sql("INSERT INTO prior VALUES(2)");
                    }
                    let mut statement = c.prepare(format!("SELECT {expression}")).unwrap();
                    statement
                        .bind_at(std::num::NonZeroUsize::new(1).unwrap(), input.clone())
                        .unwrap();
                    let error = crate::collect_rows(&mut statement).unwrap_err().to_string();
                    drop(statement);
                    let state = c.transaction_state();
                    let rows = sql("SELECT n FROM prior ORDER BY n").rows;
                    // Both accessor forms report read-only scalar extension
                    // errors. The approved engine fix preserves caller work.
                    assert_eq!(
                        state,
                        if outer {
                            crate::TransactionState::Active
                        } else {
                            crate::TransactionState::Autocommit
                        }
                    );
                    let mut expected = vec![vec![Value::Integer(1)]];
                    if outer {
                        expected.push(vec![Value::Integer(2)]);
                    }
                    assert_eq!(rows, expected);
                    sql("INSERT INTO prior VALUES(3)");
                    assert_eq!(
                        sql("SELECT count(*) FROM prior").rows,
                        vec![vec![Value::Integer(if outer { 3 } else { 2 })]]
                    );
                    if outer {
                        sql("ROLLBACK");
                        assert_eq!(
                            sql("SELECT n FROM prior").rows,
                            vec![vec![Value::Integer(1)]]
                        );
                    }
                    (error, state, rows)
                };
                assert_eq!(
                    run("__romedb_vector_field(?1,'[\"v\"]')"),
                    run("__romedb_vector_input(__romedb_value(?1,'[\"v\"]'))")
                );
            }
        }
    }

    #[test]
    fn fused_vector_fields_match_generic_conversion_and_errors() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let mut values = vec![
            Value::Null,
            Value::Integer(7),
            Value::String("[1,2,3]".into()),
            Value::Binary(vec![255]),
        ];
        for v in [
            Value::vector32(&[1., 2., 3.]),
            Value::vector64(&[1., 2., 3.]),
            Value::vector32_sparse(&[1., 0., 3.]),
            Value::vector8(&[1., 2., 3.]),
            Value::vector1bit(&[1., -2., 3.]),
        ] {
            let v = v.unwrap();
            if let Value::Vector(bytes) = &v {
                values.push(Value::Binary(bytes.clone()));
            }
            values.push(v);
        }
        let mut inputs = vec![
            turso_core::Value::Null,
            turso_core::Value::Blob(vec![255]),
            turso_core::Value::Blob(Value::Integer(1).encode().unwrap()),
        ];
        for value in values {
            inputs.push(turso_core::Value::Blob(
                Value::Object(crate::Document::from([("v".into(), value)]))
                    .encode()
                    .unwrap(),
            ));
        }
        inputs.push(turso_core::Value::Blob(b"FDB\x01{\"type\":\"Object\",\"value\":{\"v\":{\"type\":\"String\",\"value\":\"[1,2,3]\"},\"bad\":{\"type\":\"Vector\",\"value\":[255]}}}".to_vec()));
        for input in inputs {
            for path in ["[\"v\"]", "[\"missing\"]", "[\"v\",\"nested\"]", "[]"] {
                let run = |expr: &str| {
                    let mut statement = c.prepare(format!("SELECT {expr}")).unwrap();
                    statement
                        .bind_at(std::num::NonZeroUsize::new(1).unwrap(), input.clone())
                        .unwrap();
                    crate::collect_rows(&mut statement).map_err(|e| e.to_string())
                };
                assert_eq!(
                    run(&format!("__romedb_vector_field(?1,'{path}')")),
                    run(&format!(
                        "__romedb_vector_input(__romedb_value(?1,'{path}'))"
                    )),
                    "{input:?}, {path}"
                );
            }
        }
    }
}

#[cfg(test)]
mod count_value_tests {
    use super::*;
    use crate::EngineValue;
    #[test]
    fn count_marker_validates_encoded_values_and_returns_only_presence() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for (value, expected) in [
            (Value::Null, EngineValue::Null),
            (Value::Boolean(false), EngineValue::from_i64(1)),
            (
                Value::Array(vec![Value::String("x".repeat(65536))]),
                EngineValue::from_i64(1),
            ),
        ] {
            let result = c
                .run(
                    "SELECT __romedb_count_value(?1)",
                    &[EngineValue::Blob(value.encode().unwrap())],
                )
                .unwrap();
            assert_eq!(result, vec![vec![expected]]);
        }
        let mut invalid_vector = b"FDB\x01".to_vec();
        invalid_vector.extend(serde_json::to_vec(&Value::Vector(vec![])).unwrap());
        for bytes in [vec![], b"FDB\x01{".to_vec(), invalid_vector] {
            assert!(c
                .run(
                    "SELECT __romedb_count_value(?1)",
                    &[EngineValue::Blob(bytes)]
                )
                .is_err());
        }
        assert!(c
            .run("SELECT __romedb_count_value('not encoded')", &[])
            .is_err());
        assert_eq!(
            c.run("SELECT __romedb_count_value(NULL)", &[]).unwrap(),
            vec![vec![EngineValue::Null]]
        );
    }
}

#[cfg(test)]
mod grouped_evaluation_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CALLS: AtomicUsize = AtomicUsize::new(0);

    #[scalar(name = "grouped_tick")]
    fn grouped_tick(_: &[ExtValue]) -> ExtValue {
        CALLS.fetch_add(1, Ordering::SeqCst);
        ExtValue::from_integer(1)
    }

    #[test]
    fn projected_aggregate_aliases_do_not_repeat_volatile_arguments() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        unsafe {
            let api = c.engine._build_turso_ext();
            let code = (api.register_scalar_function)(
                api.ctx,
                c"grouped_tick".as_ptr(),
                0,
                false,
                0,
                grouped_tick,
                None,
                None,
            );
            c.engine._free_extension_ctx(api);
            assert_eq!(code, ResultCode::OK);
        }
        let params = crate::Parameters::new();
        for sql in [
            "CREATE TABLE docs",
            "CREATE TABLE baseline(k,n)",
            "INSERT INTO docs {k:'a',n:1}",
            "INSERT INTO docs {k:'a',n:2}",
            "INSERT INTO docs {k:'b',n:4}",
            "INSERT INTO baseline VALUES('a',1),('a',2),('b',4)",
        ] {
            c.execute(sql, &params).unwrap();
        }
        for output in [
            "sum(n+grouped_tick())",
            "sum(n+grouped_tick())+1",
            "SUM(n+grouped_tick())",
        ] {
            for having in ["total>0", "sum(n+grouped_tick())>0"] {
                for order in ["total", "sum(n+grouped_tick())"] {
                    let sql = |table| {
                        format!("SELECT k,{output} AS total FROM {table} GROUP BY k HAVING {having} ORDER BY {order},k")
                    };
                    CALLS.store(0, Ordering::SeqCst);
                    let expected = c.execute(&sql("baseline"), &params).unwrap().rows;
                    assert_eq!(
                        CALLS.load(Ordering::SeqCst),
                        3,
                        "native {output}: {having}: {order}"
                    );
                    CALLS.store(0, Ordering::SeqCst);
                    assert_eq!(c.execute(&sql("docs"), &params).unwrap().rows, expected);
                    assert_eq!(
                        CALLS.load(Ordering::SeqCst),
                        3,
                        "{output}: {having}: {order}"
                    );
                    CALLS.store(0, Ordering::SeqCst);
                    assert_eq!(
                        c.profile_select(&sql("docs"), &params).unwrap().result.rows,
                        expected
                    );
                    assert_eq!(
                        CALLS.load(Ordering::SeqCst),
                        3,
                        "profile {output}: {having}: {order}"
                    );
                }
            }
        }
        for (minimum, calls) in [(1, 2), (10, 0)] {
            let params = crate::Parameters::from([("$minimum".into(), Value::Integer(minimum))]);
            let aggregate = "sum(n+grouped_tick()) FILTER (WHERE n>$minimum)";
            for having in [
                "total IS NOT NULL".to_owned(),
                format!("{aggregate} IS NOT NULL"),
            ] {
                let sql = |table| {
                    format!("SELECT k,{aggregate} AS total FROM {table} GROUP BY k HAVING {having} ORDER BY {aggregate},k")
                };
                CALLS.store(0, Ordering::SeqCst);
                let expected = c.execute(&sql("baseline"), &params).unwrap().rows;
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    calls,
                    "native FILTER {minimum}"
                );
                CALLS.store(0, Ordering::SeqCst);
                assert_eq!(c.execute(&sql("docs"), &params).unwrap().rows, expected);
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    calls,
                    "FILTER {minimum}: {having}"
                );
                CALLS.store(0, Ordering::SeqCst);
                assert_eq!(
                    c.profile_select(&sql("docs"), &params).unwrap().result.rows,
                    expected
                );
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    calls,
                    "profile FILTER {minimum}: {having}"
                );
            }
        }
    }
}

#[cfg(test)]
mod cte_evaluation_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    #[scalar(name = "cte_tick")]
    fn cte_tick(_: &[ExtValue]) -> ExtValue {
        CALLS.fetch_add(1, Ordering::SeqCst);
        ExtValue::from_integer(1)
    }
    #[test]
    fn local_cte_correlation_probes_do_not_execute_sources() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        unsafe {
            let api = c.engine._build_turso_ext();
            let code = (api.register_scalar_function)(
                api.ctx,
                c"cte_tick".as_ptr(),
                0,
                false,
                0,
                cte_tick,
                None,
                None,
            );
            c.engine._free_extension_ctx(api);
            assert_eq!(code, ResultCode::OK);
        }
        let params = crate::Parameters::new();
        for sql in [
            "CREATE TABLE docs",
            "CREATE TABLE baseline(n)",
            "INSERT INTO docs {n:1}",
            "INSERT INTO docs {n:2}",
            "INSERT INTO docs {n:4}",
            "INSERT INTO baseline VALUES(1),(2),(4)",
        ] {
            c.execute(sql, &params).unwrap();
        }
        c.execute("CREATE TABLE local_cte_keys(n)", &params)
            .unwrap();
        c.execute("INSERT INTO local_cte_keys VALUES(1),(5)", &params)
            .unwrap();
        for join in ["JOIN", "LEFT JOIN", "RIGHT JOIN"] {
            for materialization in ["", "MATERIALIZED", "NOT MATERIALIZED"] {
                for tail in ["", " LIMIT 0"] {
                    for consumer in [
                        "SELECT max(m)+sum(tick) FROM chosen WHERE m<n",
                        "SELECT tick FROM chosen WHERE m<n ORDER BY m LIMIT 1",
                        "SELECT tick FROM chosen WHERE m<n ORDER BY m DESC LIMIT 1 OFFSET 1",
                    ] {
                        let query = |source| {
                            format!("SELECT n,(WITH chosen AS {materialization} (SELECT n AS m,cte_tick() AS tick FROM baseline) {consumer}) AS prior FROM {source} a {join} local_cte_keys b USING(n) ORDER BY n{tail}")
                        };
                        CALLS.store(0, Ordering::SeqCst);
                        let expected = c.execute(&query("baseline"), &params).unwrap().rows;
                        let calls = CALLS.load(Ordering::SeqCst);
                        if !tail.is_empty() {
                            assert_eq!(calls, 0);
                        }
                        // The new optimizer can decorrelate the relational
                        // query differently from encoded document accessors.
                        // Profiling/planning must not add source evaluations.
                        let mut document_calls = None;
                        for profile in [false, true] {
                            CALLS.store(0, Ordering::SeqCst);
                            let sql = query("docs");
                            let actual = if profile {
                                c.profile_select(&sql, &params).unwrap().result.rows
                            } else {
                                c.execute(&sql, &params).unwrap().rows
                            };
                            assert_eq!(actual, expected, "{sql}");
                            let observed = CALLS.load(Ordering::SeqCst);
                            if !tail.is_empty() {
                                assert_eq!(observed, 0);
                            }
                            if let Some(expected_calls) = document_calls {
                                assert_eq!(observed, expected_calls, "{sql}; profile={profile}");
                            } else {
                                document_calls = Some(observed);
                            }
                        }
                        CALLS.store(0, Ordering::SeqCst);
                        c.execute(&format!("EXPLAIN QUERY PLAN {}", query("docs")), &params)
                            .unwrap();
                        assert_eq!(
                            CALLS.load(Ordering::SeqCst),
                            0,
                            "planning {}",
                            query("docs")
                        );
                    }
                }
            }
        }
        for materialization in ["MATERIALIZED", "NOT MATERIALIZED"] {
            for joins in ["JOIN q v ON 1", "JOIN q v ON 1 JOIN q w ON 1"] {
                for limit in ["", " LIMIT 0"] {
                    let projection = if joins.contains("q w") {
                        "v.*,w.*"
                    } else {
                        "v.*"
                    };
                    let query = |source| {
                        format!(
                    "WITH q(x,x) AS {materialization} (SELECT cte_tick(),cte_tick() FROM baseline) SELECT {projection} FROM {source} d {joins}{limit}"
                )
                    };
                    CALLS.store(0, Ordering::SeqCst);
                    c.execute(&format!("EXPLAIN QUERY PLAN {}", query("docs")), &params)
                        .unwrap();
                    assert_eq!(CALLS.load(Ordering::SeqCst), 0, "duplicate CTE planning");
                    let expected = c.execute(&query("baseline"), &params).unwrap().rows;
                    let calls = CALLS.load(Ordering::SeqCst);
                    for profile in [false, true] {
                        CALLS.store(0, Ordering::SeqCst);
                        let sql = query("docs");
                        let actual = if profile {
                            c.profile_select(&sql, &params).unwrap().result.rows
                        } else {
                            c.execute(&sql, &params).unwrap().rows
                        };
                        assert_eq!(actual, expected, "{sql}");
                        assert_eq!(CALLS.load(Ordering::SeqCst), calls, "{sql}");
                    }
                }
            }
        }
        for nested in [false, true] {
            for materialization in ["", "MATERIALIZED", "NOT MATERIALIZED"] {
                for delete in [false, true] {
                    let query = |table: &str| {
                        let write = if delete {
                            format!("DELETE FROM {table}")
                        } else {
                            format!("UPDATE {table} SET n=n+10")
                        };
                        let body = format!("SELECT n+cte_tick()-1 AS n FROM {table}");
                        let body = if nested {
                            format!(
                                "WITH local_q AS {materialization} ({body}) SELECT n FROM local_q"
                            )
                        } else {
                            body
                        };
                        format!("WITH {table} AS (SELECT cte_tick() AS n), chosen AS {materialization} ({body}) {write} WHERE n IN (SELECT n FROM chosen) RETURNING n")
                    };
                    let mut expected = None;
                    for table in ["baseline", "docs"] {
                        c.execute("BEGIN", &params).unwrap();
                        CALLS.store(0, Ordering::SeqCst);
                        let result = c.execute(&query(table), &params).unwrap();
                        let calls = CALLS.load(Ordering::SeqCst);
                        if let Some((rows, expected_calls)) = &expected {
                            assert_eq!(&result.rows, rows, "{}", query(table));
                            assert_eq!(calls, *expected_calls, "{}", query(table));
                        } else {
                            assert!(calls > 0);
                            expected = Some((result.rows, calls));
                        }
                        c.execute("ROLLBACK", &params).unwrap();
                    }
                }
            }
        }
        c.execute(
            "CREATE VIEW counted_view AS SELECT n AS m,cte_tick() AS value FROM baseline",
            &params,
        )
        .unwrap();
        for limit in ["", " LIMIT 0"] {
            let query = |table| {
                format!("SELECT d.n FROM (SELECT n FROM {table}) d JOIN (SELECT 1 AS marker) ON d.n IN (SELECT cte_tick() FROM baseline) ORDER BY d.n{limit}")
            };
            CALLS.store(0, Ordering::SeqCst);
            c.execute(&format!("EXPLAIN QUERY PLAN {}", query("docs")), &params)
                .unwrap();
            assert_eq!(CALLS.load(Ordering::SeqCst), 0);
            let expected = c.execute(&query("baseline"), &params).unwrap().rows;
            let expected_calls = CALLS.load(Ordering::SeqCst);
            assert_eq!(expected_calls, 3);
            // Track the pinned LIMIT 0 evaluation difference explicitly: native
            // membership builds its RHS first; the logical wrapper skips it.
            let logical_calls = if limit.is_empty() { expected_calls } else { 0 };
            for predicate in [
                "(SELECT d.n IN (SELECT cte_tick() FROM baseline))",
                "(WITH lhs(k) AS NOT MATERIALIZED (SELECT d.n), members(v) AS MATERIALIZED (SELECT cte_tick() FROM baseline) SELECT CASE WHEN typeof(k)='blob' THEN k IN (SELECT v FROM members) ELSE k IN (SELECT +v FROM members) END FROM lhs)",
            ] {
                let native = format!("SELECT d.n FROM (SELECT n FROM baseline) d JOIN (SELECT 1 AS marker) ON {predicate} ORDER BY d.n{limit}");
                CALLS.store(0, Ordering::SeqCst);
                c.execute(&format!("EXPLAIN QUERY PLAN {native}"), &params).unwrap();
                assert_eq!(CALLS.load(Ordering::SeqCst), 0);
                assert_eq!(c.execute(&native, &params).unwrap().rows, expected);
                assert_eq!(CALLS.load(Ordering::SeqCst), logical_calls, "{predicate}, {limit}");
            }

            CALLS.store(0, Ordering::SeqCst);
            assert_eq!(c.execute(&query("docs"), &params).unwrap().rows, expected);
            assert_eq!(CALLS.load(Ordering::SeqCst), logical_calls, "{limit}");
            CALLS.store(0, Ordering::SeqCst);
            assert_eq!(
                c.profile_select(&query("docs"), &params)
                    .unwrap()
                    .result
                    .rows,
                expected
            );
            assert_eq!(CALLS.load(Ordering::SeqCst), logical_calls, "{limit}");
        }
        for (table, condition) in [
            ("baseline", "n=m"),
            ("baseline", "(n+0)=(m+0)"),
            ("docs", "n=m"),
        ] {
            let sql = format!("SELECT n,value FROM (SELECT n FROM {table}) JOIN counted_view ON {condition} ORDER BY n");
            CALLS.store(0, Ordering::SeqCst);
            c.execute(&format!("EXPLAIN QUERY PLAN {sql}"), &params)
                .unwrap();
            assert_eq!(CALLS.load(Ordering::SeqCst), 0);
            let rows = c.execute(&sql, &params).unwrap().rows;
            assert_eq!(
                rows,
                vec![
                    vec![Value::Integer(1), Value::Integer(1)],
                    vec![Value::Integer(2), Value::Integer(1)],
                    vec![Value::Integer(4), Value::Integer(1)]
                ]
            );
        }
        for limit in ["", " LIMIT 0"] {
            let query = |table| {
                format!("WITH x AS MATERIALIZED (SELECT * FROM counted_view) SELECT n,value FROM (SELECT n FROM {table}) JOIN x ON n=m ORDER BY n{limit}")
            };
            CALLS.store(0, Ordering::SeqCst);
            c.execute(&format!("EXPLAIN QUERY PLAN {}", query("docs")), &params)
                .unwrap();
            assert_eq!(CALLS.load(Ordering::SeqCst), 0);
            let expected = c.execute(&query("baseline"), &params).unwrap().rows;
            let expected_calls = CALLS.load(Ordering::SeqCst);
            assert_eq!(expected_calls, 3);
            CALLS.store(0, Ordering::SeqCst);
            assert_eq!(c.execute(&query("docs"), &params).unwrap().rows, expected);
            assert_eq!(CALLS.load(Ordering::SeqCst), expected_calls);
            CALLS.store(0, Ordering::SeqCst);
            assert_eq!(
                c.profile_select(&query("docs"), &params)
                    .unwrap()
                    .result
                    .rows,
                expected
            );
            assert_eq!(CALLS.load(Ordering::SeqCst), expected_calls);
        }
        for limit in ["", " LIMIT 0"] {
            let query = |table| {
                format!("SELECT n FROM (SELECT n FROM {table}) CROSS JOIN (WITH x AS MATERIALIZED (SELECT cte_tick() AS m) SELECT m FROM x) WHERE n=m ORDER BY n{limit}")
            };
            CALLS.store(0, Ordering::SeqCst);
            let expected = c.execute(&query("baseline"), &params).unwrap().rows;
            let expected_calls = CALLS.load(Ordering::SeqCst);
            // The pinned engine materializes this FROM source before outer LIMIT 0.
            assert_eq!(expected_calls, 1);
            CALLS.store(0, Ordering::SeqCst);
            assert_eq!(c.execute(&query("docs"), &params).unwrap().rows, expected);
            assert_eq!(CALLS.load(Ordering::SeqCst), expected_calls);
            CALLS.store(0, Ordering::SeqCst);
            assert_eq!(
                c.profile_select(&query("docs"), &params)
                    .unwrap()
                    .result
                    .rows,
                expected
            );
            assert_eq!(CALLS.load(Ordering::SeqCst), expected_calls);
        }
        for materialization in ["MATERIALIZED", "NOT MATERIALIZED", ""] {
            for limit in [" LIMIT 0", ""] {
                let sql = |table| {
                    format!("SELECT n,(WITH x AS {materialization} (SELECT n,cte_tick() AS v FROM {table}) SELECT sum(v) FROM x WHERE x.n>=d.n) FROM {table} d ORDER BY n{limit}")
                };
                CALLS.store(0, Ordering::SeqCst);
                let expected = c.execute(&sql("baseline"), &params).unwrap().rows;
                let expected_calls = CALLS.load(Ordering::SeqCst);
                if !limit.is_empty() {
                    assert_eq!(expected_calls, 0);
                } else {
                    assert!(expected_calls > 0);
                }
                CALLS.store(0, Ordering::SeqCst);
                assert_eq!(c.execute(&sql("docs"), &params).unwrap().rows, expected);
                let document_calls = CALLS.load(Ordering::SeqCst);
                if !limit.is_empty() {
                    assert_eq!(document_calls, 0);
                } else {
                    assert!(document_calls > 0);
                }
                // Relational decorrelation and encoded document accessors can
                // use different plans. Profiling must not evaluate extra rows.
                CALLS.store(0, Ordering::SeqCst);
                assert_eq!(
                    c.profile_select(&sql("docs"), &params).unwrap().result.rows,
                    expected
                );
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    document_calls,
                    "profile {materialization}: {limit}"
                );
            }
        }
        for (projection, predicate, calls) in [("1", " WHERE v>0", 2), ("cte_tick()", "", 0)] {
            for negate in ["", "NOT "] {
                let source_value = if calls == 0 { "1" } else { "cte_tick()" };
                let sql = format!("SELECT n,{negate}EXISTS(WITH x AS NOT MATERIALIZED (SELECT {source_value} AS v FROM docs s WHERE s.n>d.n) SELECT {projection} FROM x{predicate}) FROM docs d ORDER BY n");
                let flag = i64::from(negate.is_empty());
                let expected = vec![
                    vec![Value::Integer(1), Value::Integer(flag)],
                    vec![Value::Integer(2), Value::Integer(flag)],
                    vec![Value::Integer(4), Value::Integer(1 - flag)],
                ];
                CALLS.store(0, Ordering::SeqCst);
                assert_eq!(c.execute(&sql, &params).unwrap().rows, expected);
                assert_eq!(CALLS.load(Ordering::SeqCst), calls, "execute {sql}");
                CALLS.store(0, Ordering::SeqCst);
                assert_eq!(
                    c.profile_select(&sql, &params).unwrap().result.rows,
                    expected
                );
                assert_eq!(CALLS.load(Ordering::SeqCst), calls, "profile {sql}");
            }
        }
        for outer_limit in ["", " LIMIT 0"] {
            for skipped in [false, true] {
                let offset = if skipped {
                    "cte_tick()"
                } else {
                    "cte_tick()-1"
                };
                let sql = format!("SELECT (SELECT array::new(d.n,cte_tick()) LIMIT cte_tick() OFFSET {offset}) FROM docs d ORDER BY n{outer_limit}");
                let expected = if outer_limit.is_empty() {
                    vec![1, 2, 4]
                        .into_iter()
                        .map(|n| {
                            vec![if skipped {
                                Value::Null
                            } else {
                                Value::Array(vec![Value::Integer(n), Value::Integer(1)])
                            }]
                        })
                        .collect::<Vec<_>>()
                } else {
                    vec![]
                };
                let expected_calls = if outer_limit.is_empty() {
                    if skipped {
                        6
                    } else {
                        9
                    }
                } else {
                    0
                };
                CALLS.store(0, Ordering::SeqCst);
                assert_eq!(c.execute(&sql, &params).unwrap().rows, expected);
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    expected_calls,
                    "execute {sql}"
                );
                CALLS.store(0, Ordering::SeqCst);
                assert_eq!(
                    c.profile_select(&sql, &params).unwrap().result.rows,
                    expected
                );
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    expected_calls,
                    "profile {sql}"
                );
            }
        }
        for offset in [0, 1] {
            for outer_limit in ["", " LIMIT 0"] {
                let reference = format!("SELECT (SELECT array::new(s.n,1) FROM docs s WHERE s.n>=d.n LIMIT 1 OFFSET {offset}) FROM docs d ORDER BY n{outer_limit}");
                let expected = c.execute(&reference, &params).unwrap().rows;
                let offset_expr = if offset == 0 {
                    "cte_tick()-1"
                } else {
                    "cte_tick()"
                };
                let sql = format!("SELECT (SELECT array::new(s.n,cte_tick()) FROM docs s WHERE s.n>=d.n LIMIT cte_tick() OFFSET {offset_expr}) FROM docs d ORDER BY n{outer_limit}");
                let expected_calls = if !outer_limit.is_empty() {
                    0
                } else if offset == 0 {
                    9
                } else {
                    8
                };
                CALLS.store(0, Ordering::SeqCst);
                assert_eq!(c.execute(&sql, &params).unwrap().rows, expected);
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    expected_calls,
                    "execute {sql}"
                );
                CALLS.store(0, Ordering::SeqCst);
                assert_eq!(
                    c.profile_select(&sql, &params).unwrap().result.rows,
                    expected
                );
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    expected_calls,
                    "profile {sql}"
                );
            }
        }
        for offset in [0, 1] {
            for outer_limit in ["", " LIMIT 0"] {
                let reference = format!("SELECT (SELECT s.n+1 FROM baseline s WHERE s.n>=d.n LIMIT 1 OFFSET {offset}) FROM docs d ORDER BY n{outer_limit}");
                let expected = c.execute(&reference, &params).unwrap().rows;
                let offset_expr = if offset == 0 {
                    "cte_tick()-1"
                } else {
                    "cte_tick()"
                };
                let sql = format!("SELECT (SELECT s.n+cte_tick() FROM baseline s WHERE s.n>=d.n LIMIT cte_tick() OFFSET {offset_expr}) FROM docs d ORDER BY n{outer_limit}");
                let expected_calls = if !outer_limit.is_empty() {
                    0
                } else if offset == 0 {
                    9
                } else {
                    8
                };
                CALLS.store(0, Ordering::SeqCst);
                assert_eq!(c.execute(&sql, &params).unwrap().rows, expected);
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    expected_calls,
                    "execute {sql}"
                );
                CALLS.store(0, Ordering::SeqCst);
                assert_eq!(
                    c.profile_select(&sql, &params).unwrap().result.rows,
                    expected
                );
                assert_eq!(
                    CALLS.load(Ordering::SeqCst),
                    expected_calls,
                    "profile {sql}"
                );
            }
        }
        c.execute("CREATE TABLE evaluation_rhs(n)", &params)
            .unwrap();
        c.execute("INSERT INTO evaluation_rhs VALUES(1),(3),(5)", &params)
            .unwrap();
        for join in ["LEFT JOIN", "RIGHT JOIN"] {
            for predicate in [
                "EXISTS(SELECT 1 WHERE n>=cte_tick())",
                "NOT EXISTS(SELECT 1 WHERE n>=cte_tick())",
                "EXISTS(SELECT 1 WHERE EXISTS(SELECT 1 WHERE n>=cte_tick()))",
                "(SELECT n)>=cte_tick()",
                "(SELECT cte_tick())<=n",
                "n IN(SELECT cte_tick())",
                "n NOT IN(SELECT cte_tick())",
                "cte_tick() IN(SELECT n)",
                "cte_tick() NOT IN(SELECT n)",
                "cte_tick() IN(SELECT n WHERE 0)",
                "cte_tick() NOT IN(SELECT n WHERE 0)",
                "n IN(SELECT cte_tick() UNION ALL SELECT n)",
                "n IN(SELECT cte_tick() UNION ALL SELECT n LIMIT 1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() UNION ALL SELECT n LIMIT 1 OFFSET 1)",
                "n IN(SELECT cte_tick() UNION ALL SELECT n LIMIT 0)",
                "n NOT IN(SELECT cte_tick() UNION ALL SELECT n LIMIT 0)",
                "n IN(SELECT cte_tick() UNION ALL SELECT n LIMIT 1)",
                "n NOT IN(SELECT cte_tick() UNION ALL SELECT n LIMIT 1)",
                "n IN(SELECT cte_tick() UNION ALL SELECT n LIMIT -1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() UNION ALL SELECT n LIMIT -1 OFFSET 1)",
                "n IN(SELECT cte_tick() UNION SELECT n)",
                "n IN(SELECT cte_tick() INTERSECT SELECT n)",
                "n IN(SELECT cte_tick() EXCEPT SELECT n)",
                "n IN(SELECT cte_tick() UNION SELECT n LIMIT 0)",
                "n NOT IN(SELECT cte_tick() UNION SELECT n LIMIT 0)",
                "n IN(SELECT cte_tick() UNION SELECT n LIMIT 1)",
                "n NOT IN(SELECT cte_tick() UNION SELECT n LIMIT 1)",
                "n IN(SELECT cte_tick() UNION SELECT n LIMIT 1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() UNION SELECT n LIMIT 1 OFFSET 1)",
                "n IN(SELECT cte_tick() UNION SELECT n LIMIT -1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() UNION SELECT n LIMIT -1 OFFSET 1)",
                "n IN(SELECT cte_tick() INTERSECT SELECT n LIMIT 0)",
                "n NOT IN(SELECT cte_tick() INTERSECT SELECT n LIMIT 0)",
                "n IN(SELECT cte_tick() INTERSECT SELECT n LIMIT 1)",
                "n NOT IN(SELECT cte_tick() INTERSECT SELECT n LIMIT 1)",
                "n IN(SELECT cte_tick() INTERSECT SELECT n LIMIT 1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() INTERSECT SELECT n LIMIT 1 OFFSET 1)",
                "n IN(SELECT cte_tick() INTERSECT SELECT n LIMIT -1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() INTERSECT SELECT n LIMIT -1 OFFSET 1)",
                "n IN(SELECT cte_tick() EXCEPT SELECT n LIMIT 0)",
                "n NOT IN(SELECT cte_tick() EXCEPT SELECT n LIMIT 0)",
                "n IN(SELECT cte_tick() EXCEPT SELECT n LIMIT 1)",
                "n NOT IN(SELECT cte_tick() EXCEPT SELECT n LIMIT 1)",
                "n IN(SELECT cte_tick() EXCEPT SELECT n LIMIT 1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() EXCEPT SELECT n LIMIT 1 OFFSET 1)",
                "n IN(SELECT cte_tick() EXCEPT SELECT n LIMIT -1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() EXCEPT SELECT n LIMIT -1 OFFSET 1)",
                "n IN(SELECT cte_tick() UNION ALL SELECT n ORDER BY 1 LIMIT 1)",
                "n IN(SELECT cte_tick() UNION ALL SELECT n ORDER BY 1 DESC LIMIT 1)",
                "n IN(SELECT cte_tick() UNION SELECT n ORDER BY 1 LIMIT 1)",
                "n IN(SELECT cte_tick() UNION SELECT n ORDER BY 1 DESC LIMIT 1)",
                "n IN(SELECT cte_tick() INTERSECT SELECT n ORDER BY 1 LIMIT 1)",
                "n IN(SELECT cte_tick() INTERSECT SELECT n ORDER BY 1 DESC LIMIT 1)",
                "n IN(SELECT cte_tick() EXCEPT SELECT n ORDER BY 1 LIMIT 1)",
                "n IN(SELECT cte_tick() EXCEPT SELECT n ORDER BY 1 DESC LIMIT 1)",
                "n IN(SELECT cte_tick() UNION ALL SELECT n ORDER BY 1 LIMIT 1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() UNION ALL SELECT n ORDER BY 1 LIMIT 1 OFFSET 1)",
                "n IN(SELECT cte_tick() UNION ALL SELECT n ORDER BY 1 DESC LIMIT 1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() UNION ALL SELECT n ORDER BY 1 DESC LIMIT 1 OFFSET 1)",
                "n IN(SELECT cte_tick() UNION SELECT n ORDER BY 1 LIMIT 1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() UNION SELECT n ORDER BY 1 LIMIT 1 OFFSET 1)",
                "n IN(SELECT cte_tick() UNION SELECT n ORDER BY 1 DESC LIMIT 1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() UNION SELECT n ORDER BY 1 DESC LIMIT 1 OFFSET 1)",
                "n IN(SELECT cte_tick() INTERSECT SELECT n ORDER BY 1 LIMIT 1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() INTERSECT SELECT n ORDER BY 1 LIMIT 1 OFFSET 1)",
                "n IN(SELECT cte_tick() INTERSECT SELECT n ORDER BY 1 DESC LIMIT 1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() INTERSECT SELECT n ORDER BY 1 DESC LIMIT 1 OFFSET 1)",
                "n IN(SELECT cte_tick() EXCEPT SELECT n ORDER BY 1 LIMIT 1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() EXCEPT SELECT n ORDER BY 1 LIMIT 1 OFFSET 1)",
                "n IN(SELECT cte_tick() EXCEPT SELECT n ORDER BY 1 DESC LIMIT 1 OFFSET 1)",
                "n NOT IN(SELECT cte_tick() EXCEPT SELECT n ORDER BY 1 DESC LIMIT 1 OFFSET 1)",
            ] {
                for limit in ["", " LIMIT 0"] {
                    let query = |source: &str| {
                        format!(
                        "SELECT n,(SELECT max(r.m)+cte_tick() FROM (SELECT n AS m FROM baseline) r WHERE r.m<n AND {predicate}) AS v FROM {source} d {join} evaluation_rhs r USING(n) ORDER BY n{limit}"
                    )
                    };
                    CALLS.store(0, Ordering::SeqCst);
                    c.execute(&format!("EXPLAIN QUERY PLAN {}", query("docs")), &params)
                        .unwrap();
                    assert_eq!(CALLS.load(Ordering::SeqCst), 0, "nested EXISTS planning");
                    let expected = c.execute(&query("baseline"), &params).unwrap().rows;
                    let calls = CALLS.load(Ordering::SeqCst);
                    if !limit.is_empty() {
                        assert_eq!(calls, 0);
                    }
                    let mut document_calls = None;
                    for profile in [false, true] {
                        CALLS.store(0, Ordering::SeqCst);
                        let sql = query("docs");
                        let actual = if profile {
                            c.profile_select(&sql, &params).unwrap().result.rows
                        } else {
                            c.execute(&sql, &params).unwrap().rows
                        };
                        assert_eq!(actual, expected, "{sql}");
                        let observed = CALLS.load(Ordering::SeqCst);
                        if !limit.is_empty() {
                            assert_eq!(observed, 0);
                        }
                        if let Some(expected_calls) = document_calls {
                            assert_eq!(observed, expected_calls, "profile={profile}: {sql}");
                        } else {
                            document_calls = Some(observed);
                        }
                    }
                }
            }
        }
        for hint in ["", "MATERIALIZED", "NOT MATERIALIZED"] {
            for predicate in ["(SELECT v FROM q)<=n", "n IN(SELECT v FROM q)"] {
                for limit in ["", " LIMIT 0"] {
                    let query = |source: &str| {
                        format!("WITH q(v) AS {hint} (SELECT cte_tick()) SELECT n,(SELECT max(r.m)+cte_tick() FROM (SELECT n AS m FROM baseline) r WHERE r.m<n AND {predicate}) AS v FROM {source} d RIGHT JOIN evaluation_rhs r USING(n) ORDER BY n{limit}")
                    };
                    CALLS.store(0, Ordering::SeqCst);
                    c.execute(&format!("EXPLAIN QUERY PLAN {}", query("docs")), &params)
                        .unwrap();
                    assert_eq!(CALLS.load(Ordering::SeqCst), 0, "inherited CTE metadata");
                    let expected = c.execute(&query("baseline"), &params).unwrap().rows;
                    if !limit.is_empty() {
                        assert_eq!(CALLS.load(Ordering::SeqCst), 0);
                    }
                    let mut document_calls = None;
                    for profile in [false, true] {
                        CALLS.store(0, Ordering::SeqCst);
                        let sql = query("docs");
                        let actual = if profile {
                            c.profile_select(&sql, &params).unwrap().result.rows
                        } else {
                            c.execute(&sql, &params).unwrap().rows
                        };
                        assert_eq!(actual, expected, "{sql}");
                        let observed = CALLS.load(Ordering::SeqCst);
                        if !limit.is_empty() {
                            assert_eq!(observed, 0);
                        }
                        if let Some(expected_calls) = document_calls {
                            assert_eq!(observed, expected_calls, "profile={profile}: {sql}");
                        } else {
                            document_calls = Some(observed);
                        }
                    }
                }
            }
        }
    }
}

#[scalar(name = "__romedb_unnest_json")]
fn unnest_json(args: &[ExtValue]) -> ExtValue {
    let result = (|| -> Result<ExtValue> {
        let [value] = args else {
            return Err(Error::Validation("array::unnest expects one array".into()));
        };
        Ok(ExtValue::from_text(crate::unnest::json(&decode_arg(
            value,
        )?)?))
    })();
    result.unwrap_or_else(|error| {
        ExtValue::error_with_message(match error {
            Error::Limit(message) => format!("__romedb_udf_limit:{message}"),
            Error::Validation(message) => format!("__romedb_udf_validation:{message}"),
            other => other.to_string(),
        })
    })
}
