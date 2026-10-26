use romedb::{Database, Parameters, TransferFormat, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
#[test]
fn portable_formats_preserve_values_across_databases() {
    let a = Database::open(":memory:").unwrap();
    let a = a.connect().unwrap();
    q(&a, "CREATE TABLE docs");
    let params = Parameters::from([
        ("$blob".into(), Value::Binary(vec![0, 255])),
        ("$number".into(), Value::Number(-0.0)),
        ("$vector".into(), Value::vector32(&[1.0, 2.0]).unwrap()),
    ]);
    a.execute("INSERT INTO docs {id:type::record('docs',9223372036854775807),large:9223372036854775807,small:-9223372036854775808,text:'docs:p1',yes:true,missing:null,nested:{type:'Integer',value:'not a tag'},bytes:$blob,number:$number,vector:$vector}",&params).unwrap();
    for format in [TransferFormat::Json, TransferFormat::Ndjson] {
        let data = a.export_documents("docs", format).unwrap();
        assert!(data.contains("\"9223372036854775807\""));
        assert!(data.contains("8000000000000000"));
        let b = Database::open(":memory:").unwrap();
        let b = b.connect().unwrap();
        q(&b, "CREATE TABLE docs");
        assert_eq!(b.import_documents("docs", &data, format).unwrap(), 1);
        assert_eq!(
            q(&a, "SELECT * FROM docs").rows,
            q(&b, "SELECT * FROM docs").rows
        );
    }
}
#[test]
fn import_failures_roll_back_documents_and_indexes() {
    let a = Database::open(":memory:").unwrap();
    let a = a.connect().unwrap();
    q(&a, "CREATE TABLE docs");
    q(&a, "INSERT INTO docs {id:docs:a,name:'same'}");
    q(&a, "INSERT INTO docs {id:docs:b,name:'same'}");
    let data = a.export_documents("docs", TransferFormat::Ndjson).unwrap();
    let b = Database::open(":memory:").unwrap();
    let b = b.connect().unwrap();
    q(&b, "CREATE TABLE docs");
    q(&b, "CREATE UNIQUE INDEX names ON docs(name)");
    assert!(b
        .import_documents("docs", &data, TransferFormat::Ndjson)
        .is_err());
    assert!(q(&b, "SELECT * FROM docs").rows.is_empty());
    assert!(b
        .lookup_index("docs", "names", &Value::String("same".into()))
        .unwrap()
        .is_empty());
    let invalid = format!("{data}not json\n");
    assert!(b
        .import_documents("docs", &invalid, TransferFormat::Ndjson)
        .is_err());
    let unknown = data.replacen("\"version\":1", "\"version\":999", 1);
    assert!(b
        .import_documents("docs", &unknown, TransferFormat::Ndjson)
        .is_err());
    assert!(q(&b, "SELECT * FROM docs").rows.is_empty());
}

#[test]
fn ambiguous_fields_and_noncanonical_integers_are_rejected() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    for fields in [
        r#""x":{"type":"Integer","value":9007199254740993}"#,
        r#""x":{"type":"Integer","value":"01"}"#,
        r#""x":{"type":"Number","value":"7ff0000000000000"}"#,
        r#""x":{"type":"Null"},"x":{"type":"Null"}"#,
    ] {
        let input=format!("{{\"format\":\"romedb.documents\",\"version\":1}}\n{{\"type\":\"Object\",\"value\":{{{fields}}}}}\n");
        assert!(c
            .import_documents("docs", &input, TransferFormat::Ndjson)
            .is_err());
    }
    assert!(q(&c, "SELECT * FROM docs").rows.is_empty());
}

