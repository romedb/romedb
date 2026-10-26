use romedb::{Database, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
#[test]
fn selected_documents_keep_types_and_self_inserts_are_finite() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE posts");
    q(
        &c,
        "INSERT INTO posts {id:posts:p1,n:1,tags:[true,posts:p1]}",
    );
    let rows = q(
        &c,
        "INSERT INTO posts (n,tags) SELECT n+1,tags FROM posts RETURNING *",
    )
    .rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        q(&c, "SELECT n FROM posts ORDER BY n").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    q(&c, "CREATE TABLE archive");
    q(
        &c,
        "INSERT INTO archive (a,b,tags) SELECT n,n,tags FROM posts ORDER BY n LIMIT 1",
    );
    let rows = q(&c, "SELECT a,b,tags FROM archive").rows;
    assert_eq!(rows[0][0], rows[0][1]);
    assert!(
        matches!(&rows[0][2],Value::Array(a) if a[0]==Value::Boolean(true) && matches!(&a[1],Value::Record(_)))
    );
}
#[test]
fn later_conflicts_rollback_copies_indexes_and_keep_outer_transaction() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE src");
    q(&c, "CREATE TABLE dst");
    q(&c, "INSERT INTO src {n:1}");
    q(&c, "INSERT INTO src {n:1}");
    q(&c, "CREATE UNIQUE INDEX dst_n ON dst (n)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO dst {n:9}");
    assert!(c
        .execute("INSERT INTO dst (n) SELECT n FROM src", &Parameters::new())
        .is_err());
    assert_eq!(
        q(&c, "SELECT n FROM dst").rows,
        vec![vec![Value::Integer(9)]]
    );
    assert!(c
        .lookup_index("dst", "dst_n", &Value::Integer(1))
        .unwrap()
        .is_empty());
    q(&c, "COMMIT");
}
#[test]
fn relational_sources_parameters_and_empty_width_validation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE src (n INTEGER)");
    q(&c, "INSERT INTO src VALUES (1),(2)");
    q(&c, "CREATE TABLE dst");
    let params = Parameters::from([
        ("$min".into(), Value::Integer(2)),
        ("$flag".into(), Value::Boolean(true)),
    ]);
    c.execute(
        "INSERT INTO dst (n,flag) SELECT n,$flag FROM src WHERE n >= $min",
        &params,
    )
    .unwrap();
    assert_eq!(
        q(&c, "SELECT n,flag FROM dst").rows,
        vec![vec![Value::Integer(2), Value::Boolean(true)]]
    );
    assert!(c
        .execute(
            "INSERT INTO dst (a,b) SELECT n FROM src WHERE 0",
            &Parameters::new()
        )
        .is_err());
    assert!(q(
        &c,
        "INSERT INTO dst (a) SELECT n FROM src WHERE 0 RETURNING *"
    )
    .rows
    .is_empty());
}

#[test]
fn mismatched_compound_source_never_silently_inserts_a_prefix() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE dst");
    assert!(c
        .execute(
            "INSERT INTO dst (n) VALUES (1) UNION SELECT 2,3",
            &Parameters::new()
        )
        .is_err());
    assert!(q(&c, "SELECT * FROM dst").rows.is_empty());
}

