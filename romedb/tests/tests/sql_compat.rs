use romedb::{Database, Parameters, Value};
use turso_core::{Database as Baseline, Numeric, Value as EngineValue};
fn convert(v: EngineValue) -> Value {
    match v {
        EngineValue::Null => Value::Null,
        EngineValue::Numeric(Numeric::Integer(i)) => Value::Integer(i),
        EngineValue::Numeric(Numeric::Float(n)) => Value::Number(n.into()),
        EngineValue::Text(t) => Value::String(t.as_str().into()),
        EngineValue::Blob(b) => Value::Binary(b),
    }
}
#[test]
fn ordinary_sql_matches_the_pinned_engine() {
    let baseline = Baseline::open_file(
        Baseline::io_for_path(":memory:").expect("io"),
        ":memory:",
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .expect("baseline");
    let raw = baseline.connect().expect("baseline connection");
    let db = Database::open(":memory:").expect("romedb");
    let c = db.connect().expect("romedb connection");
    for sql in [
        "CREATE TABLE accounts(id INTEGER PRIMARY KEY, name TEXT UNIQUE)",
        "INSERT INTO accounts VALUES (1, 'Alice'), (2, 'Bob') RETURNING *",
        "SELECT id, upper(name), id / 2.0 FROM accounts ORDER BY id",
        "SELECT r'posts:p1' FROM (SELECT 1 AS r)",
        "SELECT 9223372036854775807, NULL, x'00ff', 'users:u1'",
        "SELECT 1 AS [document], 2 AS `field`, 3 AS 'alias'",
        "BEGIN",
        "UPDATE accounts SET name = 'Charlie' WHERE id = 1 RETURNING *",
        "ROLLBACK",
        "SELECT * FROM accounts ORDER BY id",
        "CREATE TABLE copied AS SELECT * FROM accounts",
        "SELECT * FROM copied ORDER BY id",
        "INSERT INTO accounts VALUES (3, 'Bob') ON CONFLICT(name) DO UPDATE SET id = 3 RETURNING *",
        "SELECT * FROM accounts ORDER BY id",
    ] {
        let mut stmt = raw.prepare(sql).expect(sql);
        let names: Vec<String> = (0..stmt.num_columns())
            .map(|i| stmt.get_column_name(i).into_owned())
            .collect();
        let expected: Vec<Vec<Value>> = stmt
            .run_collect_rows()
            .expect(sql)
            .into_iter()
            .map(|row| row.into_iter().map(convert).collect())
            .collect();
        let actual = c.execute(sql, &Parameters::new()).expect(sql);
        assert_eq!(actual.columns, names, "{sql}");
        assert_eq!(actual.rows, expected, "{sql}");
    }
}

#[test]
fn malformed_collection_sql_preserves_native_parse_errors_and_active_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let baseline = Baseline::open_file(
        Baseline::io_for_path(":memory:").unwrap(),
        ":memory:",
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap();
    let raw = baseline.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE native(v INTEGER)",
        "BEGIN",
        "INSERT INTO docs {v:1}",
    ] {
        c.execute(sql, &Parameters::new()).unwrap();
    }
    for sql in [
        "SELECT 1 + FROM docs",
        "SELECT * FROM docs WHERE",
        "WITH x AS (SELECT * FROM docs WHERE) SELECT * FROM x",
        "INSERT INTO native SELECT FROM docs",
        "SELECT 'docs' + FROM native",
        "SELECT 1 + FROM native",
    ] {
        let expected = raw
            .prepare(sql)
            .expect_err("baseline parse failure")
            .to_string();
        let report = c.execute_report(sql, &Parameters::new());
        let romedb::Error::Engine(actual) = report.result.unwrap_err() else {
            panic!("expected native parse error: {sql}");
        };
        assert_eq!(actual.to_string(), expected, "{sql}");
        assert_eq!(report.transaction_after, romedb::TransactionState::Active);
    }
    assert_eq!(
        c.execute("SELECT v FROM docs", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::Integer(1)]]
    );
    assert!(c
        .execute("SELECT * FROM native", &Parameters::new())
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        c.execute("SELECT 1; SELECT 2", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_UNSUPPORTED"
    );
    c.execute("ROLLBACK", &Parameters::new()).unwrap();
}

