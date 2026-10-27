use crate::{Error, Result};
pub use romeql_parser::{Key, Record};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Document = BTreeMap<String, Value>;
pub type Parameters = BTreeMap<String, Value>;

/// Tagged values prevent collisions with user objects and preserve scalar types.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Value {
    Null,
    Boolean(bool),
    Integer(i64),
    Number(f64),
    String(String),
    Binary(Vec<u8>),
    Record(Record),
    Object(Document),
    Array(Vec<Value>),
    Vector(Vec<u8>),
}
impl Value {
    pub fn validate(&self) -> Result<()> {
        self.validate_at(0)
    }
    fn validate_at(&self, depth: usize) -> Result<()> {
        if depth > 64 {
            return Err(Error::Limit("value nesting exceeds 64".into()));
        }
        match self {
            Self::Number(n) if !n.is_finite() => Err(Error::Validation("non-finite number".into())),
            Self::Record(r) => validate_record(r),
            Self::Vector(bytes) => crate::vectors::dimensions(bytes).map(|_| ()),
            Self::Object(fields) => {
                for value in fields.values() {
                    value.validate_at(depth + 1)?;
                }
                Ok(())
            }
            Self::Array(values) => {
                for value in values {
                    value.validate_at(depth + 1)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = b"FDB\x01".to_vec();
        serde_json::to_writer(&mut bytes, self)?;
        Ok(bytes)
    }
    /// Check the complete tagged encoding before allocating its byte buffer.
    pub(crate) fn encode_with_limit(&self, max_bytes: Option<usize>) -> Result<Vec<u8>> {
        if let Some(limit) = max_bytes {
            self.validate()?;
            struct Counter {
                used: usize,
                limit: usize,
            }
            impl std::io::Write for Counter {
                fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                    if bytes.len() > self.limit.saturating_sub(self.used) {
                        return Err(std::io::Error::other("encoded value limit exceeded"));
                    }
                    self.used += bytes.len();
                    Ok(bytes.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let mut counter = Counter { used: 0, limit };
            std::io::Write::write_all(&mut counter, b"FDB\x01")
                .map_err(|_| Error::Limit("encoded value limit exceeded".into()))?;
            serde_json::to_writer(&mut counter, self)
                .map_err(|_| Error::Limit("encoded value limit exceeded".into()))?;
        }
        self.encode()
    }
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let payload = bytes
            .strip_prefix(b"FDB\x01")
            .ok_or_else(|| Error::Storage("unknown value format".into()))?;
        let input = std::str::from_utf8(payload)
            .map_err(|error| Error::Storage(format!("invalid value UTF-8: {error}")))?;
        let value: Self = crate::decode_wire_json(input)?;
        value.validate()?;
        Ok(value)
    }
}
pub(crate) fn validate_document_value(document: &Document) -> Result<()> {
    for value in document.values() {
        value.validate_at(1)?;
    }
    Ok(())
}

pub(crate) fn encode_document(document: &Document) -> Result<Vec<u8>> {
    validate_document_value(document)?;
    #[derive(Serialize)]
    #[serde(tag = "type", content = "value")]
    enum BorrowedValue<'a> {
        Object(&'a Document),
    }
    let mut bytes = b"FDB\x01".to_vec();
    serde_json::to_writer(&mut bytes, &BorrowedValue::Object(document))?;
    Ok(bytes)
}

pub(crate) fn record_from_text(text: &str) -> Option<Record> {
    let (table, key) = text.split_once(':')?;
    if !table.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        || !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let digits = key.strip_prefix('-').unwrap_or(key);
    let key = if let Some(quoted) = key
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix('`'))
    {
        if quoted.replace("``", "").contains('`') {
            return None;
        }
        Key::String(quoted.replace("``", "`"))
    } else if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        Key::Integer(key.parse().ok()?)
    } else if key.trim() != key || key.chars().any(char::is_control) {
        return None;
    } else {
        Key::String(key.into())
    };
    let record = Record {
        table: table.to_ascii_lowercase(),
        key,
    };
    validate_record(&record).ok()?;
    Some(record)
}

pub(crate) fn validate_record(r: &Record) -> Result<()> {
    if r.table.is_empty() || matches!(&r.key, Key::String(s) if s.is_empty()) {
        return Err(Error::Validation(
            "record target and string key must be nonempty".into(),
        ));
    }
    // Use the same logical target-name rules even when the reference is nested
    // or not indexed; indexed writes already pass through canonical().
    crate::canonical(&r.table)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_encoding_preserves_bytes_and_counts_header_and_escapes() {
        for value in [
            Value::Null,
            Value::Record(Record {
                table: "docs".into(),
                key: Key::String("\"\\é".into()),
            }),
            Value::Binary(vec![0, 255]),
        ] {
            let expected = value.encode().unwrap();
            assert_eq!(value.encode_with_limit(None).unwrap(), expected);
            assert_eq!(
                value.encode_with_limit(Some(expected.len())).unwrap(),
                expected
            );
            for limit in [0, 3, expected.len() - 1] {
                assert_eq!(
                    value.encode_with_limit(Some(limit)).unwrap_err().code(),
                    "FDB_LIMIT"
                );
            }
        }
    }

    #[test]
    fn stored_numbers_round_trip_binary64_bits() {
        let mut bits = 0x1234_5678_9abc_def0u64;
        let edges = [
            0,
            1,
            0x8000_0000_0000_0000,
            0x000f_ffff_ffff_ffff,
            0x0010_0000_0000_0000,
            0x7fef_ffff_ffff_ffff,
            0xffef_ffff_ffff_ffff,
        ];
        for candidate in edges.into_iter().chain((0..4096).map(|_| {
            bits ^= bits << 13;
            bits ^= bits >> 7;
            bits ^= bits << 17;
            bits
        })) {
            let number = f64::from_bits(candidate);
            if !number.is_finite() {
                continue;
            }
            let encoded = Value::Number(number).encode().unwrap();
            let Value::Number(actual) = Value::decode(&encoded).unwrap() else {
                panic!("number type lost")
            };
            assert_eq!(
                actual.to_bits(),
                candidate,
                "encoding {}",
                String::from_utf8_lossy(&encoded)
            );
        }
    }
}

#[cfg(test)]
mod borrowed_document_tests {
    use super::*;

    #[test]
    fn borrowed_encoding_preserves_storage_bytes_and_validation() {
        for vector in [
            Value::vector32(&[1.0, 0.0, -1.0]).unwrap(),
            Value::vector64(&[1.0, 0.0, -1.0]).unwrap(),
            Value::vector32_sparse(&[1.0, 0.0, -1.0]).unwrap(),
            Value::vector8(&[1.0, 0.0, -1.0]).unwrap(),
            Value::vector1bit(&[1.0, 0.0, -1.0]).unwrap(),
        ] {
            for key in [Key::Integer(i64::MIN), Key::String("ไทย:\"\n".into())] {
                let document = Document::from([
                    (
                        "id".into(),
                        Value::Record(Record {
                            table: "docs".into(),
                            key,
                        }),
                    ),
                    ("vector".into(), vector.clone()),
                    (
                        "nested".into(),
                        Value::Object(Document::from([(
                            "escaped\"\n".into(),
                            Value::String("\0\t\\".into()),
                        )])),
                    ),
                ]);
                let bytes = encode_document(&document).unwrap();
                let mut previous_encoding = b"FDB\x01".to_vec();
                previous_encoding
                    .extend(serde_json::to_vec(&Value::Object(document.clone())).unwrap());
                assert_eq!(bytes, previous_encoding);
                assert_eq!(bytes, Value::Object(document.clone()).encode().unwrap());
                assert_eq!(Value::decode(&bytes).unwrap(), Value::Object(document));
            }
        }
        let document = Document::from([
            (
                "id".into(),
                Value::Record(Record {
                    table: "docs".into(),
                    key: Key::Integer(i64::MAX),
                }),
            ),
            (
                "é".into(),
                Value::Array(vec![
                    Value::Null,
                    Value::Boolean(true),
                    Value::Integer(i64::MIN),
                    Value::Number(-0.0),
                    Value::String("ไทย".into()),
                    Value::Binary(vec![0, 255]),
                    Value::Object(Document::new()),
                ]),
            ),
        ]);
        let bytes = encode_document(&document).unwrap();
        assert_eq!(bytes, Value::Object(document.clone()).encode().unwrap());
        assert_eq!(Value::decode(&bytes).unwrap(), Value::Object(document));
        for value in [
            Value::Number(f64::NAN),
            Value::Vector(vec![]),
            Value::Record(Record {
                table: String::new(),
                key: Key::Integer(1),
            }),
        ] {
            let document = Document::from([("v".into(), value)]);
            assert_eq!(
                encode_document(&document).unwrap_err().code(),
                Value::Object(document.clone()).encode().unwrap_err().code()
            );
        }
        let mut value = Value::Null;
        for depth in 0..66 {
            let document = Document::from([("v".into(), value.clone())]);
            let borrowed = encode_document(&document);
            let owned = Value::Object(document).encode();
            assert_eq!(borrowed.is_ok(), owned.is_ok(), "depth {depth}");
            if let (Ok(a), Ok(b)) = (borrowed, owned) {
                assert_eq!(a, b);
            }
            value = Value::Array(vec![value]);
        }
    }
}

/// A field read from an encoded document.
#[derive(Debug, PartialEq)]
pub(crate) enum Field {
    Found(Value),
    /// The path is missing or crosses a non-object parent.
    Missing,
    /// The encoded value is not an object.
    RootNotObject,
}

/// Read one nonempty field path from an encoded document, deserializing only
/// that field. Encodings outside the encoder's layout (another key order,
/// nesting beyond the parser's recursion limit) fall back to a full decode,
/// so the result always equals reading the fully decoded document.
/// Decode common canonical values without the JSON decoder: scalars and
/// GeoJSON-style points with two number coordinates. Other values return `None`.
pub(crate) fn decode_simple(bytes: &[u8]) -> Option<Value> {
    let text = bytes.strip_prefix(b"FDB\x01")?;
    let value = field::scalar(text).or_else(|| field::point(text))?;
    value.validate().ok().map(|()| value)
}

/// A field's stored encoding, for accessors that return encoded values.
pub(crate) enum EncodedField {
    Found(Vec<u8>),
    Missing,
    RootNotObject,
}

/// The field's bytes as stored, which are the canonical encoding of its value,
/// without decoding the value. Falls back to decoding for any other layout.
pub(crate) fn encoded_field(bytes: &[u8], path: &[String]) -> Result<EncodedField> {
    if let Some(found) = field::read_encoded(bytes, path) {
        return Ok(found);
    }
    Ok(match decode_field(bytes, path)? {
        Field::Found(value) => EncodedField::Found(value.encode()?),
        Field::Missing => EncodedField::Missing,
        Field::RootNotObject => EncodedField::RootNotObject,
    })
}

pub(crate) fn decode_field(bytes: &[u8], path: &[String]) -> Result<Field> {
    if let Some(found) = field::read(bytes, path) {
        return Ok(found);
    }
    let Value::Object(document) = Value::decode(bytes)? else {
        return Ok(Field::RootNotObject);
    };
    match crate::path_value(&document, path) {
        Ok(Some(found)) => Ok(Field::Found(found.clone())),
        Ok(None) | Err(Error::Validation(_)) => Ok(Field::Missing),
        Err(error) => Err(error),
    }
}

mod field {
    use super::{EncodedField, Field, Value};
    use std::cmp::Ordering;

    /// Stored documents are canonical serde_json output: no whitespace, every
    /// value tagged as `{"type":"Kind"}` or `{"type":"Kind","value":...}`, and
    /// object members written once in key order. The reader walks that layout
    /// byte by byte, stops when the member is found or a later key shows it is
    /// absent, and decodes only the member. Any other layout returns `None`, and
    /// the caller decodes the whole document instead.
    pub(super) fn read(bytes: &[u8], path: &[String]) -> Option<Field> {
        match lookup(bytes, path)? {
            Lookup::Found(text) => {
                let value = match scalar(text) {
                    Some(value) => value,
                    None => crate::decode_wire_json(std::str::from_utf8(text).ok()?).ok()?,
                };
                value.validate().ok().map(|()| Field::Found(value))
            }
            Lookup::Missing => Some(Field::Missing),
            Lookup::RootNotObject => None,
        }
    }

    /// Stored values were validated when written, so their bytes are returned
    /// with the encoding prefix and are not checked again.
    pub(super) fn read_encoded(bytes: &[u8], path: &[String]) -> Option<EncodedField> {
        match lookup(bytes, path)? {
            Lookup::Found(text) => {
                let mut encoded = Vec::with_capacity(text.len() + 4);
                encoded.extend_from_slice(b"FDB\x01");
                encoded.extend_from_slice(text);
                Some(EncodedField::Found(encoded))
            }
            Lookup::Missing => Some(EncodedField::Missing),
            Lookup::RootNotObject => None,
        }
    }

    enum Lookup<'a> {
        /// The member's tagged JSON text.
        Found(&'a [u8]),
        Missing,
        RootNotObject,
    }

    const OBJECT: &[u8] = b"{\"type\":\"Object\",\"value\":{";
    const TAG: &[u8] = b"{\"type\":\"";

    fn lookup<'a>(bytes: &'a [u8], path: &[String]) -> Option<Lookup<'a>> {
        let payload = bytes.strip_prefix(b"FDB\x01")?;
        if path.is_empty() {
            return None;
        }
        let mut at = 0;
        for (depth, step) in path.iter().enumerate() {
            if !payload[at..].starts_with(OBJECT) {
                // A tagged value of another kind has no members.
                payload[at..].starts_with(TAG).then_some(())?;
                return Some(if depth == 0 {
                    Lookup::RootNotObject
                } else {
                    Lookup::Missing
                });
            }
            at += OBJECT.len();
            loop {
                match *payload.get(at)? {
                    b'}' => return Some(Lookup::Missing),
                    b'"' => {}
                    _ => return None,
                }
                let key_end = string_end(payload, at)?;
                let order = compare_key(&payload[at + 1..key_end - 1], step)?;
                if payload.get(key_end) != Some(&b':') {
                    return None;
                }
                at = key_end + 1;
                match order {
                    Ordering::Less => {
                        at = value_end(payload, at)?;
                        match *payload.get(at)? {
                            b',' => at += 1,
                            b'}' => return Some(Lookup::Missing),
                            _ => return None,
                        }
                    }
                    Ordering::Equal => break,
                    Ordering::Greater => return Some(Lookup::Missing),
                }
            }
        }
        let end = value_end(payload, at)?;
        payload[at..].starts_with(TAG).then_some(())?;
        Some(Lookup::Found(&payload[at..end]))
    }