#[test]
fn json_replay_preserves_constraint_errors_and_outer_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {id:docs:a,name:'same'}");
    q(&c, "INSERT INTO docs {id:docs:b,name:'same'}");
    let payload = c.export_documents("docs", TransferFormat::Json).unwrap();
    q(&c, "DELETE FROM docs");
    q(&c, "CREATE UNIQUE INDEX names ON docs(name)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {id:docs:prior,name:'prior'}");
    assert_eq!(
        c.import_documents("docs", &payload, TransferFormat::Json)
            .unwrap_err()
            .code(),
        "FDB_CONSTRAINT"
    );
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .documents,
        1
    );
    assert!(c
        .lookup_index("docs", "names", &Value::String("same".into()))
        .unwrap()
        .is_empty());
    let retry = payload.replacen("same", "different", 1);
    assert_eq!(
        c.import_documents("docs", &retry, TransferFormat::Json)
            .unwrap(),
        2
    );
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .documents,
        3
    );
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT * FROM docs").rows.is_empty());
}

#[test]
fn consuming_portable_values_preserve_lossless_wire_contract() {
    let value = Value::Object(
        [
            ("integer".into(), Value::Integer(i64::MAX)),
            ("number".into(), Value::Number(-0.0)),
            (
                "nested".into(),
                Value::Array(vec![
                    Value::Boolean(true),
                    Value::Null,
                    Value::Binary(vec![0, 255]),
                ]),
            ),
        ]
        .into(),
    );
    let text = value.clone().into_portable_json().unwrap();
    let mut written = Vec::new();
    value.clone().write_portable_json(&mut written).unwrap();
    assert_eq!(written, text.as_bytes());
    assert_eq!(text, value.to_portable_value().unwrap().to_string());
    let encoded = value.clone().into_portable_value().unwrap();
    assert_eq!(encoded["value"]["integer"]["type"], "Integer");
    assert_eq!(encoded["value"]["integer"]["value"], "9223372036854775807");
    assert_eq!(encoded["value"]["number"]["type"], "Number");
    assert_eq!(encoded["value"]["number"]["value"], "8000000000000000");
    assert_eq!(encoded, value.to_portable_value().unwrap());
    let decoded = Value::from_portable_value(encoded).unwrap();
    let Value::Object(fields) = &decoded else {
        panic!("object type lost")
    };
    let Value::Number(number) = fields["number"] else {
        panic!("number type lost")
    };
    assert_eq!(number.to_bits(), (-0.0f64).to_bits());
    assert_eq!(decoded, value);
    for value in [
        Value::vector32(&[1.0, 0.0, -1.0]).unwrap(),
        Value::vector64(&[1.0, 0.0, -1.0]).unwrap(),
        Value::vector32_sparse(&[1.0, 0.0, -1.0]).unwrap(),
        Value::vector8(&[1.0, 0.0, -1.0]).unwrap(),
        Value::vector1bit(&[1.0, 0.0, -1.0]).unwrap(),
    ] {
        let mut written = Vec::new();
        value.clone().write_portable_json(&mut written).unwrap();
        assert_eq!(
            written,
            value.clone().into_portable_json().unwrap().as_bytes()
        );
        assert_eq!(
            value.clone().into_portable_json().unwrap(),
            value.to_portable_value().unwrap().to_string()
        );
        assert_eq!(
            Value::from_portable_value(value.clone().into_portable_value().unwrap()).unwrap(),
            value
        );
    }
    assert_eq!(
        Value::Number(f64::INFINITY)
            .into_portable_value()
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    let mut deep = Value::Null;
    for _ in 0..65 {
        deep = Value::Array(vec![deep]);
    }
    assert_eq!(deep.into_portable_value().unwrap_err().code(), "FDB_LIMIT");
}

#[test]
fn portable_json_writer_validates_before_output_and_propagates_sink_failure() {
    struct Sink {
        bytes: Vec<u8>,
        remaining: usize,
    }
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(std::io::Error::other("test sink exhausted"));
            }
            let count = bytes.len().min(self.remaining).min(3);
            self.bytes.extend_from_slice(&bytes[..count]);
            self.remaining -= count;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            panic!("the value writer must leave flushing to its caller")
        }
    }
    let value = Value::Array(vec![
        Value::String("quoted\"\nไทย".into()),
        Value::Record(romedb::Record {
            table: "docs".into(),
            key: romedb::Key::Integer(i64::MIN),
        }),
        Value::Binary(vec![255; 4096]),
    ]);
    let expected = value.clone().into_portable_json().unwrap();
    let mut sink = Sink {
        bytes: b"prefix:".to_vec(),
        remaining: usize::MAX,
    };
    value.clone().write_portable_json(&mut sink).unwrap();
    assert_eq!(&sink.bytes[7..], expected.as_bytes());
    for limit in [0, 1, 19, expected.len() - 1] {
        let mut sink = Sink {
            bytes: Vec::new(),
            remaining: limit,
        };
        let error = value.clone().write_portable_json(&mut sink).unwrap_err();
        assert_eq!(error.code(), "FDB_STORAGE");
        let romedb::Error::Encoding(error) = error else {
            panic!("lost sink error")
        };
        assert!(error.is_io());
        assert_eq!(sink.bytes, expected.as_bytes()[..limit]);
    }
    let mut deep = Value::Null;
    for _ in 0..65 {
        deep = Value::Array(vec![deep]);
    }
    for (invalid, code) in [
        (
            Value::Array(vec![Value::Integer(1), Value::Number(f64::INFINITY)]),
            "FDB_VALIDATION",
        ),
        (deep, "FDB_LIMIT"),
    ] {
        let mut output = b"unchanged".to_vec();
        assert_eq!(
            invalid.write_portable_json(&mut output).unwrap_err().code(),
            code
        );
        assert_eq!(output, b"unchanged");
    }
}

