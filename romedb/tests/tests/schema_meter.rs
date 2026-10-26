use std::sync::Arc;
use turso_core::{
    execution_meter::{ExecutionLimits, ExecutionMeter, ExecutionSnapshot},
    Connection, Database, LimboError, StepResult,
};

#[allow(dead_code)]
#[path = "../../../tests/integration/queued_io.rs"]
mod queued_io;

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

fn fixture(c: &Arc<Connection>) {
    c.execute("CREATE TABLE source(n INTEGER)").unwrap();
    c.execute("INSERT INTO source VALUES(1),(2),(3),(4),(5)")
        .unwrap();
}

fn run(c: &Arc<Connection>, sql: &str) -> turso_core::Result<()> {
    c.prepare(sql)?.run_with_row_callback(|_| Ok(()))
}

fn measure(c: &Arc<Connection>, sql: &str, separate: bool) -> ExecutionSnapshot {
    let meter = Arc::new(if separate {
        ExecutionMeter::with_schema_read_limit(ExecutionLimits::default(), None)
    } else {
        ExecutionMeter::default()
    });
    c.set_execution_meter(Some(meter.clone())).unwrap();
    run(c, sql).unwrap();
    c.set_execution_meter(None).unwrap();
    meter.snapshot()
}

#[test]
fn schema_builds_separate_source_reads_and_preserve_default_metrics() {
    let dir = tempfile::tempdir().unwrap();
    for in_memory in [true, false] {
        let mut snapshots = Vec::new();
        for separate in [false, true] {
            let path = if in_memory {
                ":memory:".to_owned()
            } else {
                dir.path()
                    .join(format!("ddl-{separate}.db"))
                    .to_str()
                    .unwrap()
                    .to_owned()
            };
            let c = open(&path);
            fixture(&c);
            let mut results = Vec::new();
            for (sql, writes) in [
                ("CREATE INDEX source_n ON source(n)", 0),
                ("CREATE TABLE copied AS SELECT n FROM source", 5),
            ] {
                let work = measure(&c, sql, separate);
                assert!(work.schema_rows_read > 0, "{sql}: {work:?}");
                assert_eq!(
                    work.rows_read,
                    5 + if separate { 0 } else { work.schema_rows_read },
                    "{sql}: {work:?}"
                );
                assert_eq!(work.row_mutations, writes, "{sql}: {work:?}");
                results.push(work);
            }
            run(&c, "SELECT n FROM copied WHERE n=5").unwrap();
            snapshots.push(results);
        }
        for (default, separated) in snapshots[0].iter().zip(&snapshots[1]) {
            assert_eq!(default.schema_rows_read, separated.schema_rows_read);
            assert_eq!(default.rows_written, separated.rows_written);
            assert_eq!(default.row_mutations, separated.row_mutations);
            assert_eq!(default.vm_steps, separated.vm_steps);
        }
    }
}

#[test]
fn schema_cursor_paths_and_aliases_keep_statement_metrics_unchanged() {
    let c = open(":memory:");
    fixture(&c);
    c.execute("CREATE TABLE second(n INTEGER)").unwrap();
    c.execute("CREATE INDEX source_n ON source(n)").unwrap();
    for (sql, visits) in [
        ("SELECT name FROM sqlite_schema", 3),
        ("SELECT name FROM sqlite_master", 3),
        ("SELECT name FROM sqlite_schema ORDER BY rowid DESC", 3),
        ("SELECT name FROM sqlite_schema WHERE rowid=1", 1),
        ("SELECT name FROM sqlite_schema WHERE rowid=99", 0),
        ("SELECT name FROM sqlite_schema WHERE rowid>=2", 2),
        ("SELECT count(*) FROM sqlite_schema", 3),
    ] {
        let meter = Arc::new(ExecutionMeter::with_schema_read_limit(
            ExecutionLimits::default(),
            None,
        ));
        let mut statement = c.prepare(sql).unwrap();
        c.set_execution_meter(Some(meter.clone())).unwrap();
        statement.run_with_row_callback(|_| Ok(())).unwrap();
        assert_eq!(statement.metrics().rows_read, visits, "{sql}");
        assert_eq!(meter.snapshot().rows_read, 0, "{sql}");
        assert_eq!(meter.snapshot().schema_rows_read, visits, "{sql}");
        c.set_execution_meter(None).unwrap();
    }
    // Application tables and temporary/materialized visits are never inferred to
    // be catalog work from a table name prefix or a physical root-page number.
    c.execute("CREATE TABLE app_catalog(n INTEGER)").unwrap();
    c.execute("INSERT INTO app_catalog SELECT n FROM source")
        .unwrap();
    for sql in [
        "SELECT n FROM app_catalog",
        "SELECT n FROM source ORDER BY n DESC",
        "WITH materialized AS MATERIALIZED (SELECT n FROM source) SELECT * FROM materialized",
    ] {
        let all = measure(&c, sql, false);
        let separated = measure(&c, sql, true);
        assert_eq!(all.rows_read, separated.rows_read, "{sql}");
        assert!(separated.rows_read >= 5, "{sql}");
        assert_eq!(separated.schema_rows_read, 0, "{sql}");
    }
}