#[test]
fn collection_sources_feed_native_inserts_and_conflict_policies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native-target.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(
            &c,
            "INSERT INTO docs (n,data) VALUES (1,X'31'),(2,X'32'),(3,X'33')",
        );
        q(&c, "CREATE TABLE target(n INTEGER PRIMARY KEY,data BLOB)");
        q(&c, "BEGIN");
        let result = q(
            &c,
            "INSERT INTO target SELECT n,data FROM docs ORDER BY n RETURNING n,hex(data) AS bytes",
        );
        assert_eq!(result.affected, 3);
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Integer(1), Value::String("31".into())],
                vec![Value::Integer(2), Value::String("32".into())],
                vec![Value::Integer(3), Value::String("33".into())]
            ]
        );
        q(&c, "ROLLBACK");
        assert!(q(&c, "SELECT n FROM target").rows.is_empty());
        q(&c, "INSERT INTO target VALUES (2,X'78')");
        assert_eq!(
            c.execute(
                "INSERT INTO target SELECT n,data FROM docs ORDER BY n",
                &Parameters::new()
            )
            .unwrap_err()
            .code(),
            "FDB_CONSTRAINT"
        );
        assert_eq!(
            q(&c, "SELECT n FROM target").rows,
            vec![vec![Value::Integer(2)]]
        );
        assert_eq!(
            c.execute(
                "INSERT OR FAIL INTO target SELECT n,data FROM docs ORDER BY n",
                &Parameters::new()
            )
            .unwrap_err()
            .code(),
            "FDB_CONSTRAINT"
        );
        assert_eq!(
            q(&c, "SELECT n FROM target ORDER BY n").rows,
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        );
        assert_eq!(
            q(
                &c,
                "INSERT OR IGNORE INTO target SELECT n,data FROM docs ORDER BY n"
            )
            .affected,
            1
        );
        let p = Parameters::from([
            ("$min".into(), Value::Integer(1)),
            ("$tag".into(), Value::String("done".into())),
        ]);
        let result = c.execute("INSERT OR REPLACE INTO target SELECT n AS key,data FROM docs WHERE key>$min RETURNING $tag AS tag", &p).unwrap();
        assert_eq!(
            result.rows,
            vec![
                vec![Value::String("done".into())],
                vec![Value::String("done".into())]
            ]
        );
        q(&c, "CREATE TABLE duplicate_columns(a INTEGER,b INTEGER)");
        q(
            &c,
            "INSERT INTO duplicate_columns SELECT n,n FROM docs WHERE n=1",
        );
        assert_eq!(
            q(&c, "SELECT * FROM duplicate_columns").rows,
            vec![vec![Value::Integer(1), Value::Integer(1)]]
        );
        assert!(c
            .execute(
                "INSERT INTO target SELECT name,sql FROM __romedb_catalog",
                &Parameters::new()
            )
            .is_err());
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        q(&c, "SELECT n,hex(data) FROM target ORDER BY n").rows,
        vec![
            vec![Value::Integer(1), Value::String("31".into())],
            vec![Value::Integer(2), Value::String("32".into())],
            vec![Value::Integer(3), Value::String("33".into())]
        ]
    );
}

#[test]
fn native_insert_sources_support_grouping_upsert_and_target_triggers() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {id:docs:1,category:'a',amount:2}");
    q(&c, "INSERT INTO docs {id:docs:2,category:'a',amount:3}");
    q(&c, "INSERT INTO docs {id:docs:3,category:'b',amount:4}");
    q(
        &c,
        "CREATE TABLE totals(category TEXT PRIMARY KEY,amount INTEGER)",
    );
    q(&c, "CREATE TABLE audit(category TEXT)");
    q(&c, "CREATE TRIGGER record_total AFTER INSERT ON totals BEGIN INSERT INTO audit VALUES (NEW.category); END");
    q(
        &c,
        "INSERT INTO totals SELECT category,sum(amount) FROM docs GROUP BY category",
    );
    assert_eq!(
        q(&c, "SELECT * FROM totals ORDER BY category").rows,
        vec![
            vec![Value::String("a".into()), Value::Integer(5)],
            vec![Value::String("b".into()), Value::Integer(4)]
        ]
    );
    assert_eq!(
        q(&c, "SELECT count(*) FROM audit").rows,
        vec![vec![Value::Integer(2)]]
    );
    let result=q(&c, "INSERT INTO totals SELECT category,sum(amount)+1 FROM docs WHERE 1 GROUP BY category ON CONFLICT(category) DO UPDATE SET amount=excluded.amount RETURNING amount");
    assert_eq!(result.rows.len(), 2);
    assert_eq!(
        q(&c, "SELECT amount FROM totals ORDER BY category").rows,
        vec![vec![Value::Integer(6)], vec![Value::Integer(5)]]
    );
    q(&c, "CREATE TABLE categories(category TEXT)");
    q(
        &c,
        "INSERT INTO categories SELECT DISTINCT category FROM docs",
    );
    assert_eq!(
        q(&c, "SELECT count(*) FROM categories").rows,
        vec![vec![Value::Integer(2)]]
    );
    for sql in [
        "INSERT INTO categories SELECT '__romedb_sql_scalar'(category) FROM docs",
        "INSERT INTO categories SELECT name FROM '__romedb_catalog'",
        "INSERT INTO categories SELECT name FROM docs JOIN '__romedb_catalog'",
    ] {
        assert_eq!(
            c.execute(sql, &Parameters::new()).unwrap_err().code(),
            "FDB_UNSUPPORTED"
        );
    }
    q(
        &c,
        "INSERT INTO categories SELECT '__romedb_literal' FROM docs WHERE amount=2",
    );
    q(&c, "CREATE TABLE keys(key INTEGER PRIMARY KEY)");
    q(&c, "INSERT INTO keys SELECT record::id(id) FROM docs");
    assert_eq!(
        q(&c, "SELECT key FROM keys ORDER BY key").rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(3)]
        ]
    );
    assert!(c
        .execute("INSERT INTO keys SELECT id FROM docs", &Parameters::new())
        .is_err());
    assert!(c
        .execute(
            "INSERT INTO keys SELECT record::fetch(id) FROM docs",
            &Parameters::new()
        )
        .is_err());
}