    /// `{"type":"Point","coordinates":[x,y]}` with number coordinates.
    pub(super) fn point(text: &[u8]) -> Option<Value> {
        let rest = text.strip_prefix(
            b"{\"type\":\"Object\",\"value\":{\"coordinates\":{\"type\":\"Array\",\"value\":[",
        )?;
        let rest = rest.strip_suffix(b"]},\"type\":{\"type\":\"String\",\"value\":\"Point\"}}}")?;
        let split = rest.windows(3).position(|w| w == b"},{")?;
        let (x, y) = (&rest[..split + 1], &rest[split + 2..]);
        let number = |member: &[u8]| match scalar(member)? {
            number @ Value::Number(_) => Some(number),
            _ => None,
        };
        Some(Value::Object(super::Document::from([
            (
                "coordinates".into(),
                Value::Array(vec![number(x)?, number(y)?]),
            ),
            ("type".into(), Value::String("Point".into())),
        ])))
    }

    /// Null, boolean, integer, number and escape-free string members, parsed
    /// without the JSON decoder. Anything else returns `None` and is decoded.
    pub(super) fn scalar(text: &[u8]) -> Option<Value> {
        if text == b"{\"type\":\"Null\"}" {
            return Some(Value::Null);
        }
        let rest = text.strip_prefix(b"{\"type\":\"")?;
        let kind_end = rest.iter().position(|b| *b == b'"')?;
        let (kind, rest) = rest.split_at(kind_end);
        let raw = rest.strip_prefix(b"\",\"value\":")?.strip_suffix(b"}")?;
        match kind {
            b"Boolean" => match raw {
                b"true" => Some(Value::Boolean(true)),
                b"false" => Some(Value::Boolean(false)),
                _ => None,
            },
            b"Integer" => std::str::from_utf8(raw)
                .ok()?
                .parse()
                .ok()
                .map(Value::Integer),
            // serde_json writes the shortest round-trip form, which Rust parses
            // back to the same value.
            b"Number" => {
                let digits = std::str::from_utf8(raw).ok()?;
                digits
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E'))
                    .then(|| digits.parse().ok().map(Value::Number))?
            }
            b"String" => {
                let inner = raw.strip_prefix(b"\"")?.strip_suffix(b"\"")?;
                if inner.iter().any(|b| matches!(b, b'"' | b'\\') || *b < 0x20) {
                    return None;
                }
                std::str::from_utf8(inner)
                    .ok()
                    .map(|s| Value::String(s.to_owned()))
            }
            _ => None,
        }
    }

