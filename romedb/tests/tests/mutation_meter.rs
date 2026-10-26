use std::sync::Arc;
use turso_core::{
    execution_meter::{ExecutionLimits, ExecutionMeter},
    Connection, Database, LimboError,
};

fn open(path: &str) -> Arc<Connection> {
    Database::open_file(
        Database::io_for_path(path).unwrap(),
        path,
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap()
    .connect()
    .unwrap()
}
fn count(c: &Arc<Connection>, sql: &str) -> usize {
    let mut rows = 0;
    c.prepare(sql)
        .unwrap()
        .run_with_row_callback(|_| {
            rows += 1;
            Ok(())
        })
        .unwrap();
    rows
}
fn measured(c: &Arc<Connection>, sql: &str, changes: i64, mutations: u64) {
    let meter = Arc::new(ExecutionMeter::default());
    let mut statement = c.prepare(sql).unwrap();
    c.set_execution_meter(Some(meter.clone())).unwrap();
    statement.run_with_row_callback(|_| Ok(())).unwrap();
    assert_eq!(
        statement.n_change(),
        changes,
        "SQL changes must remain compatible: {sql}"
    );
    assert_eq!(
        meter.snapshot().row_mutations,
        mutations,
        "{sql}: {:?}",
        meter.snapshot()
    );
    c.set_execution_meter(None).unwrap();
}
#[test]
fn mutations_distinguish_update_replace_index_and_temporary_work() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        ":memory:".to_owned(),
        dir.path().join("mutations.db").to_str().unwrap().to_owned(),
    ] {
        let c = open(&path);
        measured(
            &c,
            "CREATE TABLE items(id INTEGER PRIMARY KEY,n INTEGER UNIQUE,payload TEXT)",
            0,
            0,
        );
        measured(&c, "CREATE INDEX items_payload ON items(payload)", 0, 0);
        for (sql, changes, mutations) in [
            ("INSERT INTO items VALUES(1,10,'one')",1,1),
            ("UPDATE items SET payload=payload WHERE id=1",1,1),
            ("UPDATE items SET id=2 WHERE id=1",1,1),
            ("INSERT OR REPLACE INTO items VALUES(3,10,'replacement')",1,2),
            ("INSERT INTO items VALUES(4,20,'other')",1,1),
            ("INSERT OR REPLACE INTO items VALUES(3,20,'two victims')",1,3),
            ("INSERT OR REPLACE INTO items VALUES(3,20,'same victim')",1,2),
            ("INSERT INTO items VALUES(5,20,'changed') ON CONFLICT(n) DO UPDATE SET payload=excluded.payload",1,1),
            ("INSERT OR IGNORE INTO items VALUES(5,20,'ignored')",0,0),
            ("UPDATE items SET n=21 WHERE id=3",1,1),
            ("SELECT DISTINCT payload FROM items ORDER BY payload LIMIT 1",0,0),
            ("DELETE FROM items WHERE id=3",1,1),
            ("DELETE FROM items WHERE id=99",0,0),
            ("INSERT INTO items VALUES(8,80,'returning') RETURNING id",1,1),
            ("CREATE INDEX items_n_more ON items(n)",0,0),
            ("UPDATE items SET n=81 WHERE id=8 RETURNING id",1,1),
            ("DELETE FROM items RETURNING id",1,1),
            ("INSERT INTO items VALUES(10,100,'a'),(20,200,'b'),(30,300,'c')",3,3),
            ("UPDATE OR REPLACE items SET id=20,n=300 WHERE id=10",1,3),
            ("INSERT INTO items VALUES(40,400,'d')",1,1),
            ("UPDATE OR REPLACE items SET n=400 WHERE id=20",1,2),
            ("INSERT INTO items VALUES(50,500,'e')",1,1),
            ("UPDATE OR REPLACE items SET id=50 WHERE id=20",1,2),
            ("DELETE FROM items",1,1),
        ] { measured(&c, sql, changes, mutations); }
        assert_eq!(count(&c, "SELECT * FROM items"), 0);
    }
}
#[test]
fn mutation_budgets_keep_attempts_and_rollback_all_rows() {
    let c = open(":memory:");
    c.execute("CREATE TABLE items(id INTEGER PRIMARY KEY,n INTEGER UNIQUE)")
        .unwrap();
    for limit in [0, 2, 4] {
        let meter = Arc::new(ExecutionMeter::with_limits(ExecutionLimits {
            max_row_mutations: Some(limit),
            ..Default::default()
        }));
        let mut statement = c
            .prepare("INSERT INTO items VALUES(1,10),(2,20),(3,30),(4,40)")
            .unwrap();
        c.set_execution_meter(Some(meter.clone())).unwrap();
        let outcome = statement.run_with_row_callback(|_| Ok(()));
        if limit < 4 {
            assert!(matches!(outcome, Err(LimboError::Interrupt)), "{outcome:?}");
            assert_eq!(meter.snapshot().row_mutations, limit + 1);
            assert!(meter.snapshot().mutation_budget_exhausted);
        } else {
            outcome.unwrap();
            assert_eq!(meter.snapshot().row_mutations, 4);
            assert!(!meter.snapshot().mutation_budget_exhausted);
        }
        if limit < 4 {
            assert!(matches!(
                c.set_execution_meter(None),
                Err(LimboError::StatementsInProgress(_))
            ));
        }
        // Finalize the failed writer before another statement uses the connection,
        // matching the checked frontend statement lifecycle.
        drop(statement);
        c.set_execution_meter(None).unwrap();
        assert_eq!(
            count(&c, "SELECT * FROM items"),
            if limit < 4 { 0 } else { 4 }
        );
    }
    c.execute("BEGIN").unwrap();
    let meter = Arc::new(ExecutionMeter::default());
    c.set_execution_meter(Some(meter.clone())).unwrap();
    c.execute("UPDATE items SET n=n+1").unwrap();
    assert_eq!(meter.snapshot().row_mutations, 4);
    c.execute("ROLLBACK").unwrap();
    assert_eq!(
        meter.snapshot().row_mutations,
        4,
        "retained mutation events are not committed writes"
    );
    c.set_execution_meter(None).unwrap();
    assert_eq!(count(&c, "SELECT * FROM items WHERE n IN (10,20,30,40)"), 4);
}
#[test]
fn trigger_mutations_share_budget_and_preserve_prior_transaction_work() {
    let c = open(":memory:");
    c.execute("CREATE TABLE items(id INTEGER PRIMARY KEY)")
        .unwrap();
    c.execute("CREATE TABLE audit(id INTEGER)").unwrap();
    c.execute("CREATE TABLE prior(n INTEGER)").unwrap();
    c.execute(
        "CREATE TRIGGER log_item AFTER INSERT ON items BEGIN INSERT INTO audit VALUES(new.id); END",
    )
    .unwrap();
    measured(&c, "INSERT INTO items VALUES(1)", 1, 2);
    c.execute("BEGIN").unwrap();
    c.execute("INSERT INTO prior VALUES(9)").unwrap();
    let meter = Arc::new(ExecutionMeter::with_limits(ExecutionLimits {
        max_row_mutations: Some(1),
        ..Default::default()
    }));
    c.set_execution_meter(Some(meter.clone())).unwrap();
    let result = c
        .prepare("INSERT INTO items VALUES(2)")
        .unwrap()
        .run_with_row_callback(|_| Ok(()));
    assert!(matches!(result, Err(LimboError::Interrupt)), "{result:?}");
    assert_eq!(meter.snapshot().row_mutations, 2);
    c.set_execution_meter(None).unwrap();
    assert!(!c.get_auto_commit());
    assert_eq!(count(&c, "SELECT * FROM prior"), 1);
    assert_eq!(count(&c, "SELECT * FROM items"), 1);
    assert_eq!(count(&c, "SELECT * FROM audit"), 1);
    c.execute("ROLLBACK").unwrap();
}

