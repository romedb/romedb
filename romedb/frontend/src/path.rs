//! A bounded JSON-style path grammar over typed values, with no JSON guessing.
use crate::{Error, Result, Value};
enum Segment {
    Key(String),
    Index(usize),
}
fn invalid() -> Error {
    Error::Validation(
        "invalid document path; use object keys and non-negative array positions".into(),
    )
}
fn quoted(input: &str, start: usize) -> Result<(String, usize)> {
    let bytes = input.as_bytes();
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            return Ok((
                serde_json::from_str(&input[start..=i]).map_err(|_| invalid())?,
                i + 1,
            ));
        }
        i += 1;
    }
    Err(invalid())
}
fn parse(input: &str) -> Result<Vec<Segment>> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'$') {
        return Err(invalid());
    }
    let mut i = 1;
    let mut out = Vec::new();
    while i < bytes.len() {
        if out.len() >= 64 {
            return Err(Error::Limit("document path nesting exceeds 64".into()));
        }
        match bytes[i] {
            b'.' => {
                i += 1;
                if bytes.get(i) == Some(&b'"') {
                    let (key, end) = quoted(input, i)?;
                    out.push(Segment::Key(key));
                    i = end;
                } else {
                    let start = i;
                    while i < bytes.len() && !matches!(bytes[i], b'.' | b'[') {
                        i += 1;
                    }
                    let key = &input[start..i];
                    if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        return Err(invalid());
                    }
                    out.push(Segment::Key(key.into()));
                }
            }
            b'[' => {
                i += 1;
                if bytes.get(i) == Some(&b'"') {
                    let (key, end) = quoted(input, i)?;
                    out.push(Segment::Key(key));
                    i = end;
                } else {
                    let start = i;
                    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                        i += 1;
                    }
                    if start == i {
                        return Err(invalid());
                    }
                    out.push(Segment::Index(
                        input[start..i].parse().map_err(|_| invalid())?,
                    ));
                }
                if bytes.get(i) != Some(&b']') {
                    return Err(invalid());
                }
                i += 1;
            }
            _ => return Err(invalid()),
        }
    }
    Ok(out)
}
pub(crate) fn get<'a>(value: &'a Value, path: &str) -> Result<Option<&'a Value>> {
    let mut current = value;
    for segment in parse(path)? {
        current = match (current, segment) {
            (Value::Object(object), Segment::Key(key)) => match object.get(&key) {
                Some(v) => v,
                None => return Ok(None),
            },
            (Value::Array(array), Segment::Index(i)) => match array.get(i) {
                Some(v) => v,
                None => return Ok(None),
            },
            _ => return Ok(None),
        };
    }
    Ok(Some(current))
}
