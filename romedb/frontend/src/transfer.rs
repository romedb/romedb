//! Versioned, lossless document transfer. Not a schema or database backup format.
use crate::{Connection, Document, Error, Key, Record, Result, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_DOCUMENTS: usize = 100_000;
#[derive(Clone, Copy, Debug)]
pub enum TransferFormat {
    Json,
    Ndjson,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    format: String,
    version: u32,
}
impl Header {
    fn new() -> Self {
        Self {
            format: "romedb.documents".into(),
            version: 1,
        }
    }
    fn check(&self) -> Result<()> {
        if self.format != "romedb.documents" || self.version != 1 {
            return Err(Error::Validation(
                "unsupported document transfer format/version".into(),
            ));
        }
        Ok(())
    }
}
#[cfg(test)]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    header: Header,
    documents: Vec<Portable>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "value", deny_unknown_fields)]
enum Portable {
    Null,
    Boolean(bool),
    Integer(#[serde(with = "decimal")] i64),
    Number(#[serde(with = "float_bits")] f64),
    String(String),
    Binary(Vec<u8>),
    Record(PortableRecord),
    Object(#[serde(deserialize_with = "unique_fields")] BTreeMap<String, Portable>),
    Array(Vec<Portable>),
    Vector(Vec<u8>),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableRecord {
    table: String,
    key: PortableKey,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "value", deny_unknown_fields)]
enum PortableKey {
    Integer(#[serde(with = "decimal")] i64),
    String(String),
}

// Serialize views of the validated value tree. Unlike Portable (the input
// representation), these wrappers do not rebuild nested arrays and objects.
struct PortableRef<'a>(&'a Value);
struct PortableArray<'a>(&'a [Value]);
struct PortableObject<'a>(&'a Document);
struct PortableKeyRef<'a>(&'a Key);
#[derive(Serialize)]
struct PortableRecordRef<'a> {
    table: &'a str,
    key: PortableKeyRef<'a>,
}
fn tagged<S: serde::Serializer>(
    serializer: S,
    tag: &str,
    value: &impl Serialize,
) -> std::result::Result<S::Ok, S::Error> {
    use serde::ser::SerializeStruct;
    let mut output = serializer.serialize_struct("Portable", 2)?;
    output.serialize_field("type", tag)?;
    output.serialize_field("value", value)?;
    output.end()
}
impl Serialize for PortableRef<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self.0 {
            Value::Null => {
                use serde::ser::SerializeStruct;
                let mut output = serializer.serialize_struct("Portable", 1)?;
                output.serialize_field("type", "Null")?;
                output.end()
            }
            Value::Boolean(value) => tagged(serializer, "Boolean", value),
            Value::Integer(value) => tagged(serializer, "Integer", &value.to_string()),
            Value::Number(value) => {
                tagged(serializer, "Number", &format!("{:016x}", value.to_bits()))
            }
            Value::String(value) => tagged(serializer, "String", value),
            Value::Binary(value) => tagged(serializer, "Binary", value),
            Value::Vector(value) => tagged(serializer, "Vector", value),
            Value::Array(value) => tagged(serializer, "Array", &PortableArray(value)),
            Value::Object(value) => tagged(serializer, "Object", &PortableObject(value)),
            Value::Record(value) => tagged(
                serializer,
                "Record",
                &PortableRecordRef {
                    table: &value.table,
                    key: PortableKeyRef(&value.key),
                },
            ),
        }
    }
}
impl Serialize for PortableArray<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(PortableRef))
    }
}
impl Serialize for PortableObject<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter().map(|(key, value)| (key, PortableRef(value))))
    }
}
impl Serialize for PortableKeyRef<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self.0 {
            Key::Integer(value) => tagged(serializer, "Integer", &value.to_string()),
            Key::String(value) => tagged(serializer, "String", value),
        }
    }
}
mod decimal {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        let text = String::deserialize(deserializer)?;
        let value = text.parse::<i64>().map_err(serde::de::Error::custom)?;
        if value.to_string() != text {
            return Err(serde::de::Error::custom("expected canonical decimal int64"));
        }
        Ok(value)
    }
}
impl From<Value> for Portable {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Boolean(v) => Self::Boolean(v),
            Value::Integer(v) => Self::Integer(v),
            Value::Number(v) => Self::Number(v),
            Value::String(v) => Self::String(v),
            Value::Binary(v) => Self::Binary(v),
            Value::Vector(v) => Self::Vector(v),
            Value::Array(v) => Self::Array(v.into_iter().map(Self::from).collect()),
            Value::Object(v) => {
                Self::Object(v.into_iter().map(|(k, v)| (k, Self::from(v))).collect())
            }
            Value::Record(v) => Self::Record(PortableRecord {
                table: v.table,
                key: match v.key {
                    Key::Integer(v) => PortableKey::Integer(v),
                    Key::String(v) => PortableKey::String(v),
                },
            }),
        }
    }
}
impl From<Portable> for Value {
    fn from(value: Portable) -> Self {
        match value {
            Portable::Null => Self::Null,
            Portable::Boolean(v) => Self::Boolean(v),
            Portable::Integer(v) => Self::Integer(v),
            Portable::Number(v) => Self::Number(v),
            Portable::String(v) => Self::String(v),
            Portable::Binary(v) => Self::Binary(v),
            Portable::Vector(v) => Self::Vector(v),
            Portable::Array(v) => Self::Array(v.into_iter().map(Self::from).collect()),
            Portable::Object(v) => {
                Self::Object(v.into_iter().map(|(k, v)| (k, Self::from(v))).collect())
            }
            Portable::Record(v) => Self::Record(Record {
                table: v.table,
                key: match v.key {
                    PortableKey::Integer(v) => Key::Integer(v),
                    PortableKey::String(v) => Key::String(v),
                },
            }),
        }
    }
}
// Bound the encoded output while serde emits it, without a temporary JSON
// string for each document. The engine and decoded current row have separate costs.
struct TransferWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}
impl std::io::Write for TransferWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::other("transfer byte limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl TransferWriter {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        use std::io::Write;
        self.write_all(bytes)
            .map_err(|_| Error::Limit("transfer byte limit exceeded".into()))
    }
    fn json(&mut self, value: &impl Serialize) -> Result<()> {
        serde_json::to_writer(&mut *self, value).map_err(|error| {
            if self.exceeded {
                Error::Limit("transfer byte limit exceeded".into())
            } else {
                error.into()
            }
        })
    }
}

