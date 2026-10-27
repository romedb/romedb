use romedb::{Database, Key, Parameters, Record, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
fn r(table: &str, key: i64) -> Value {
    Value::Record(Record {
        table: table.into(),
        key: Key::Integer(key),
    })
}
#[test]
fn batched_references_preserve_order_duplicates_nulls_and_typed_identity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE users");
    q(&c, "BEGIN");
    for i in 0..260 {
        q(
            &c,
            &format!("INSERT INTO users {{id:type::record('users',{i}),n:{i}}}"),
        );
    }
    q(&c, "COMMIT");
    let mut refs = (0..260).rev().map(|i| r("USERS", i)).collect::<Vec<_>>();
    refs.extend([r("users", 2), Value::Null, r("users", 999), r("missing", 1)]);
    let docs = c.fetch_records(&refs).unwrap();
    assert_eq!(docs.len(), 264);
    assert!(matches!(&docs[0],Value::Object(d) if d["n"]==Value::Integer(259)));
    assert_eq!(docs[257], docs[260]);
    assert_eq!(docs[261..], [Value::Null, Value::Null, Value::Null]);
    assert!(c.fetch_records(&[Value::String("users:2".into())]).is_err());
    assert_eq!(
        c.fetch_records(&vec![Value::Null; 16_385])
            .unwrap_err()
            .code(),
        "FDB_LIMIT"
    );
}
#[test]
fn select_fetches_are_typed_one_hop_projections() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE users");
    q(&c, "CREATE TABLE posts");
    q(
        &c,
        "INSERT INTO users {id:users:u1,name:'Alice',other:users:u2}",
    );
    q(&c, "INSERT INTO posts {id:posts:p1,author:users:u1}");
    q(&c, "INSERT INTO posts {id:posts:p2,author:users:missing}");
    q(&c, "INSERT INTO posts {id:posts:p3}");
    let rows = q(
        &c,
        "SELECT p.id,record::fetch(p.author) AS author FROM posts p ORDER BY p.id",
    )
    .rows;
    assert!(
        matches!(&rows[0][1],Value::Object(d) if d["name"]==Value::String("Alice".into()) && matches!(d["other"],Value::Record(_)))
    );
    assert_eq!(rows[1][1], Value::Null);
    assert_eq!(rows[2][1], Value::Null);
    for sql in [
        "SELECT record::fetch(record::fetch(author)) FROM posts",
        "SELECT n FROM posts WHERE record::fetch(author)",
        "SELECT record::fetch(author) AS a FROM posts ORDER BY a",
        "SELECT record::fetch(author) AS a FROM posts WHERE a IS NOT NULL",
        "UPDATE posts SET author=record::fetch(author)",
        "DELETE FROM posts RETURNING record::fetch(author)",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert_eq!(q(&c, "SELECT * FROM posts").rows.len(), 3);
    q(&c, "BEGIN");
    q(&c, "UPDATE users:u1 {name:'Changed'}");
    assert!(
        matches!(&q(&c,"SELECT record::fetch(users:u1) AS u").rows[0][0],Value::Object(d) if d["name"]==Value::String("Changed".into()))
    );
    q(&c, "ROLLBACK");
}
#[test]
fn relational_targets_require_explicit_correctly_typed_primary_keys() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(
        &c,
        "CREATE TABLE accounts(id INTEGER PRIMARY KEY,name TEXT)",
    );
    q(&c, "INSERT INTO accounts VALUES(1,'Alice')");
    q(&c, "CREATE TABLE labels(code TEXT PRIMARY KEY, value)");
    q(&c, "INSERT INTO labels VALUES('one',9)");
    let rows = q(
        &c,
        "SELECT record::fetch(accounts:1) AS a,record::fetch(labels:one) AS b",
    )
    .rows;
    assert!(matches!(&rows[0][0],Value::Object(d) if d["id"]==Value::Integer(1)));
    assert!(matches!(&rows[0][1],Value::Object(d) if d["value"]==Value::Integer(9)));
    assert!(q(&c, "SELECT record::fetch(accounts:99) AS a").rows[0][0] == Value::Null);
    q(&c, "CREATE TABLE composite(a,b,PRIMARY KEY(a,b))");
    q(&c, "CREATE TABLE implicit(a)");
    for reference in ["accounts:`1`", "labels:1", "composite:1", "implicit:1"] {
        assert!(c
            .execute(
                &format!("SELECT record::fetch({reference}) AS v"),
                &Parameters::new()
            )
            .is_err());
    }
}