#[test]
fn independent_schema_and_data_budgets_retain_crossing_visits_and_rollback_ddl() {
    let dir = tempfile::tempdir().unwrap();
    for in_memory in [true, false] {
        for (case, (read_limit, schema_limit, mutation_limit)) in [
            (Some(4), None, None),
            (None, Some(0), None),
            (None, None, Some(4)),
            (Some(5), Some(100), Some(5)),
        ]
        .into_iter()
        .enumerate()
        {
            let path = if in_memory {
                ":memory:".to_owned()
            } else {
                dir.path()
                    .join(format!("budgets-{case}.db"))
                    .to_str()
                    .unwrap()
                    .to_owned()
            };
            let c = open(&path);
            fixture(&c);
            c.execute("BEGIN").unwrap();
            c.execute("CREATE TABLE caller(n INTEGER)").unwrap();
            c.execute("INSERT INTO caller VALUES(42)").unwrap();
            c.execute("SAVEPOINT ddl").unwrap();
            let meter = Arc::new(ExecutionMeter::with_schema_read_limit(
                ExecutionLimits {
                    max_rows_read: read_limit,
                    max_row_mutations: mutation_limit,
                    max_vm_steps: None,
                },
                schema_limit,
            ));
            c.set_execution_meter(Some(meter.clone())).unwrap();
            let outcome = run(&c, "CREATE TABLE copied AS SELECT n FROM source");
            let work = meter.snapshot();
            if case == 3 {
                outcome.unwrap();
                assert_eq!(work.rows_read, 5);
                assert_eq!(work.row_mutations, 5);
            } else {
                assert!(matches!(outcome, Err(LimboError::Interrupt)), "{outcome:?}");
                match case {
                    0 => {
                        assert_eq!(work.rows_read, 5);
                        assert!(work.read_budget_exhausted);
                    }
                    1 => {
                        assert_eq!(work.schema_rows_read, 1);
                        assert!(work.schema_budget_exhausted);
                    }
                    2 => {
                        assert_eq!(work.row_mutations, 5);
                        assert!(work.mutation_budget_exhausted);
                    }
                    _ => unreachable!(),
                }
                assert!(matches!(run(&c, "SELECT 1"), Err(LimboError::Interrupt)));
                assert_eq!(meter.snapshot(), work);
            }
            c.set_execution_meter(None).unwrap();
            c.execute("ROLLBACK TO ddl").unwrap();
            c.execute("RELEASE ddl").unwrap();
            c.execute("COMMIT").unwrap();
            assert!(c.prepare("SELECT * FROM copied").is_err());
            let mut values = Vec::new();
            c.prepare("SELECT n FROM caller")
                .unwrap()
                .run_with_row_callback(|row| {
                    values.push(row.get::<i64>(0)?);
                    Ok(())
                })
                .unwrap();
            assert_eq!(values, [42]);
            assert_eq!(meter.snapshot(), work);
            c.execute("CREATE TABLE copied AS SELECT n FROM source")
                .unwrap();
        }
    }
}