impl Connection {
    /// Export one collection's documents in a versioned typed format.
    pub fn export_documents(&self, table: &str, format: TransferFormat) -> Result<String> {
        self.export_documents_bounded(table, format, MAX_BYTES, MAX_DOCUMENTS)
    }

    fn export_documents_bounded(
        &self,
        table: &str,
        format: TransferFormat,
        max_bytes: usize,
        max_documents: usize,
    ) -> Result<String> {
        self.atomic(|| {
            let collection = self.catalog(table)?;
            let mut output = TransferWriter {
                bytes: Vec::new(),
                limit: max_bytes,
                exceeded: false,
            };
            match format {
                TransferFormat::Json => {
                    output.append(b"{\"header\":")?;
                    output.json(&Header::new())?;
                    output.append(b",\"documents\":[")?;
                }
                TransferFormat::Ndjson => {
                    output.json(&Header::new())?;
                    output.append(b"\n")?;
                }
            }
            let mut count = 0;
            let mut failure = None;
            let mut statement = self.prepare(format!(
                "SELECT doc FROM {} ORDER BY id LIMIT {}",
                crate::quote(&collection.storage),
                max_documents.saturating_add(1)
            ))?;
            let execution = crate::parser_stack(|| {
                statement.run_with_row_callback(|row| {
                    let result = (|| -> Result<()> {
                        if count >= max_documents {
                            return Err(Error::Limit("transfer document limit exceeded".into()));
                        }
                        let mut values = row.get_values();
                        let value = values
                            .next()
                            .ok_or_else(|| Error::Storage("missing transfer document".into()))?;
                        let document = Value::Object(crate::decode_document(value)?);
                        if matches!(format, TransferFormat::Json) && count != 0 {
                            output.append(b",")?;
                        }
                        output.json(&PortableRef(&document))?;
                        if matches!(format, TransferFormat::Ndjson) {
                            output.append(b"\n")?;
                        }
                        count += 1;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        failure = Some(error);
                        return Err(turso_core::LimboError::Interrupt);
                    }
                    Ok(())
                })
            });
            drop(statement);
            if let Some(error) = failure {
                return Err(error);
            }
            execution?;
            if matches!(format, TransferFormat::Json) {
                output.append(b"]}")?;
            }
            String::from_utf8(output.bytes)
                .map_err(|_| Error::Storage("invalid transfer UTF-8".into()))
        })
    }
    /// Insert all transferred documents atomically into an existing collection.
    pub fn import_documents(
        &self,
        table: &str,
        input: &str,
        format: TransferFormat,
    ) -> Result<usize> {
        if input.len() > MAX_BYTES {
            return Err(Error::Limit("transfer exceeds 64 MiB".into()));
        }
        if matches!(format, TransferFormat::Ndjson) {
            // Validate every line before any mutation, without retaining a full
            // document vector. The immutable input is replayed inside the transaction.
            let mut count = 0;
            for document in ndjson_documents(input)? {
                document?;
                count += 1;
                if count > MAX_DOCUMENTS {
                    return Err(Error::Limit("transfer exceeds 100000 documents".into()));
                }
            }
            // One savepoint covers the whole import, so any failure restores
            // every document; documents need no savepoint of their own.
            return self.atomic(|| {
                let collection = self.catalog(table)?;
                for document in ndjson_documents(input)? {
                    self.insert_in_transaction(&collection, document?)?;
                }
                Ok(count)
            });
        }
        let count = json_documents(input, |_| Ok(()))?;
        self.atomic(|| {
            let collection = self.catalog(table)?;
            json_documents(input, |document| {
                self.insert_in_transaction(&collection, document)
                    .map(|_| ())
            })?;
            Ok(count)
        })
    }
}

// Deserialize the envelope and document sequence without retaining the sequence.
// The first pass has a no-op callback; only a fully validated input is replayed
// with a write callback. Envelope field order remains unrestricted.
fn json_documents(input: &str, mut visit: impl FnMut(Document) -> Result<()>) -> Result<usize> {
    use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
    struct Documents<'a, F>(&'a mut F);
    impl<'de, F: FnMut(Portable) -> std::result::Result<(), String>> DeserializeSeed<'de>
        for Documents<'_, F>
    {
        type Value = ();
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            de: D,
        ) -> std::result::Result<(), D::Error> {
            de.deserialize_seq(self)
        }
    }
    impl<'de, F: FnMut(Portable) -> std::result::Result<(), String>> Visitor<'de> for Documents<'_, F> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a document array")
        }
        fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> std::result::Result<(), S::Error> {
            while let Some(document) = seq.next_element::<Portable>()? {
                (self.0)(document).map_err(S::Error::custom)?;
            }
            Ok(())
        }
    }
    struct Envelope<'a, F>(&'a mut F);
    impl<'de, F: FnMut(Portable) -> std::result::Result<(), String>> Visitor<'de> for Envelope<'_, F> {
        type Value = Header;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a document transfer envelope")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> std::result::Result<Header, M::Error> {
            let mut header = None;
            let mut documents = false;
            while let Some(key) = map.next_key::<String>()? {
                match key.as_str() {
                    "header" => {
                        if header.is_some() {
                            return Err(M::Error::duplicate_field("header"));
                        }
                        header = Some(map.next_value::<Header>()?);
                    }
                    "documents" => {
                        if documents {
                            return Err(M::Error::duplicate_field("documents"));
                        }
                        documents = true;
                        map.next_value_seed(Documents(&mut *self.0))?;
                    }
                    _ => return Err(M::Error::unknown_field(&key, &["header", "documents"])),
                }
            }
            if !documents {
                return Err(M::Error::missing_field("documents"));
            }
            header.ok_or_else(|| M::Error::missing_field("header"))
        }
    }
    let mut count = 0;
    let mut failure = None;
    let mut consume = |portable| {
        let result = (|| {
            if count >= MAX_DOCUMENTS {
                return Err(Error::Limit("transfer exceeds 100000 documents".into()));
            }
            visit(transfer_document(portable)?)?;
            count += 1;
            Ok(())
        })();
        result.map_err(|error| {
            let message = error.to_string();
            failure = Some(error);
            message
        })
    };
    let mut deserializer = crate::wire_json::deserializer(input)?;
    let parsed = crate::parser_stack(|| {
        serde::Deserializer::deserialize_map(&mut deserializer, Envelope(&mut consume))
            .and_then(|header| deserializer.end().map(|()| header))
    });
    if let Some(error) = failure {
        return Err(error);
    }
    parsed
        .map_err(|error| Error::Validation(format!("document transfer: {error}")))?
        .check()?;
    Ok(count)
}