#[test]
fn change_capture_maintenance_is_not_a_row_mutation() {
    for mode in ["id", "before", "after", "full"] {
        let c = open(":memory:");
        c.execute("CREATE TABLE items(id INTEGER PRIMARY KEY,n INTEGER)")
            .unwrap();
        c.execute(format!("PRAGMA capture_data_changes_conn('{mode}')"))
            .unwrap();
        let total_before = c.total_changes();
        measured(&c, "INSERT INTO items VALUES(1,10)", 1, 1);
        if mode == "full" {
            assert_eq!(
                c.total_changes() - total_before,
                3,
                "legacy total_changes includes capture maintenance"
            );
        }
        measured(&c, "UPDATE items SET n=11 WHERE id=1", 1, 1);
        measured(&c, "DELETE FROM items WHERE id=1", 1, 1);
        let captured = count(&c, "SELECT * FROM turso_cdc");
        assert!(captured > 0);
        let meter = Arc::new(ExecutionMeter::with_limits(ExecutionLimits {
            max_row_mutations: Some(0),
            ..Default::default()
        }));
        c.set_execution_meter(Some(meter.clone())).unwrap();
        let result = c
            .prepare("INSERT INTO items VALUES(2,20)")
            .unwrap()
            .run_with_row_callback(|_| Ok(()));
        assert!(
            matches!(result, Err(LimboError::Interrupt)),
            "{mode}: {result:?}"
        );
        assert_eq!(meter.snapshot().row_mutations, 1);
        c.set_execution_meter(None).unwrap();
        assert_eq!(count(&c, "SELECT * FROM items"), 0);
        assert_eq!(count(&c, "SELECT * FROM turso_cdc"), captured);
    }
}

