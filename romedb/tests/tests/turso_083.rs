//! Public frontend qualification for the Turso 0.8.3 integration.
use romedb::{Connection, Database, Parameters, QueryResult, Value};
use std::sync::Arc;
use turso_core::execution_meter::{ExecutionLimits, ExecutionMeter};

fn q(c: &Connection, sql: &str) -> QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn ids(c: &Connection) -> Vec<Vec<Value>> {
    q(c, "SELECT id, v FROM items ORDER BY id").rows
}

fn explicit_id_one_then_generated_id(mvcc: bool) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("autoincrement.db");
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    if mvcc {
        q(&c, "PRAGMA journal_mode=mvcc");
    }
    q(
        &c,
        "CREATE TABLE items (id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT)",
    );
    q(&c, "INSERT INTO items (id, v) VALUES (1, 'explicit')");
    q(&c, "INSERT INTO items (v) VALUES ('generated')");
    let expected = vec![
        vec![Value::Integer(1), Value::String("explicit".into())],
        vec![Value::Integer(2), Value::String("generated".into())],
    ];
    assert_eq!(ids(&c), expected, "mvcc={mvcc}");
    drop(c);
    drop(db);
    let reopened = Database::open(path.to_str().unwrap()).unwrap();
    assert_eq!(ids(&reopened.connect().unwrap()), expected, "mvcc={mvcc}");
}

#[test]
fn explicit_autoincrement_id_one_is_not_overwritten_in_wal_mode() {
    explicit_id_one_then_generated_id(false);
}

#[test]
fn explicit_autoincrement_id_one_is_not_overwritten_in_mvcc_mode() {
    explicit_id_one_then_generated_id(true);
}

fn metered_explicit_insert(mvcc: bool) -> turso_core::execution_meter::ExecutionSnapshot {
    let conn = turso_core::Database::open_file(
        turso_core::Database::io_for_path(":memory:").unwrap(),
        ":memory:",
        Arc::new(turso_core::SqliteDialect),
    )
    .unwrap()
    .connect()
    .unwrap();
    if mvcc {
        conn.execute("PRAGMA journal_mode=mvcc").unwrap();
    }
    conn.execute("CREATE TABLE items (id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT)")
        .unwrap();
    let meter = Arc::new(ExecutionMeter::with_limits(ExecutionLimits {
        max_row_mutations: Some(1),
        ..Default::default()
    }));
    let mut statement = conn
        .prepare("INSERT INTO items (id, v) VALUES (1, 'explicit')")
        .unwrap();
    conn.set_execution_meter(Some(meter.clone())).unwrap();
    statement.run_with_row_callback(|_| Ok(())).unwrap();
    drop(statement);
    conn.set_execution_meter(None).unwrap();
    meter.snapshot()
}

#[test]
fn explicit_autoincrement_insert_counts_one_customer_mutation() {
    for mvcc in [false, true] {
        let snapshot = metered_explicit_insert(mvcc);
        assert_eq!(snapshot.row_mutations, 1, "mvcc={mvcc} {snapshot:?}");
        assert!(!snapshot.mutation_budget_exhausted, "mvcc={mvcc}");
    }
}

#[test]
fn switching_to_mvcc_inside_a_transaction_is_rejected_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("switch.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE articles");
        q(&c, "INSERT INTO articles {id:articles:one,title:'kept'}");
        q(&c, "BEGIN");
        let err = c
            .execute("PRAGMA journal_mode=mvcc", &Parameters::new())
            .expect_err("switching journal mode inside a transaction must fail");
        assert!(err.to_string().contains("mvcc"), "{err}");
        q(&c, "COMMIT");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    assert_eq!(
        q(&db.connect().unwrap(), "SELECT title FROM articles").rows,
        vec![vec![Value::String("kept".into())]]
    );
}