fn transfer_document(portable: Portable) -> Result<Document> {
    let value = Value::from(portable);
    value.validate()?;
    let Value::Object(document) = value else {
        return Err(Error::Validation(
            "transfer entries must be typed objects".into(),
        ));
    };
    Ok(document)
}

fn ndjson_documents(input: &str) -> Result<impl Iterator<Item = Result<Document>> + '_> {
    let mut lines = input.lines();
    let header: Header = serde_json::from_str(
        lines
            .next()
            .ok_or_else(|| Error::Validation("missing transfer header".into()))?,
    )
    .map_err(|error| Error::Validation(format!("document transfer: {error}")))?;
    header.check()?;
    Ok(lines.enumerate().map(|(index, line)| {
        let portable = crate::decode_wire_json(line).map_err(|error| match error {
            Error::Limit(_) => error,
            _ => Error::Validation(format!("transfer line {}: {error}", index + 2)),
        })?;
        transfer_document(portable)
    }))
}

fn unique_fields<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, Portable>, D::Error> {
    struct Fields;
    impl<'de> serde::de::Visitor<'de> for Fields {
        type Value = BTreeMap<String, Portable>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an object with unique field names")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut fields = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, Portable>()? {
                if fields.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate document field"));
                }
            }
            Ok(fields)
        }
    }
    deserializer.deserialize_map(Fields)
}