    /// Order of an encoded key relative to a path step, as in the sorted map.
    fn compare_key(raw: &[u8], step: &str) -> Option<Ordering> {
        if !raw.contains(&b'\\') {
            return Some(raw.cmp(step.as_bytes()));
        }
        let mut quoted = Vec::with_capacity(raw.len() + 2);
        quoted.push(b'"');
        quoted.extend_from_slice(raw);
        quoted.push(b'"');
        let key: String = serde_json::from_slice(&quoted).ok()?;
        Some(key.as_bytes().cmp(step.as_bytes()))
    }

    /// Position after the string that starts at `at`.
    fn string_end(bytes: &[u8], mut at: usize) -> Option<usize> {
        at += 1;
        loop {
            match *bytes.get(at)? {
                b'"' => return Some(at + 1),
                b'\\' => at += 2,
                _ => at += 1,
            }
        }
    }

    /// Position after the JSON value that starts at `at`.
    fn value_end(bytes: &[u8], mut at: usize) -> Option<usize> {
        match *bytes.get(at)? {
            b'"' => string_end(bytes, at),
            b'{' | b'[' => {
                let mut depth = 0usize;
                loop {
                    match *bytes.get(at)? {
                        b'"' => {
                            at = string_end(bytes, at)?;
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(at + 1);
                            }
                        }
                        _ => {}
                    }
                    at += 1;
                }
            }
            _ => {
                while !matches!(bytes.get(at), None | Some(b',' | b'}' | b']')) {
                    at += 1;
                }
                Some(at)
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::{decode_field, Field, Value};
        use crate::Document;

        fn sample() -> Value {
            Value::Object(Document::from([
                ("a".into(), Value::Integer(1)),
                ("name".into(), Value::String("quote \" and \\ slash".into())),
                (
                    "nested".into(),
                    Value::Object(Document::from([
                        (
                            "deep".into(),
                            Value::Array(vec![
                                Value::Number(1.5),
                                Value::Null,
                                Value::Boolean(true),
                            ]),
                        ),
                        ("bin".into(), Value::Binary(vec![0, 1, 255])),
                    ])),
                ),
                (
                    "rec".into(),
                    Value::Record(crate::Record {
                        table: "t".into(),
                        key: crate::Key::String("k".into()),
                    }),
                ),
                ("nothing".into(), Value::Null),
                ("vec".into(), Value::vector32(&[1.0, 2.0]).unwrap()),
                ("with \"escape\"".into(), Value::Integer(7)),
            ]))
        }

        #[test]
        fn simple_values_decode_like_the_json_decoder() {
            let point = |x: f64, y: f64| {
                Value::Object(Document::from([
                    ("type".into(), Value::String("Point".into())),
                    (
                        "coordinates".into(),
                        Value::Array(vec![Value::Number(x), Value::Number(y)]),
                    ),
                ]))
            };
            for value in [
                point(106.7, 10.8),
                point(-180.0, -90.0),
                point(-0.0, 5e-324),
                point(1.5e-7, 89.99999999999999),
                Value::Integer(-3),
                Value::String("day".into()),
            ] {
                let bytes = value.encode().unwrap();
                assert_eq!(
                    super::super::decode_simple(&bytes),
                    Some(Value::decode(&bytes).unwrap()),
                    "{value:?}"
                );
            }
            for value in [
                Value::Object(Document::from([
                    ("type".into(), Value::String("Point".into())),
                    (
                        "coordinates".into(),
                        Value::Array(vec![Value::Integer(1), Value::Number(2.0)]),
                    ),
                ])),
                Value::Object(Document::from([
                    ("type".into(), Value::String("Point".into())),
                    ("coordinates".into(), Value::Array(vec![Value::Number(1.0)])),
                    ("crs".into(), Value::Null),
                ])),
                Value::String("tab\t".into()),
                Value::Array(vec![Value::Number(1.0), Value::Number(2.0)]),
            ] {
                assert_eq!(
                    super::super::decode_simple(&value.encode().unwrap()),
                    None,
                    "{value:?}"
                );
            }
        }

        #[test]
        fn scalar_members_parse_like_the_json_decoder() {
            let values = [
                Value::Null,
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Integer(0),
                Value::Integer(i64::MIN),
                Value::Integer(i64::MAX),
                Value::Number(0.0),
                Value::Number(-0.0),
                Value::Number(0.1),
                Value::Number(-1.5e-7),
                Value::Number(f64::MIN_POSITIVE),
                Value::Number(5e-324),
                Value::Number(f64::MAX),
                Value::Number(123456789.0),
                Value::String(String::new()),
                Value::String("day".into()),
                Value::String("ภาษาไทย 🚀".into()),
                Value::String("quote \" slash \\ tab \t nul \0".into()),
            ];
            for value in values {
                let document = Value::Object(Document::from([("a".into(), value.clone())]));
                let bytes = document.encode().unwrap();
                let Some(Field::Found(found)) = super::read(&bytes, &["a".into()]) else {
                    panic!("{value:?}")
                };
                match (&found, &value) {
                    (Value::Number(a), Value::Number(b)) => assert_eq!(a.to_bits(), b.to_bits()),
                    _ => assert_eq!(found, value),
                }
                let member = &bytes
                    [bytes.windows(5).position(|w| w == b"\"a\":{").unwrap() + 4..bytes.len() - 2];
                let direct = super::scalar(member);
                let escaped =
                    matches!(&value, Value::String(s) if s.contains(['"', '\\', '\t', '\0']));
                assert_eq!(direct.is_none(), escaped, "{value:?}");
            }
        }

        #[test]
        fn field_reads_match_full_decoding() {
            let value = sample();
            let bytes = value.encode().unwrap();
            let Value::Object(document) = &value else {
                panic!()
            };
            let paths: Vec<Vec<&str>> = vec![
                vec!["a"],
                vec!["name"],
                vec!["nested"],
                vec!["nested", "deep"],
                vec!["nested", "bin"],
                vec!["rec"],
                vec!["nothing"],
                vec!["vec"],
                vec!["with \"escape\""],
                vec!["missing"],
                vec!["a", "below_scalar"],
                vec!["nested", "missing"],
                vec!["nested", "deep", "x"],
                vec!["nothing", "x"],
            ];
            for path in paths {
                let path = path.into_iter().map(String::from).collect::<Vec<_>>();
                let expected = match crate::path_value(document, &path) {
                    Ok(Some(found)) => Field::Found(found.clone()),
                    _ => Field::Missing,
                };
                assert_eq!(super::read(&bytes, &path), Some(expected), "{path:?}");
                let expected = match crate::path_value(document, &path) {
                    Ok(Some(found)) => Field::Found(found.clone()),
                    _ => Field::Missing,
                };
                assert_eq!(decode_field(&bytes, &path).unwrap(), expected, "{path:?}");
                let encoded = match crate::path_value(document, &path) {
                    Ok(Some(found)) => Some(found.encode().unwrap()),
                    _ => None,
                };
                match super::super::encoded_field(&bytes, &path).unwrap() {
                    super::super::EncodedField::Found(found) => {
                        assert_eq!(Some(found), encoded, "{path:?}")
                    }
                    super::super::EncodedField::Missing => assert_eq!(encoded, None, "{path:?}"),
                    super::super::EncodedField::RootNotObject => panic!("{path:?}"),
                }
            }
            // Keys after the requested one in the sorted encoding end the search.
            for key in ["0", "b", "nestec", "nestee", "zzz", ""] {
                assert_eq!(
                    super::read(&bytes, &[key.into()]),
                    Some(Field::Missing),
                    "{key}"
                );
            }
        }

        #[test]
        fn noncanonical_encodings_fall_back_to_full_decoding() {
            let reordered = b"FDB\x01{\"value\":{\"a\":{\"type\":\"Integer\",\"value\":3}},\"type\":\"Object\"}";
            assert_eq!(super::read(reordered, &["a".into()]), None);
            assert_eq!(
                decode_field(reordered, &["a".into()]).unwrap(),
                Field::Found(Value::Integer(3))
            );
            let array_root = Value::Array(vec![Value::Integer(1)]).encode().unwrap();
            assert_eq!(super::read(&array_root, &["a".into()]), None);
            assert_eq!(
                decode_field(&array_root, &["a".into()]).unwrap(),
                Field::RootNotObject
            );
            assert!(decode_field(
                b"FDB\x01{\"type\":\"Object\",\"value\":{\"a\":",
                &["a".into()]
            )
            .is_err());
        }
    }
}
