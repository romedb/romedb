use romedb::{Database, Document, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}
fn point(x: f32, y: f32) -> Value {
    Value::vector32(&[x, y]).unwrap()
}
fn insert(c: &romedb::Connection, key: &str, v: Value) {
    c.insert(
        "items",
        Document::from([
            (
                "id".into(),
                Value::Record(romedb::Record {
                    table: "items".into(),
                    key: romedb::Key::String(key.into()),
                }),
            ),
            ("v".into(), v),
        ]),
    )
    .unwrap();
}
fn keys(c: &romedb::Connection, v: &Value, n: usize) -> Vec<String> {
    c.search_vectors("items_vec", v, n)
        .unwrap()
        .rows
        .into_iter()
        .map(|row| match &row[0] {
            Value::Record(r) => match &r.key {
                romedb::Key::String(s) => s.clone(),
                _ => panic!(),
            },
            _ => panic!(),
        })
        .collect()
}
#[test]
fn ann_persists_build_writes_and_connection_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ann.db");
    {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let writer = db.connect().unwrap();
        let reader = db.connect().unwrap();
        q(&writer, "CREATE TABLE items");
        insert(&writer, "a", point(1., 0.));
        insert(&writer, "b", point(0., 1.));
        insert(&writer, "null", Value::Null);
        writer
            .create_vector_index("items", "items_vec", vec!["v".into()], 2, "l2", false)
            .unwrap();
        assert_eq!(keys(&reader, &point(1., 0.), 2), vec!["a", "b"]);
        q(&reader, "BEGIN");
        assert_eq!(keys(&reader, &point(1., 0.), 1), vec!["a"]);
        q(&writer, "BEGIN");
        q(&writer, "DELETE FROM items WHERE id=items:a");
        insert(&writer, "c", point(1., 0.));
        assert_eq!(keys(&writer, &point(1., 0.), 1), vec!["c"]);
        assert_eq!(keys(&reader, &point(1., 0.), 1), vec!["a"]);
        q(&writer, "COMMIT");
        assert_eq!(keys(&reader, &point(1., 0.), 1), vec!["a"]);
        q(&reader, "COMMIT");
        assert_eq!(keys(&reader, &point(1., 0.), 1), vec!["c"]);
        q(&writer, "BEGIN");
        q(&writer, "SAVEPOINT one");
        q(&writer, "DELETE FROM items WHERE id=items:c");
        assert_eq!(keys(&writer, &point(1., 0.), 1), vec!["b"]);
        q(&writer, "ROLLBACK TO one");
        insert(&writer, "d", point(0.5, 0.));
        assert_eq!(keys(&writer, &point(1., 0.), 2), vec!["c", "d"]);
        q(&writer, "ROLLBACK");
        assert_eq!(keys(&writer, &point(1., 0.), 2), vec!["c", "b"]);
        writer
            .check_collection_integrity("items", Default::default())
            .unwrap();
    }
    let db = Database::open(file.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(keys(&c, &point(1., 0.), 2), vec!["c", "b"]);
    q(&c, "BEGIN");
    q(&c, "DROP INDEX items_vec");
    assert!(c.search_vectors("items_vec", &point(1., 0.), 2).is_err());
    q(&c, "ROLLBACK");
    assert_eq!(keys(&c, &point(1., 0.), 2), vec!["c", "b"]);
    q(&c, "DROP INDEX items_vec");
    drop(db.connect().unwrap());
}
#[test]
fn ann_journal_checkpoint_rollback_reused_ids_and_validation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE items");
    c.create_vector_index("items", "items_vec", vec!["v".into()], 2, "cosine", false)
        .unwrap();
    assert!(keys(&c, &point(1., 0.), 10).is_empty());
    c.create_vector_index("items", "items_vec", vec!["v".into()], 2, "cosine", true)
        .unwrap();
    assert!(c
        .create_vector_index("items", "items_vec", vec!["v".into()], 2, "l2", true)
        .is_err());
    assert!(c.search_vectors("items_vec", &point(0., 0.), 10).is_err());
    assert!(c
        .search_vectors("items_vec", &Value::vector64(&[1., 0.]).unwrap(), 10)
        .is_err());
    assert!(c
        .insert("items", Document::from([("v".into(), point(0., 0.))]))
        .is_err());
    insert(&c, "a", point(1., 0.));
    q(&c, "BEGIN");
    // More than one checkpoint, then roll back to the original graph/log.
    for n in 0..1030 {
        q(
            &c,
            &format!("UPSERT items:a {{v:vector32('[{},1]')}}", n + 1),
        );
    }
    assert_eq!(keys(&c, &point(1., 0.), 1), vec!["a"]);
    q(&c, "ROLLBACK");
    assert_eq!(
        c.search_vectors("items_vec", &point(1., 0.), 1)
            .unwrap()
            .rows[0][1],
        Value::Number(0.)
    );
    q(&c, "BEGIN");
    q(&c, "SAVEPOINT before");
    q(&c, "DELETE FROM items");
    insert(&c, "bad_branch", point(0., 1.));
    assert_eq!(keys(&c, &point(0., 1.), 1), vec!["bad_branch"]);
    q(&c, "ROLLBACK TO before");
    q(&c, "DELETE FROM items");
    insert(&c, "new_branch", point(-1., 0.));
    assert_eq!(keys(&c, &point(-1., 0.), 1), vec!["new_branch"]);
    q(&c, "COMMIT");
    c.check_collection_integrity("items", Default::default())
        .unwrap();
    q(&c, "DROP TABLE items");
    drop(db.connect().unwrap());
}