mod float_bits {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{:016x}", value.to_bits()))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        let text = String::deserialize(deserializer)?;
        let bits = u64::from_str_radix(&text, 16).map_err(serde::de::Error::custom)?;
        let value = f64::from_bits(bits);
        if text != format!("{bits:016x}") || !value.is_finite() {
            return Err(serde::de::Error::custom(
                "expected 16 lowercase hexadecimal digits encoding finite binary64",
            ));
        }
        Ok(value)
    }
}

impl Value {
    /// A single tagged value using the document transfer v1 value encoding.
    pub fn to_portable_value(&self) -> Result<serde_json::Value> {
        self.validate()?;
        Ok(serde_json::to_value(PortableRef(self))?)
    }
    /// Consume a tagged value using the document transfer v1 value encoding.
    /// Avoids cloning the owned value tree before portable conversion.
    pub fn into_portable_value(self) -> Result<serde_json::Value> {
        self.validate()?;
        Ok(serde_json::to_value(PortableRef(&self))?)
    }
    /// Consume and serialize a transfer-v1 value directly to JSON text, without
    /// constructing an intermediate serde_json value tree.
    pub fn into_portable_json(self) -> Result<String> {
        self.validate()?;
        Ok(serde_json::to_string(&PortableRef(&self))?)
    }
    /// Consume and write a transfer-v1 value to a JSON sink without an
    /// intermediate JSON string. Validation completes before writing any bytes.
    /// An I/O failure can leave a partial value in the sink; this does not flush
    /// the writer or make its output atomic.
    pub fn write_portable_json(self, writer: impl std::io::Write) -> Result<()> {
        self.validate()?;
        Ok(serde_json::to_writer(writer, &PortableRef(&self))?)
    }
    /// Decode a tagged transfer v1 value and validate its logical type.
    pub fn from_portable_value(value: serde_json::Value) -> Result<Self> {
        let value = Self::from(serde_json::from_value::<Portable>(value)?);
        value.validate()?;
        Ok(value)
    }
}

