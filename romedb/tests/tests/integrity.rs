use romedb::{Database, IntegrityLimits, Parameters, Value};
#[test]
fn collection_integrity_survives_reopen_and_exact_byte_limits() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("audit.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE docs",
            "DEFINE FIELD embedding ON docs TYPE vector<3> REQUIRED",
            "CREATE INDEX docs_tag ON docs(profile.tag)",
            "INSERT INTO docs {id:docs:a,profile:{tag:'rust'},embedding:vector32('[1,0,0]')}",
            "INSERT INTO docs (id,embedding,data) VALUES (docs:b,vector32('[0,1,0]'),X'00ff')",
        ] {
            c.execute(sql, &Parameters::new()).unwrap();
        }
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    let report = c
        .check_collection_integrity("docs", IntegrityLimits::default())
        .unwrap();
    assert_eq!(
        (report.documents, report.indexes, report.index_entries),
        (2, 1, 2)
    );
    let exact = IntegrityLimits {
        max_documents: 2,
        max_encoded_bytes: report.encoded_bytes,
    };
    assert_eq!(
        c.check_collection_integrity("docs", exact)
            .unwrap()
            .encoded_bytes,
        report.encoded_bytes
    );
    assert_eq!(
        c.check_collection_integrity(
            "docs",
            IntegrityLimits {
                max_encoded_bytes: report.encoded_bytes - 1,
                ..exact
            }
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        c.execute("SELECT count(*) FROM docs", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::Integer(2)]]
    );
}
