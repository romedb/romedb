//! Public frontend qualification for the Turso 0.8.4 integration.
use romedb::{
    Connection, Database, Parameters, QueryResult, ResultLimits, TransactionState, Value,
    WriteWorkLimits,
};

fn q(c: &Connection, sql: &str) -> QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn count(c: &Connection, table: &str) -> Value {
    q(c, &format!("SELECT count(*) FROM {table}")).rows[0][0].clone()
}

fn open(dir: &tempfile::TempDir, name: &str, mvcc: bool) -> Connection {
    let path = dir.path().join(name);
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    if mvcc {
        q(&c, "PRAGMA journal_mode=mvcc");
    }
    c
}

fn cascade_fixture(c: &Connection) {
    for sql in [
        "PRAGMA foreign_keys=ON",
        "CREATE TABLE parent(id INTEGER PRIMARY KEY)",
        "CREATE TABLE child(id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent(id) ON DELETE CASCADE)",
        "INSERT INTO parent VALUES (1), (2), (3)",
        "INSERT INTO child VALUES (1, 1), (2, 1), (3, 2), (4, 2), (5, 3)",
    ] {
        q(c, sql);
    }
}

fn budget(n: u64) -> WriteWorkLimits {
    WriteWorkLimits {
        max_row_mutations: Some(n),
        ..Default::default()
    }
}

fn limits() -> ResultLimits {
    ResultLimits {
        max_rows: 100,
        max_payload_bytes: 65536,
    }
}

/// 0.8.4 collects the row IDs of a DELETE first whenever an ON DELETE action can
/// fire. The metered count must still be every deleted parent and child row,
/// and a budget that the cascade passes must undo the whole statement.
#[test]
fn cascading_delete_counts_parent_and_child_rows_and_stops_atomically() {
    let dir = tempfile::tempdir().unwrap();
    for mvcc in [false, true] {
        let c = open(&dir, &format!("cascade-{mvcc}.db"), mvcc);
        cascade_fixture(&c);
        let sql = "DELETE FROM parent WHERE id <= 2";

        let failed = c.write_metered(sql, &Parameters::new(), limits(), budget(5));
        assert!(failed.outcome.is_err(), "mvcc={mvcc}");
        assert!(failed.work.mutation_budget_exhausted, "mvcc={mvcc}");
        assert_eq!(
            c.transaction_state(),
            TransactionState::Autocommit,
            "mvcc={mvcc}"
        );
        assert_eq!(count(&c, "parent"), Value::Integer(3), "mvcc={mvcc}");
        assert_eq!(count(&c, "child"), Value::Integer(5), "mvcc={mvcc}");

        let done = c.write_metered(sql, &Parameters::new(), limits(), budget(6));
        done.outcome.unwrap_or_else(|e| panic!("mvcc={mvcc}: {e}"));
        assert_eq!(done.work.row_mutations, 6, "mvcc={mvcc}");
        assert!(!done.work.mutation_budget_exhausted, "mvcc={mvcc}");
        assert_eq!(count(&c, "parent"), Value::Integer(1), "mvcc={mvcc}");
        assert_eq!(count(&c, "child"), Value::Integer(1), "mvcc={mvcc}");
    }
}

/// 0.8.4 opens a statement savepoint for an UPDATE that changes row IDs. A
/// failure on a later row must undo the rows the statement already moved, and
/// leave the caller's transaction and its earlier writes in place.
#[test]
fn failed_row_id_update_keeps_the_callers_transaction() {
    let dir = tempfile::tempdir().unwrap();
    for mvcc in [false, true] {
        let c = open(&dir, &format!("rowid-{mvcc}.db"), mvcc);
        q(&c, "CREATE TABLE t(k INTEGER PRIMARY KEY, v INTEGER)");
        q(&c, "INSERT INTO t VALUES (1, 1), (2, 2), (5, 5), (15, 15)");
        q(&c, "BEGIN");
        q(&c, "INSERT INTO t VALUES (30, 30)");
        let err = c
            .execute(
                "UPDATE t SET k = k + 10 WHERE k BETWEEN 1 AND 5",
                &Parameters::new(),
            )
            .expect_err("k = 15 already exists");
        assert!(err.to_string().contains("UNIQUE"), "mvcc={mvcc}: {err}");
        assert_eq!(
            c.transaction_state(),
            TransactionState::Active,
            "mvcc={mvcc}"
        );
        q(&c, "COMMIT");
        let keys: Vec<Value> = q(&c, "SELECT k FROM t ORDER BY k")
            .rows
            .into_iter()
            .map(|row| row[0].clone())
            .collect();
        assert_eq!(
            keys,
            [1, 2, 5, 15, 30].map(Value::Integer).to_vec(),
            "mvcc={mvcc}"
        );
        assert_eq!(
            q(&c, "PRAGMA integrity_check").rows,
            vec![vec![Value::String("ok".into())]],
            "mvcc={mvcc}"
        );
    }
}