#[test]
fn link_reads_observe_the_existing_transaction_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let a = db.connect().unwrap();
    let b = db.connect().unwrap();
    q(&a, "CREATE TABLE users");
    q(&a, "INSERT INTO users {id:users:u1,name:'Old'}");
    q(&a, "BEGIN");
    q(&a, "SELECT * FROM users");
    q(&b, "UPDATE users:u1 {name:'New'}");
    assert!(
        matches!(&q(&a,"SELECT record::fetch(users:u1) AS u").rows[0][0],Value::Object(d) if d["name"]==Value::String("Old".into()))
    );
    let profile = a
        .profile_select("SELECT record::fetch(users:u1) AS u", &Parameters::new())
        .unwrap();
    assert!(
        matches!(&profile.result.rows[0][0], Value::Object(d) if d["name"] == Value::String("Old".into()))
    );
    assert_eq!(profile.metrics.fetch_batches, 1);
    q(&a, "COMMIT");
    assert!(
        matches!(&q(&a,"SELECT record::fetch(users:u1) AS u").rows[0][0],Value::Object(d) if d["name"]==Value::String("New".into()))
    );
}

#[test]
fn fetch_projections_share_one_statement_byte_budget() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE targets");
    q(&c, "CREATE TABLE positions(n INTEGER)");
    q(&c, "INSERT INTO positions VALUES(1)");
    for _ in 0..12 {
        q(&c, "INSERT INTO positions SELECT n FROM positions");
    }
    q(&c, "BEGIN");
    let params = Parameters::from([("$text".into(), Value::String("x".repeat(8192)))]);
    c.execute("INSERT INTO targets {id:targets:a,text:$text}", &params)
        .unwrap();
    // Each projection alone is about 32 MiB. Combined expansion exceeds 64 MiB,
    // while 8,192 references remain below the 16,384-position count limit.
    let error = c
        .execute(
            "SELECT record::fetch(targets:a) AS a,record::fetch(targets:a) AS b FROM positions",
            &Parameters::new(),
        )
        .unwrap_err();
    assert_eq!(error.code(), "FDB_LIMIT", "{error}");
    assert_eq!(
        c.profile_select(
            "SELECT record::fetch(targets:a) AS a,record::fetch(targets:a) AS b FROM positions",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );

    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    let rows = q(&c, "SELECT record::fetch(targets:a) FROM positions").rows;
    assert_eq!(rows.len(), 4096);
    assert!(rows
        .iter()
        .all(|row| matches!(&row[0], Value::Object(doc) if doc["text"] == params["$text"])));
    drop(rows);
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT * FROM targets").rows.is_empty());
}

#[test]
fn profiles_attribute_deduplicated_target_batches_separately() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE native(n INTEGER PRIMARY KEY)");
    q(&c, "BEGIN");
    for n in 0..130 {
        q(
            &c,
            &format!("INSERT INTO docs {{id:type::record('docs',{n}),n:{n}}}"),
        );
        q(&c, &format!("INSERT INTO native VALUES({n})"));
    }
    let sql = "SELECT record::fetch(type::record('docs',n)) AS a,record::fetch(type::record('docs',n)) AS b,record::fetch(type::record('native',n)) AS c FROM native ORDER BY n";
    let profile = c.profile_select(sql, &Parameters::new()).unwrap();
    assert_eq!(profile.result.rows.len(), 130);
    assert!(profile.result.rows.iter().all(|row| row[0] == row[1]));
    assert_eq!(profile.metrics.fetch_batches, 4); // Two 128-key batches per target.
    assert!(profile.metrics.fetch_rows_read >= 260);
    assert!(profile.metrics.fetch_vm_steps > 0);
    assert_eq!(
        c.profile_select(sql, &Parameters::new()).unwrap().metrics,
        profile.metrics
    );
    let primary = c.profile_select("SELECT type::record('docs',n) AS a,type::record('docs',n) AS b,type::record('native',n) AS c FROM native ORDER BY n", &Parameters::new()).unwrap();
    assert_eq!(primary.metrics.rows_read, profile.metrics.rows_read);
    assert_eq!(primary.metrics.fetch_batches, 0);
    assert_eq!(primary.metrics.fetch_rows_read, 0);
    assert_eq!(primary.metrics.fetch_vm_steps, 0);
    let missing = c
        .profile_select(
            "SELECT record::fetch(type::record('missing',n)) FROM native",
            &Parameters::new(),
        )
        .unwrap();
    assert_eq!(missing.metrics.fetch_batches, 0);
    assert_eq!(missing.metrics.fetch_rows_read, 0);
    assert_eq!(missing.metrics.fetch_vm_steps, 0);
    q(&c, "ROLLBACK");
}
