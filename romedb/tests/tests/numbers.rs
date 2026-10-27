use romedb::{Database, Document, Key, Record, Value};
#[test]
fn numeric_bits_and_index_keys_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("numbers.db");
    let path = path.to_str().unwrap();
    let bits = [
        4477615450478306380u64,
        4477615450478306380u64 | (1 << 63),
        1,
        0x8000_0000_0000_0000,
        0x0010_0000_0000_0000,
        0x7fef_ffff_ffff_ffff,
    ];
    {
        let db = Database::open(path).unwrap();
        let c = db.connect().unwrap();
        c.create_collection("numbers", false).unwrap();
        c.create_index("numbers", "values", vec!["value".into()], false)
            .unwrap();
        for (i, bits) in bits.iter().enumerate() {
            let doc = Document::from([
                (
                    "id".into(),
                    Value::Record(Record {
                        table: "numbers".into(),
                        key: Key::String(i.to_string()),
                    }),
                ),
                ("value".into(), Value::Number(f64::from_bits(*bits))),
                (
                    "nested".into(),
                    Value::Array(vec![Value::Number(f64::from_bits(*bits))]),
                ),
            ]);
            c.insert("numbers", doc).unwrap();
        }
    }
    let db = Database::open(path).unwrap();
    let c = db.connect().unwrap();
    for (i, bits) in bits.iter().enumerate() {
        let id = Record {
            table: "numbers".into(),
            key: Key::String(i.to_string()),
        };
        let doc = c.get(&id).unwrap().unwrap();
        let Value::Number(value) = doc["value"] else {
            panic!("number type lost")
        };
        assert_eq!(value.to_bits(), *bits);
        let Value::Array(nested) = &doc["nested"] else {
            panic!("array type lost")
        };
        let Value::Number(value) = nested[0] else {
            panic!("nested number type lost")
        };
        assert_eq!(value.to_bits(), *bits);
        let indexed = c
            .lookup_index("numbers", "values", &Value::Number(f64::from_bits(*bits)))
            .unwrap();
        assert_eq!(indexed.len(), 1);
        assert_eq!(indexed[0]["id"], Value::Record(id));
    }
}
