use romedb::{Database, Document, Key, Parameters, Record, Value};
fn reference(table: &str, key: &str) -> Value {
    Value::Record(Record {
        table: table.into(),
        key: Key::String(key.into()),
    })
}
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
#[test]
fn invalid_nested_reference_targets_are_rejected_before_any_write() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    for table in [
        "__romedb_catalog",
        "__ROMEDB_hidden",
        "sqlite_schema",
        "SQLITE_private",
        "bad\0name",
        "",
    ] {
        let doc = Document::from([("nested".into(), Value::Array(vec![reference(table, "key")]))]);
        assert!(c.insert("docs", doc.clone()).is_err(), "target {table:?}");
        let params = Parameters::from([("$doc".into(), Value::Object(doc))]);
        assert!(c
            .execute("INSERT INTO docs DOCUMENT $doc", &params)
            .is_err());
    }
    let input = r#"{"header":{"format":"romedb.documents","version":1},"documents":[
        {"type":"Object","value":{}},
        {"type":"Object","value":{"nested":{"type":"Record","value":{"table":"__romedb_catalog","key":{"type":"String","value":"key"}}}}}
    ]}"#;
    assert!(c
        .import_documents("docs", input, romedb::TransferFormat::Json)
        .is_err());
    assert!(q(&c, "SELECT * FROM docs").rows.is_empty());
}
#[test]
fn failed_reference_patches_preserve_existing_data_and_indexes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE INDEX refs ON docs(author)");
    q(&c, "INSERT INTO docs {id:docs:p1,author:users:u1}");
    let id = Record {
        table: "docs".into(),
        key: Key::String("p1".into()),
    };
    let invalid = reference("__romedb_catalog", "key");
    assert!(c
        .patch(&id, Document::from([("other".into(), invalid.clone())]))
        .is_err());
    assert!(c
        .upsert(
            "docs",
            Document::from([
                ("id".into(), Value::Record(id.clone())),
                ("other".into(), invalid.clone())
            ])
        )
        .is_err());
    let params = Parameters::from([("$bad".into(), invalid)]);
    assert!(c.execute("UPDATE docs SET other=$bad", &params).is_err());
    assert!(c
        .execute("INSERT INTO docs (other) VALUES ($bad)", &params)
        .is_err());
    assert!(!c.get(&id).unwrap().unwrap().contains_key("other"));
    assert_eq!(
        c.lookup_index("docs", "refs", &reference("USERS", "u1"))
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn case_insensitive_targets_keep_index_and_fetch_identity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE users");
    q(&c, "INSERT INTO users {id:users:u1,name:'Alice'}");
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE UNIQUE INDEX refs ON docs(author)");
    c.insert(
        "docs",
        Document::from([("author".into(), reference("UsErS", "u1"))]),
    )
    .unwrap();
    assert_eq!(
        c.lookup_index("docs", "refs", &reference("users", "u1"))
            .unwrap()
            .len(),
        1
    );
    assert!(c
        .insert(
            "docs",
            Document::from([("author".into(), reference("USERS", "u1"))])
        )
        .is_err());
    let rows = q(
        &c,
        "SELECT record::table(author) AS target, record::fetch(author) AS user FROM docs",
    )
    .rows;
    assert_eq!(rows[0][0], Value::String("users".into()));
    let Value::Object(user) = &rows[0][1] else {
        panic!("missing fetched document")
    };
    assert_eq!(user["name"], Value::String("Alice".into()));
}