#[test]
fn schema_limits_are_exact_and_sticky_across_maintenance_views() {
    let c = open(":memory:");
    fixture(&c);
    c.execute("CREATE TABLE other(n INTEGER)").unwrap();
    for limit in [0, 1, 2] {
        let root = Arc::new(ExecutionMeter::with_schema_read_limit(
            ExecutionLimits {
                max_rows_read: Some(0),
                ..Default::default()
            },
            Some(limit),
        ));
        let maintenance = Arc::new(root.without_row_mutations());
        c.set_execution_meter(Some(maintenance.clone())).unwrap();
        let result = run(&c, "SELECT count(*) FROM sqlite_schema");
        assert_eq!(result.is_ok(), limit == 2);
        assert_eq!(root.snapshot().schema_rows_read, (limit + 1).min(2));
        assert_eq!(root.snapshot().rows_read, 0);
        assert_eq!(root.snapshot().schema_budget_exhausted, limit < 2);
        assert_eq!(root.snapshot(), maintenance.snapshot());
        c.set_execution_meter(Some(root.clone())).unwrap();
        if limit < 2 {
            let before = root.snapshot();
            assert!(matches!(run(&c, "SELECT 1"), Err(LimboError::Interrupt)));
            assert_eq!(root.snapshot(), before);
        }
        c.set_execution_meter(None).unwrap();
    }
    // Legacy read budgets still include schema visits.
    let all = Arc::new(ExecutionMeter::with_limits(ExecutionLimits {
        max_rows_read: Some(1),
        ..Default::default()
    }));
    c.set_execution_meter(Some(all.clone())).unwrap();
    assert!(matches!(
        run(&c, "SELECT count(*) FROM sqlite_schema"),
        Err(LimboError::Interrupt)
    ));
    assert_eq!(all.snapshot().rows_read, 2);
    assert_eq!(all.snapshot().schema_rows_read, 2);
    assert!(all.snapshot().read_budget_exhausted);
    assert!(!all.snapshot().schema_budget_exhausted);
    c.set_execution_meter(None).unwrap();
}