#[cfg(test)]
mod export_tests {
    use super::*;
    #[test]
    fn borrowed_portable_encoding_matches_owned_wire_at_all_supported_depths() {
        let mut values = vec![
            Value::Null,
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Integer(i64::MIN),
            Value::Integer(i64::MAX),
            Value::Number(-0.0),
            Value::Number(f64::MIN_POSITIVE),
            Value::Number(f64::from_bits(1)),
            Value::Number(f64::MAX),
            Value::String("quoted\"\\\nไทย".into()),
            Value::Binary(vec![0, 127, 255]),
            Value::Array(Vec::new()),
            Value::Object(Document::new()),
            Value::Record(Record {
                table: "docs".into(),
                key: Key::Integer(i64::MIN),
            }),
            Value::Record(Record {
                table: "docs".into(),
                key: Key::String("ไทย\"\n".into()),
            }),
            Value::vector32(&[1.0, 0.0, -1.0]).unwrap(),
            Value::vector64(&[1.0, 0.0, -1.0]).unwrap(),
            Value::vector32_sparse(&[1.0, 0.0, -1.0]).unwrap(),
            Value::vector8(&[1.0, 0.0, -1.0]).unwrap(),
            Value::vector1bit(&[1.0, 0.0, -1.0]).unwrap(),
        ];
        values.push(Value::Object(
            values
                .iter()
                .enumerate()
                .map(|(index, value)| (format!("field {index}\""), value.clone()))
                .collect(),
        ));
        let mut nested = Value::Null;
        for depth in 0..=64 {
            values.push(nested.clone());
            nested = if depth % 2 == 0 {
                Value::Array(vec![nested])
            } else {
                Value::Object([("nested".into(), nested)].into())
            };
        }
        for value in values {
            let expected = serde_json::to_string(&Portable::from(value.clone())).unwrap();
            assert_eq!(value.clone().into_portable_json().unwrap(), expected);
            let mut output = Vec::new();
            value.clone().write_portable_json(&mut output).unwrap();
            assert_eq!(output, expected.as_bytes());
            let encoded = serde_json::to_value(Portable::from(value.clone())).unwrap();
            assert_eq!(value.to_portable_value().unwrap(), encoded);
            assert_eq!(value.clone().into_portable_value().unwrap(), encoded);
            assert_eq!(Value::from_portable_value(encoded).unwrap(), value);
        }
    }
    #[test]
    fn direct_portable_json_preserves_record_shapes_and_rejects_invalid_values() {
        for key in [Key::Integer(i64::MIN), Key::String("ไทย\"\n".into())] {
            let value = Value::Record(Record {
                table: "docs".into(),
                key,
            });
            let json: serde_json::Value =
                serde_json::from_str(&value.clone().into_portable_json().unwrap()).unwrap();
            assert_eq!(json, value.to_portable_value().unwrap());
            assert_eq!(Value::from_portable_value(json).unwrap(), value);
        }
        assert_eq!(
            Value::Number(f64::NAN)
                .into_portable_json()
                .unwrap_err()
                .code(),
            "FDB_VALIDATION"
        );
        let mut value = Value::Null;
        for _ in 0..65 {
            value = Value::Array(vec![value]);
        }
        assert_eq!(value.into_portable_json().unwrap_err().code(), "FDB_LIMIT");
    }