#[test]
fn maximum_document_depth_roundtrips_storage_and_both_transfer_formats() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    let mut value = Value::String("[\\\"{}]ไทย".into());
    for depth in 0..63 {
        value = if depth % 2 == 0 {
            Value::Array(vec![value])
        } else {
            Value::Object([("nested".into(), value)].into())
        };
    }
    let doc = c
        .insert("docs", [("payload".into(), value.clone())].into())
        .unwrap();
    let Value::Record(id) = &doc["id"] else {
        panic!("record id lost")
    };
    assert_eq!(c.get(id).unwrap().unwrap(), doc);
    assert_eq!(
        q(&c, "SELECT payload FROM docs").rows,
        vec![vec![value.clone()]]
    );
    for (table, format) in [
        ("json_copy", TransferFormat::Json),
        ("lines_copy", TransferFormat::Ndjson),
    ] {
        let data = c.export_documents("docs", format).unwrap();
        // Import IDs deliberately retain their collection name, so replay into
        // the original collection after a transactional delete.
        q(&c, "BEGIN");
        q(&c, "DELETE FROM docs");
        assert_eq!(
            c.import_documents("docs", &data, format).unwrap(),
            1,
            "{table}"
        );
        assert_eq!(c.get(id).unwrap().unwrap(), doc);
        q(&c, "ROLLBACK");
    }
    let too_deep = Value::Array(vec![value]);
    assert_eq!(
        c.insert("docs", [("payload".into(), too_deep)].into())
            .unwrap_err()
            .code(),
        "FDB_LIMIT"
    );
    assert_eq!(c.get(id).unwrap().unwrap(), doc);
    let header = r#"{"format":"romedb.documents","version":1}"#;
    let valid = Value::Object(doc.clone()).into_portable_json().unwrap();
    let excessive = format!("{}0{}", "[".repeat(137), "]".repeat(137));
    for (format, input) in [
        (
            TransferFormat::Json,
            format!("{{\"header\":{header},\"documents\":[{valid},{excessive}]}}"),
        ),
        (
            TransferFormat::Ndjson,
            format!("{header}\n{valid}\n{excessive}\n"),
        ),
    ] {
        q(&c, "BEGIN");
        q(&c, "DELETE FROM docs");
        assert_eq!(
            c.import_documents("docs", &input, format)
                .unwrap_err()
                .code(),
            "FDB_LIMIT"
        );
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert!(c.get(id).unwrap().is_none());
        q(&c, "ROLLBACK");
        assert_eq!(c.get(id).unwrap().unwrap(), doc);
    }
}