#[test]
fn index_build_io_resumption_does_not_duplicate_source_or_schema_visits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queued-schema.db");
    let path = path.to_str().unwrap();
    let io = Arc::new(queued_io::QueuedIo::new());
    {
        let db = Database::open_file(
            io.clone(),
            path,
            std::sync::Arc::new(turso_core::SqliteDialect),
        )
        .unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE source(n INTEGER, payload BLOB)")
            .unwrap();
        c.execute("BEGIN").unwrap();
        for n in 0..100 {
            c.execute(format!("INSERT INTO source VALUES({n},zeroblob(1000))"))
                .unwrap();
        }
        c.execute("COMMIT").unwrap();
        c.execute("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    }
    let db = Database::open_file(
        io.clone(),
        path,
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap();
    let c = db.connect().unwrap();
    let mut statement = c.prepare("CREATE INDEX source_n ON source(n)").unwrap();
    let meter = Arc::new(ExecutionMeter::with_schema_read_limit(
        ExecutionLimits {
            max_rows_read: Some(100),
            ..Default::default()
        },
        Some(2),
    ));
    c.set_execution_meter(Some(meter.clone())).unwrap();
    let mut suspended = 0;
    loop {
        match statement.step().unwrap() {
            StepResult::Done => break,
            StepResult::Yield => continue,
            StepResult::IO => {
                suspended += 1;
                let before = meter.snapshot();
                for _ in 0..3 {
                    assert!(matches!(statement.step().unwrap(), StepResult::IO));
                    assert_eq!(meter.snapshot(), before);
                }
                assert!(io.step_one().unwrap().is_some());
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(suspended > 0);
    assert_eq!(meter.snapshot().rows_read, 100);
    assert_eq!(meter.snapshot().schema_rows_read, 2);
    assert_eq!(meter.snapshot().row_mutations, 0);
    c.set_execution_meter(None).unwrap();
}

#[test]
fn failed_unique_build_retains_reads_and_empty_build_needs_no_data_allowance() {
    let c = open(":memory:");
    fixture(&c);
    c.execute("UPDATE source SET n=1 WHERE n=2").unwrap();
    let meter = Arc::new(ExecutionMeter::with_schema_read_limit(
        ExecutionLimits::default(),
        None,
    ));
    c.set_execution_meter(Some(meter.clone())).unwrap();
    assert!(run(&c, "CREATE UNIQUE INDEX source_unique ON source(n)").is_err());
    let failed = meter.snapshot();
    assert_eq!(failed.rows_read, 5);
    assert_eq!(failed.row_mutations, 0);
    assert!(!failed.read_budget_exhausted && !failed.schema_budget_exhausted);
    c.set_execution_meter(None).unwrap();
    assert!(c
        .prepare("SELECT n FROM source INDEXED BY source_unique")
        .is_err());
    c.execute("DELETE FROM source").unwrap();
    for sql in [
        "CREATE INDEX source_n ON source(n)",
        "CREATE TABLE copied AS SELECT n FROM source",
        "CREATE TABLE empty(n INTEGER)",
        "CREATE TABLE IF NOT EXISTS empty(n INTEGER)",
    ] {
        let empty = Arc::new(ExecutionMeter::with_schema_read_limit(
            ExecutionLimits {
                max_rows_read: Some(0),
                max_row_mutations: Some(0),
                max_vm_steps: None,
            },
            Some(100),
        ));
        c.set_execution_meter(Some(empty.clone())).unwrap();
        run(&c, sql).unwrap();
        assert_eq!(empty.snapshot().rows_read, 0, "{sql}");
        assert_eq!(empty.snapshot().row_mutations, 0, "{sql}");
        c.set_execution_meter(None).unwrap();
    }
    assert_eq!(meter.snapshot(), failed);
}

#[test]
fn native_ddl_distinguishes_schema_changes_from_row_rewrites() {
    for (sql, reads, mutations) in [
        ("DROP INDEX source_n", 0, 0),
        ("DROP INDEX IF EXISTS missing", 0, 0),
        // Destroying the tree has no per-row events. The checked frontend must
        // account for the removed rows independently inside its atomic scope.
        ("DROP TABLE source", 0, 0),
        ("ALTER TABLE source RENAME TO renamed", 0, 0),
        ("ALTER TABLE source RENAME COLUMN n TO renamed", 0, 0),
        (
            "ALTER TABLE source ADD COLUMN added INTEGER DEFAULT 7",
            0,
            0,
        ),
        (
            "ALTER TABLE source ALTER COLUMN extra TO renamed TEXT",
            0,
            0,
        ),
        ("ALTER TABLE source DROP COLUMN extra", 5, 5),
    ] {
        let c = open(":memory:");
        c.execute("CREATE TABLE source(n INTEGER, extra TEXT)")
            .unwrap();
        c.execute("INSERT INTO source(n) VALUES(1),(2),(3),(4),(5)")
            .unwrap();
        c.execute("CREATE INDEX source_n ON source(n)").unwrap();
        let work = measure(&c, sql, true);
        assert_eq!(work.rows_read, reads, "{sql}: {work:?}");
        assert_eq!(work.row_mutations, mutations, "{sql}: {work:?}");
        assert!(work.vm_steps > 0);
    }
}

#[test]
fn mvcc_schema_writes_do_not_consume_customer_mutation_budgets() {
    let dir = tempfile::tempdir().unwrap();
    let c = open(dir.path().join("schema-mvcc.db").to_str().unwrap());
    c.execute("PRAGMA journal_mode=mvcc").unwrap();
    for sql in [
        "CREATE TABLE items(id INTEGER PRIMARY KEY, value TEXT)",
        "CREATE INDEX items_value ON items(value)",
        "ALTER TABLE items ADD COLUMN extra INTEGER",
        "ALTER TABLE items RENAME COLUMN extra TO renamed",
        "DROP INDEX items_value",
        "ALTER TABLE items RENAME TO renamed_items",
    ] {
        let meter = Arc::new(ExecutionMeter::with_schema_read_limit(
            ExecutionLimits {
                max_row_mutations: Some(0),
                ..Default::default()
            },
            None,
        ));
        c.set_execution_meter(Some(meter.clone())).unwrap();
        let result = run(&c, sql);
        c.set_execution_meter(None).unwrap();
        assert!(result.is_ok(), "{sql}: {result:?}, {:?}", meter.snapshot());
        assert_eq!(meter.snapshot().row_mutations, 0, "{sql}");
        assert!(!meter.snapshot().mutation_budget_exhausted);
    }
    // A real inserted row must still count, including before the first checkpoint
    // when the new table is represented solely by an MVCC table identity.
    let work = measure(&c, "INSERT INTO renamed_items VALUES(1,'one',1)", true);
    assert_eq!(work.row_mutations, 1);
    let work = measure(
        &c,
        "CREATE TABLE copied AS SELECT * FROM renamed_items",
        true,
    );
    assert_eq!(work.row_mutations, 1);
    assert_eq!(work.rows_read, 1);
    let work = measure(&c, "DELETE FROM copied WHERE id=1", true);
    assert_eq!(work.row_mutations, 1);
    assert_eq!(measure(&c, "DROP TABLE copied", true).row_mutations, 0);
    assert_eq!(
        measure(&c, "DROP TABLE renamed_items", true).row_mutations,
        0
    );
}