#[test]
fn native_source_free_scalar_limit_parameter_matches_pinned_preparation() {
    let baseline = Baseline::open_file(
        Baseline::io_for_path(":memory:").unwrap(),
        ":memory:",
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap();
    let raw = baseline.connect().unwrap();
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE baseline(n INTEGER)",
        "INSERT INTO baseline VALUES(1),(2)",
    ] {
        raw.prepare(sql).unwrap().run_collect_rows().unwrap();
        c.execute(sql, &Parameters::new()).unwrap();
    }
    let sql = "SELECT n,(SELECT d.n ORDER BY d.n DESC LIMIT $limit OFFSET $offset) FROM baseline d ORDER BY n";
    let stmt = raw.prepare(sql).unwrap();
    assert!(stmt.parameter_index("$limit").is_none());
    assert!(stmt.parameter_index("$offset").is_some());
    let params = Parameters::from([
        ("$limit".into(), Value::Integer(1)),
        ("$offset".into(), Value::Integer(0)),
    ]);
    assert!(matches!(c.execute(sql,&params),Err(romedb::Error::Parameter(name)) if name=="$limit"));
    assert!(
        matches!(c.profile_select(sql,&params),Err(romedb::Error::Parameter(name)) if name=="$limit")
    );
    let literal =
        "SELECT n,(SELECT d.n ORDER BY d.n DESC LIMIT 1 OFFSET 0) FROM baseline d ORDER BY n";
    let expected = raw
        .prepare(literal)
        .unwrap()
        .run_collect_rows()
        .unwrap()
        .into_iter()
        .map(|row| row.into_iter().map(convert).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(
        c.execute(literal, &Parameters::new()).unwrap().rows,
        expected
    );
}

#[test]
fn experimental_table_features_retain_pinned_default_rejection() {
    let baseline = Baseline::open_file(
        Baseline::io_for_path(":memory:").unwrap(),
        ":memory:",
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap();
    let raw = baseline.connect().unwrap();
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for (sql, flag) in [
        (
            "CREATE TABLE gated(n INTEGER PRIMARY KEY) WITHOUT ROWID",
            "--experimental-without-rowid",
        ),
        (
            "CREATE TABLE gated(n INTEGER,label TEXT GENERATED ALWAYS AS ('item-' || n) VIRTUAL)",
            "--experimental-generated-columns",
        ),
    ] {
        let native_error = match raw.prepare(sql) {
            Err(error) => error,
            Ok(_) => panic!("expected gated feature: {sql}"),
        };
        assert!(native_error.to_string().contains(flag));
        match c.execute(sql, &Parameters::new()) {
            Err(romedb::Error::Engine(error)) => {
                assert_eq!(error.to_string(), native_error.to_string())
            }
            other => panic!("expected native feature rejection for {sql}: {other:?}"),
        }
        for retry in ["CREATE TABLE gated(n INTEGER)", "DROP TABLE gated"] {
            raw.prepare(retry).unwrap().run_collect_rows().unwrap();
            c.execute(retry, &Parameters::new()).unwrap();
        }
    }
}

#[test]
fn pinned_tuple_pagination_parameter_metadata_gap() {
    let baseline = Baseline::open_file(
        Baseline::io_for_path(":memory:").unwrap(),
        ":memory:",
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap();
    let raw = baseline.connect().unwrap();
    let mut create = raw
        .prepare("CREATE TABLE native(a INTEGER,b INTEGER)")
        .unwrap();
    create.run_with_row_callback(|_| Ok(())).unwrap();
    // Establish whether the missing binding originates before the RomeDB adapter.
    let tuple = raw
        .prepare("UPDATE native SET (a,b)=(SELECT b,a LIMIT $limit OFFSET $offset) RETURNING a,b")
        .unwrap();
    assert!(tuple.parameter_index("$limit").is_none());
    assert!(tuple.parameter_index("$offset").is_some());
    let scalar = raw
        .prepare("SELECT a FROM native LIMIT $limit OFFSET $offset")
        .unwrap();
    assert!(scalar.parameter_index("$limit").is_some());
    assert!(scalar.parameter_index("$offset").is_some());
}
