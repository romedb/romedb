use crate::{Document, Error, Result, Value};
use std::collections::HashSet;

const MAX_ITEMS: usize = 100_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn call(name: &str, args: &[Value]) -> Result<Value> {
    if name == "array_contains" {
        return contains(args);
    }
    let [input] = args else {
        return Err(Error::Validation(format!("{name} expects one argument")));
    };
    let mut budget = crate::links::FetchBudget {
        used: 0,
        limit: MAX_BYTES,
    };
    charge(&mut budget, input)?;
    let output = match (name, input) {
        ("array_len", Value::Array(values)) => Value::Integer(values.len() as i64),
        ("array_distinct", Value::Array(values)) => {
            bounded(values.len())?;
            let mut seen = HashSet::new();
            let mut result = Vec::new();
            for value in values {
                let key = identity(value)?;
                if seen.insert(key) {
                    result.push(value.clone());
                }
            }
            Value::Array(result)
        }
        ("array_flatten", Value::Array(values)) => {
            bounded(values.len())?;
            let mut result = Vec::new();
            for value in values {
                if let Value::Array(values) = value {
                    bounded(result.len().saturating_add(values.len()))?;
                    result.extend(values.iter().cloned());
                } else {
                    bounded(result.len() + 1)?;
                    result.push(value.clone());
                }
            }
            Value::Array(result)
        }
        ("object_keys" | "object_values" | "object_entries", Value::Object(document)) => {
            bounded(document.len())?;
            Value::Array(
                document
                    .iter()
                    .map(|(key, value)| match name {
                        "object_keys" => Value::String(key.clone()),
                        "object_values" => value.clone(),
                        _ => Value::Array(vec![Value::String(key.clone()), value.clone()]),
                    })
                    .collect(),
            )
        }
        ("object_from_entries", Value::Array(entries)) => {
            bounded(entries.len())?;
            let mut document = Document::new();
            for entry in entries {
                let Value::Array(pair) = entry else {
                    return Err(Error::Validation(
                        "doc::from_entries expects [string,value] pairs".into(),
                    ));
                };
                let [Value::String(key), value] = pair.as_slice() else {
                    return Err(Error::Validation(
                        "doc::from_entries expects [string,value] pairs".into(),
                    ));
                };
                if document.insert(key.clone(), value.clone()).is_some() {
                    return Err(Error::Validation(
                        "doc::from_entries rejects duplicate keys".into(),
                    ));
                }
            }
            Value::Object(document)
        }
        _ => return Err(Error::Validation(format!("invalid {name} argument type"))),
    };
    output.validate()?;
    charge(&mut budget, &output)?;
    Ok(output)
}

fn contains(args: &[Value]) -> Result<Value> {
    let [input, needle] = args else {
        return Err(Error::Validation(
            "array::contains expects array and value".into(),
        ));
    };
    needle.encode_with_limit(Some(1024 * 1024))?;
    let mut budget = crate::links::FetchBudget {
        used: 0,
        limit: MAX_BYTES,
    };
    charge(&mut budget, input)?;
    charge(&mut budget, needle)?;
    let found = match input {
        Value::Null => false,
        Value::Array(values) => {
            bounded(values.len())?;
            values.iter().any(|value| equal(value, needle))
        }
        _ => {
            return Err(Error::Validation(
                "array::contains expects array or null".into(),
            ))
        }
    };
    let output = Value::Boolean(found);
    charge(&mut budget, &output)?;
    Ok(output)
}

fn bounded(length: usize) -> Result<()> {
    if length > MAX_ITEMS {
        return Err(Error::Limit(
            "array/object helper exceeds 100000 items".into(),
        ));
    }
    Ok(())
}

fn charge(budget: &mut crate::links::FetchBudget, value: &Value) -> Result<()> {
    budget
        .charge(value)
        .map(|_| ())
        .map_err(|_| Error::Limit("array/object helper input and output exceed 64 MiB".into()))
}

pub(crate) fn equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Record(left), Value::Record(right)) => {
            left.table.eq_ignore_ascii_case(&right.table) && left.key == right.key
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, value)| right.get(key).is_some_and(|other| equal(value, other)))
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| equal(left, right))
        }
        _ => left == right,
    }
}

pub(crate) fn identity(value: &Value) -> Result<Vec<u8>> {
    fn normalize(value: &mut Value) {
        match value {
            Value::Record(record) => record.table.make_ascii_lowercase(),
            Value::Number(number) if *number == 0.0 => *number = 0.0,
            Value::Object(document) => document.values_mut().for_each(normalize),
            Value::Array(values) => values.iter_mut().for_each(normalize),
            _ => {}
        }
    }
    let mut value = value.clone();
    normalize(&mut value);
    value.encode()
}
