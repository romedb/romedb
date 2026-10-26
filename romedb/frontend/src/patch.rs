use crate::{Document, Error, Result, Value};

struct Budget {
    bytes: crate::links::FetchBudget,
    work: usize,
}

impl Budget {
    fn charge(&mut self, value: &Value) -> Result<()> {
        self.bytes
            .charge(value)
            .map(|_| ())
            .map_err(|_| Error::Limit("PATCH cumulative values exceed 64 MiB".into()))
    }

    fn work(&mut self, amount: usize) -> Result<()> {
        self.work = self
            .work
            .checked_sub(amount)
            .ok_or_else(|| Error::Limit("PATCH work exceeds 1000000 steps".into()))?;
        Ok(())
    }
}

fn invalid(message: &str) -> Error {
    Error::Validation(format!("PATCH {message}"))
}

fn string<'a>(operation: &'a Document, field: &str) -> Result<&'a str> {
    match operation.get(field) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(invalid(&format!("requires a string {field}"))),
    }
}

fn pointer(input: &str) -> Result<Vec<String>> {
    if input.len() > 16_384 {
        return Err(Error::Limit("PATCH pointer exceeds 16384 bytes".into()));
    }
    let Some(input) = input.strip_prefix('/') else {
        return Err(invalid("requires a member pointer beginning with /"));
    };
    let mut path = Vec::new();
    for part in input.split('/') {
        if path.len() == 64 {
            return Err(Error::Limit("PATCH pointer exceeds 64 steps".into()));
        }
        let mut name = String::new();
        let mut chars = part.chars();
        while let Some(ch) = chars.next() {
            name.push(if ch == '~' {
                match chars.next() {
                    Some('0') => '~',
                    Some('1') => '/',
                    _ => return Err(invalid("has an invalid pointer escape")),
                }
            } else {
                ch
            });
        }
        path.push(name);
    }
    Ok(path)
}

fn writable(path: &[String]) -> Result<()> {
    if path.first().is_some_and(|key| key == "id") {
        return Err(invalid("cannot change id"));
    }
    Ok(())
}

fn index(key: &str, length: usize, adding: bool) -> Result<usize> {
    if adding && key == "-" {
        return Ok(length);
    }
    if key.is_empty()
        || (key.len() > 1 && key.starts_with('0'))
        || !key.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid(
            "array index is not a canonical nonnegative integer",
        ));
    }
    let index = key
        .parse::<usize>()
        .map_err(|_| invalid("array index is too large"))?;
    if index > length || (!adding && index == length) {
        return Err(invalid("array index is out of bounds"));
    }
    Ok(index)
}

fn get<'a>(mut value: &'a Value, path: &[String], budget: &mut Budget) -> Result<&'a Value> {
    for key in path {
        budget.work(1)?;
        value = match value {
            Value::Object(document) => document
                .get(key)
                .ok_or_else(|| invalid("member is missing"))?,
            Value::Array(values) => &values[index(key, values.len(), false)?],
            _ => return Err(invalid("pointer cannot traverse this value")),
        };
    }
    Ok(value)
}

fn parent<'a>(
    mut value: &'a mut Value,
    path: &[String],
    budget: &mut Budget,
) -> Result<&'a mut Value> {
    for key in path {
        budget.work(1)?;
        value = match value {
            Value::Object(document) => document
                .get_mut(key)
                .ok_or_else(|| invalid("parent is missing"))?,
            Value::Array(values) => {
                let position = index(key, values.len(), false)?;
                &mut values[position]
            }
            _ => return Err(invalid("pointer cannot traverse this value")),
        };
    }
    Ok(value)
}

fn remove(value: &mut Value, path: &[String], budget: &mut Budget) -> Result<Value> {
    writable(path)?;
    let (key, path) = path
        .split_last()
        .ok_or_else(|| invalid("requires a member path"))?;
    match parent(value, path, budget)? {
        Value::Object(document) => document
            .remove(key)
            .ok_or_else(|| invalid("member is missing")),
        Value::Array(values) => {
            let position = index(key, values.len(), false)?;
            budget.work(values.len() - position)?;
            Ok(values.remove(position))
        }
        _ => Err(invalid("destination parent must be an object or array")),
    }
}

fn put(
    value: &mut Value,
    path: &[String],
    incoming: Value,
    adding: bool,
    budget: &mut Budget,
) -> Result<()> {
    writable(path)?;
    let (key, path) = path
        .split_last()
        .ok_or_else(|| invalid("requires a member path"))?;
    match parent(value, path, budget)? {
        Value::Object(document) => {
            if !adding && !document.contains_key(key) {
                return Err(invalid("replace member is missing"));
            }
            document.insert(key.clone(), incoming);
        }
        Value::Array(values) => {
            let position = index(key, values.len(), adding)?;
            if adding {
                budget.work(values.len() - position + 1)?;
                values.insert(position, incoming);
            } else {
                values[position] = incoming;
            }
        }
        _ => return Err(invalid("destination parent must be an object or array")),
    }
    Ok(())
}

pub(crate) fn apply(before: &Document, operations: Value) -> Result<Document> {
    let mut budget = Budget {
        bytes: crate::links::FetchBudget {
            used: 0,
            limit: 64 * 1024 * 1024,
        },
        work: 1_000_000,
    };
    budget.charge(&operations)?;
    let Value::Array(operations) = operations else {
        return Err(invalid("requires an array of operations"));
    };
    if operations.len() > 1024 {
        return Err(Error::Limit("PATCH exceeds 1024 operations".into()));
    }
    serde_json::to_writer(&mut budget.bytes, before)
        .map_err(|_| Error::Limit("PATCH cumulative values exceed 64 MiB".into()))?;
    let mut document = Value::Object(before.clone());
    for operation in operations {
        budget.work(1)?;
        let Value::Object(mut operation) = operation else {
            return Err(invalid("operation must be an object"));
        };
        let path = pointer(string(&operation, "path")?)?;
        match string(&operation, "op")? {
            "add" | "replace" => {
                let adding = string(&operation, "op")? == "add";
                let value = operation
                    .remove("value")
                    .ok_or_else(|| invalid("requires value"))?;
                put(&mut document, &path, value, adding, &mut budget)?;
            }
            "remove" => {
                remove(&mut document, &path, &mut budget)?;
            }
            "copy" | "move" => {
                let moving = string(&operation, "op")? == "move";
                let from = pointer(string(&operation, "from")?)?;
                writable(&path)?;
                if moving && path.len() > from.len() && path.starts_with(&from) {
                    return Err(invalid("cannot move a parent into its descendant"));
                }
                let value = if moving {
                    remove(&mut document, &from, &mut budget)?
                } else {
                    let value = get(&document, &from, &mut budget)?;
                    budget.charge(value)?;
                    value.clone()
                };
                put(&mut document, &path, value, true, &mut budget)?;
            }
            "test" => {
                let expected = operation
                    .get("value")
                    .ok_or_else(|| invalid("requires value"))?;
                if !crate::collections::equal(get(&document, &path, &mut budget)?, expected) {
                    return Err(invalid("test failed"));
                }
            }
            _ => {
                return Err(invalid(
                    "operation must be add, remove, replace, copy, move or test",
                ))
            }
        }
        document.validate()?;
        budget.charge(&document)?;
    }
    let Value::Object(document) = document else {
        unreachable!()
    };
    Ok(document)
}