    #[test]
    fn json_preflight_checks_late_envelope_errors_without_writes() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let params = crate::Parameters::new();
        c.execute("CREATE TABLE docs", &params).unwrap();
        c.execute("BEGIN", &params).unwrap();
        c.execute("INSERT INTO docs {id:docs:prior,n:9}", &params)
            .unwrap();
        let header = serde_json::to_string(&Header::new()).unwrap();
        let document = r#"{"type":"Object","value":{"id":{"type":"Record","value":{"table":"docs","key":{"type":"String","value":"new"}}}}}"#;
        let prefix = format!("{{\"documents\":[{document}]");
        let valid = format!("{prefix},\"header\":{header}}}");
        let before = c.engine.total_changes();
        for input in [
            format!("{prefix}}}"),
            format!("{prefix},\"header\":{{\"format\":\"romedb.documents\",\"version\":2}}}}"),
            format!("{prefix},\"header\":{header},\"header\":{header}}}"),
            format!("{prefix},\"header\":{header},\"documents\":[]}}"),
            format!("{prefix},\"header\":{header},\"unknown\":0}}"),
            format!("{valid} trailing"),
            format!("{{\"header\":{header},\"documents\":[{document},{{\"type\":\"Integer\",\"value\":\"1\"}}]}}"),
            format!("{{\"header\":{header},\"documents\":[{document},invalid]}}"),
        ] {
            assert_eq!(c.import_documents("docs", &input, TransferFormat::Json).unwrap_err().code(), "FDB_VALIDATION", "{input}");
            assert_eq!(c.engine.total_changes(), before);
            assert_eq!(c.transaction_state(), crate::TransactionState::Active);
        }
        assert_eq!(
            c.import_documents("docs", &valid, TransferFormat::Json)
                .unwrap(),
            1
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .documents,
            2
        );
        c.execute("ROLLBACK", &params).unwrap();
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .documents,
            0
        );
    }

    #[test]
    fn json_document_count_is_bounded_during_deserialization() {
        let header = serde_json::to_string(&Header::new()).unwrap();
        let document = r#"{"type":"Object","value":{}}"#;
        let documents = std::iter::repeat_n(document, MAX_DOCUMENTS)
            .collect::<Vec<_>>()
            .join(",");
        let exact = format!("{{\"header\":{header},\"documents\":[{documents}]}}");
        assert_eq!(json_documents(&exact, |_| Ok(())).unwrap(), MAX_DOCUMENTS);
        let excess = format!("{{\"header\":{header},\"documents\":[{documents},{document}]}}");
        let mut visited = 0;
        assert_eq!(
            json_documents(&excess, |_| {
                visited += 1;
                Ok(())
            })
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        assert_eq!(visited, MAX_DOCUMENTS);
    }

    #[test]
    fn ndjson_preflight_rejects_late_invalid_entries_before_writes() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE docs", &crate::Parameters::new())
            .unwrap();
        c.execute(
            "INSERT INTO docs {id:docs:first,n:1}",
            &crate::Parameters::new(),
        )
        .unwrap();
        let payload = c.export_documents("docs", TransferFormat::Ndjson).unwrap();
        c.execute("DELETE FROM docs", &crate::Parameters::new())
            .unwrap();
        c.execute("BEGIN", &crate::Parameters::new()).unwrap();
        c.execute(
            "INSERT INTO docs {id:docs:prior,n:9}",
            &crate::Parameters::new(),
        )
        .unwrap();
        let before = c.engine.total_changes();
        for suffix in [
            "invalid",
            "{\"type\":\"Integer\",\"value\":\"1\"}",
            "{\"type\":\"Object\",\"value\":{\"n\":{\"type\":\"Integer\",\"value\":\"01\"}}}",
        ] {
            assert!(c
                .import_documents(
                    "docs",
                    &format!("{payload}{suffix}\n"),
                    TransferFormat::Ndjson
                )
                .is_err());
            assert_eq!(c.engine.total_changes(), before);
            assert_eq!(c.transaction_state(), crate::TransactionState::Active);
            assert_eq!(
                c.check_collection_integrity("docs", Default::default())
                    .unwrap()
                    .documents,
                1
            );
        }
        assert_eq!(
            c.import_documents("docs", &payload, TransferFormat::Ndjson)
                .unwrap(),
            1
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .documents,
            2
        );
        c.execute("ROLLBACK", &crate::Parameters::new()).unwrap();
    }

    #[test]
    fn incremental_export_preserves_format_and_enforces_exact_limits() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE docs", &crate::Parameters::new())
            .unwrap();
        c.execute("BEGIN", &crate::Parameters::new()).unwrap();
        c.execute(
            "INSERT INTO docs {id:docs:a,text:'ไทย',flag:true};",
            &crate::Parameters::new(),
        )
        .unwrap();
        c.execute(
            "INSERT INTO docs {id:docs:b,text:'line',flag:false};",
            &crate::Parameters::new(),
        )
        .unwrap();
        let docs = ["a", "b"]
            .into_iter()
            .map(|key| {
                let record = Record {
                    table: "docs".into(),
                    key: Key::String(key.into()),
                };
                Portable::from(Value::Object(c.get(&record).unwrap().unwrap()))
            })
            .collect::<Vec<_>>();
        let expected_json = serde_json::to_string(&Bundle {
            header: Header::new(),
            documents: docs,
        })
        .unwrap();
        for format in [TransferFormat::Json, TransferFormat::Ndjson] {
            let output = c.export_documents("docs", format).unwrap();
            if matches!(format, TransferFormat::Json) {
                assert_eq!(output, expected_json);
            }
            assert_eq!(
                c.export_documents_bounded("docs", format, output.len(), 2)
                    .unwrap(),
                output
            );
            for bytes in [0, 1, output.len() - 1] {
                assert_eq!(
                    c.export_documents_bounded("docs", format, bytes, 2)
                        .unwrap_err()
                        .code(),
                    "FDB_LIMIT"
                );
                assert_eq!(c.transaction_state(), crate::TransactionState::Active);
            }
            assert_eq!(
                c.export_documents_bounded("docs", format, MAX_BYTES, 1)
                    .unwrap_err()
                    .code(),
                "FDB_LIMIT"
            );
            assert_eq!(
                c.check_collection_integrity("docs", Default::default())
                    .unwrap()
                    .documents,
                2
            );
            let other = crate::Database::open(":memory:").unwrap();
            let other = other.connect().unwrap();
            other
                .execute("CREATE TABLE docs", &crate::Parameters::new())
                .unwrap();
            assert_eq!(other.import_documents("docs", &output, format).unwrap(), 2);
            assert_eq!(other.export_documents("docs", format).unwrap(), output);
        }
        c.execute("ROLLBACK", &crate::Parameters::new()).unwrap();
    }
}