#[test]
fn ann_romeql_sources_keep_typed_hits_limits_and_filters() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE items");
    q(&c, "DEFINE FIELD v ON items TYPE vector<2>");
    q(
        &c,
        "CREATE SEARCH INDEX items_vec ON items(v) USING VECTOR WITH (metric='l2',dimensions=2)",
    );
    assert_eq!(
        c.execute(
            "DEFINE FIELD OVERWRITE v ON items TYPE vector<3>",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_VALIDATION"
    );
    insert(&c, "c", point(1., 0.));
    insert(&c, "a", point(1., 0.));
    insert(&c, "b", point(2., 0.));
    let sql="SELECT id,distance FROM search::vector('items_vec',vector32('[1,0]'),2) ORDER BY distance,id";
    let result = c.execute(sql, &Parameters::new()).unwrap();
    assert_eq!(result.columns, vec!["id", "distance"]);
    assert_eq!(keys(&c, &point(1., 0.), 2), vec!["a", "c"]);
    assert_eq!(
        result.rows,
        c.search_vectors("items_vec", &point(1., 0.), 2)
            .unwrap()
            .rows
    );
    assert!(c
        .execute(
            "SELECT id FROM search::vector('items_vec',vector32('[1,0]'),1) WHERE id=items:b",
            &Parameters::new()
        )
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(c.execute("SELECT i.id,h.distance FROM search::vector('items_vec',$v,2) h JOIN items i ON i.id=h.id ORDER BY h.distance,i.id",&Parameters::from([("$v".into(),point(1.,0.))])).unwrap().rows,result.rows);
    let plan = c
        .execute(&format!("EXPLAIN QUERY PLAN {sql}"), &Parameters::new())
        .unwrap();
    eprintln!("ANN materialization plan: {:?}", plan.rows);
    assert!(format!("{:?}", plan.rows).contains("__romedb_ann_hnsw_hits"));
    assert!(c
        .execute(
            "SELECT * FROM search::vector('items_vec',vector32('[1,0]'),0)",
            &Parameters::new()
        )
        .unwrap()
        .rows
        .is_empty());
    c.check_collection_integrity("items", Default::default())
        .unwrap();
}

#[test]
fn ann_failed_builds_replacement_and_late_write_validation_are_atomic() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "INSERT INTO items {v:'bad'}");
    assert!(c
        .create_vector_index("items", "items_vec", vec!["v".into()], 2, "l2", false)
        .is_err());
    drop(db.connect().unwrap());
    q(&c, "DELETE FROM items");
    q(&c, "CREATE UNIQUE INDEX unique_code ON items(code)");
    q(
        &c,
        "INSERT INTO items {id:items:a,code:7,v:vector32('[1,0]')}",
    );
    c.create_vector_index("items", "items_vec", vec!["v".into()], 2, "l2", false)
        .unwrap();
    q(
        &c,
        "INSERT OR REPLACE INTO items (id,code,v) VALUES (items:b,7,vector32('[0,1]'))",
    );
    assert_eq!(keys(&c, &point(1., 0.), 10), vec!["b"]);
    let before = c
        .search_vectors("items_vec", &point(1., 0.), 10)
        .unwrap()
        .rows;
    assert!(c
        .execute(
            "INSERT INTO items(v) SELECT vector32('[1,0]') UNION ALL SELECT 1",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(
        c.search_vectors("items_vec", &point(1., 0.), 10)
            .unwrap()
            .rows,
        before
    );
    c.check_collection_integrity("items", Default::default())
        .unwrap();
}

#[test]
fn ann_recovers_committed_checkpoint_and_redo_after_process_exit() {
    const ENV: &str = "ROMEDB_ANN_CRASH_TEST_PATH";
    if let Ok(path) = std::env::var(ENV) {
        let db = Database::open(&path).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE items");
        c.create_vector_index("items", "items_vec", vec!["v".into()], 2, "l2", false)
            .unwrap();
        q(&c, "BEGIN");
        for n in 0..1026 {
            q(&c, &format!("UPSERT items:a {{v:vector32('[{n},0]')}}"));
        }
        q(&c, "COMMIT");
        insert(&c, "b", point(0., 1.));
        q(&c, "BEGIN");
        q(&c, "DELETE FROM items");
        insert(&c, "uncommitted", point(0., 0.));
        assert_eq!(keys(&c, &point(0., 0.), 10), vec!["uncommitted"]);
        std::process::exit(73);
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crash.db");
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ann_recovers_committed_checkpoint_and_redo_after_process_exit",
        ])
        .env(ENV, &path)
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(73),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(keys(&c, &point(1025., 0.), 10), vec!["a", "b"]);
    assert_eq!(
        c.check_collection_integrity("items", Default::default())
            .unwrap()
            .documents,
        2
    );
}