#[allow(dead_code)]
#[path = "../../../tests/integration/queued_io.rs"]
mod queued_io;

#[test]
fn pending_io_does_not_repeat_mutation_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queued-mutations.db");
    let io = Arc::new(queued_io::QueuedIo::new());
    let db = Database::open_file(
        io.clone(),
        path.to_str().unwrap(),
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap();
    let c = db.connect().unwrap();
    c.execute("CREATE TABLE items(id INTEGER PRIMARY KEY,payload BLOB)")
        .unwrap();
    let sql = format!(
        "INSERT INTO items VALUES {}",
        (0..64)
            .map(|n| format!("({n},zeroblob(1000))"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut statement = c.prepare(sql).unwrap();
    let meter = Arc::new(ExecutionMeter::default());
    c.set_execution_meter(Some(meter.clone())).unwrap();
    let mut suspended = 0;
    loop {
        match statement.step().unwrap() {
            turso_core::StepResult::Done => break,
            turso_core::StepResult::Yield => continue,
            turso_core::StepResult::IO => {
                suspended += 1;
                let before = meter.snapshot();
                for _ in 0..3 {
                    assert!(matches!(
                        statement.step().unwrap(),
                        turso_core::StepResult::IO
                    ));
                    assert_eq!(meter.snapshot(), before);
                }
                assert!(io.step_one().unwrap().is_some());
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(suspended > 0);
    assert_eq!(meter.snapshot().row_mutations, 64);
    drop(statement);
    c.set_execution_meter(None).unwrap();
    assert_eq!(count(&c, "SELECT * FROM items"), 64);
}

#[test]
fn maintenance_views_share_work_and_exhaustion_without_logical_mutations() {
    let c = open(":memory:");
    c.execute("CREATE TABLE items(n INTEGER)").unwrap();
    c.execute("INSERT INTO items VALUES(1),(2),(3)").unwrap();
    let root = Arc::new(ExecutionMeter::with_limits(ExecutionLimits {
        max_row_mutations: Some(0),
        ..Default::default()
    }));
    let maintenance = Arc::new(root.without_row_mutations());
    c.set_execution_meter(Some(maintenance.clone())).unwrap();
    c.execute("UPDATE items SET n=n+1").unwrap();
    c.set_execution_meter(None).unwrap();
    assert_eq!(root.snapshot(), maintenance.snapshot());
    assert_eq!(root.snapshot().row_mutations, 0);
    assert_eq!(root.snapshot().rows_read, 3);
    assert!(root.snapshot().rows_written >= 3);
    assert!(root.snapshot().vm_steps > 0);
    c.set_execution_meter(Some(root.clone())).unwrap();
    assert!(c.execute("INSERT INTO items VALUES(9)").is_err());
    c.set_execution_meter(None).unwrap();
    assert_eq!(root.snapshot().row_mutations, 1);
    let exhausted = root.snapshot();
    c.set_execution_meter(Some(maintenance.clone())).unwrap();
    assert!(c.execute("UPDATE items SET n=0").is_err());
    c.set_execution_meter(None).unwrap();
    assert_eq!(root.snapshot(), exhausted);
    for limits in [
        ExecutionLimits {
            max_rows_read: Some(1),
            ..Default::default()
        },
        ExecutionLimits {
            max_vm_steps: Some(1),
            ..Default::default()
        },
    ] {
        let root = Arc::new(ExecutionMeter::with_limits(limits));
        let maintenance = Arc::new(root.without_row_mutations());
        c.set_execution_meter(Some(maintenance.clone())).unwrap();
        assert!(c.execute("UPDATE items SET n=0").is_err());
        c.set_execution_meter(None).unwrap();
        let exhausted = root.snapshot();
        assert_eq!(exhausted, maintenance.snapshot());
        assert!(exhausted.read_budget_exhausted || exhausted.vm_budget_exhausted);
        assert_eq!(exhausted.row_mutations, 0);
        c.set_execution_meter(Some(root.clone())).unwrap();
        assert!(c.execute("SELECT * FROM items").is_err());
        c.set_execution_meter(None).unwrap();
        assert_eq!(root.snapshot(), exhausted);
    }
}
