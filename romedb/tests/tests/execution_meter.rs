use std::sync::Arc;
use turso_core::{execution_meter::ExecutionMeter, Connection, Database, LimboError, StepResult};

// Reuse the existing deterministic engine I/O harness without changing it.
#[allow(dead_code)]
#[path = "../../../tests/integration/queued_io.rs"]
mod queued_io;

// The upstream run_ignore_rows helper maps interruption to Busy. Use the
// callback helper, which preserves the StepResult::Interrupt distinction.
fn run(c: &Arc<Connection>, sql: &str) -> turso_core::Result<()> {
    c.prepare(sql)?.run_with_row_callback(|_| Ok(()))
}

fn connection(path: &str) -> Arc<Connection> {
    let db = Database::open_file(
        Database::io_for_path(path).unwrap(),
        path,
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap();
    let c = db.connect().unwrap();
    c.execute("CREATE TABLE input(n INTEGER)").unwrap();
    c.execute("INSERT INTO input VALUES (1),(2),(3),(4),(5)")
        .unwrap();
    c
}

fn exercise(path: &str) {
    let c = connection(path);
    let meter = Arc::new(ExecutionMeter::default());
    let mut query = c.prepare("SELECT n FROM input").unwrap();
    c.set_execution_meter(Some(meter.clone())).unwrap();
    query.run_ignore_rows().unwrap();
    assert_eq!(meter.snapshot().rows_read, 5);
    assert_eq!(meter.snapshot().vm_steps, query.metrics().vm_steps);
    let successful = meter.snapshot();
    // Partial reads remain available when expression evaluation fails.
    let mut failing = c
        .prepare("SELECT abs(CASE WHEN n=3 THEN -9223372036854775808 ELSE n END) FROM input")
        .unwrap();
    assert!(failing.run_ignore_rows().is_err());
    let failed = meter.snapshot();
    assert_eq!(failed.rows_read - successful.rows_read, 3);
    assert!(failed.vm_steps > successful.vm_steps);
    drop(failing);
    assert_eq!(meter.snapshot(), failed);
    c.set_execution_meter(None).unwrap();
    query.reset().unwrap();
    query.run_ignore_rows().unwrap();
    assert_eq!(meter.snapshot(), failed);

    // A reusable statement binds the new scope after reset, not at prepare time.
    let second = Arc::new(ExecutionMeter::default());
    c.set_execution_meter(Some(second.clone())).unwrap();
    query.reset().unwrap();
    query.run_ignore_rows().unwrap();
    assert_eq!(second.snapshot().rows_read, 5);
    assert_eq!(meter.snapshot(), failed);
    c.set_execution_meter(None).unwrap();
    drop(query);
    drop(c);
    assert_eq!(meter.snapshot(), failed);
}

#[test]
fn retained_metrics_cover_success_failure_reset_and_drop() {
    exercise(":memory:");
    let dir = tempfile::tempdir().unwrap();
    exercise(dir.path().join("meter.db").to_str().unwrap());
}

#[test]
fn active_statement_prevents_rebinding_and_drop_keeps_partial_reads() {
    let c = connection(":memory:");
    let meter = Arc::new(ExecutionMeter::default());
    c.set_execution_meter(Some(meter.clone())).unwrap();
    let mut query = c.prepare("SELECT n FROM input").unwrap();
    loop {
        match query.step().unwrap() {
            StepResult::IO => query._io().step().unwrap(),
            StepResult::Yield => continue,
            StepResult::Row => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(matches!(
        c.set_execution_meter(None),
        Err(LimboError::StatementsInProgress(_))
    ));
    assert_eq!(meter.snapshot().rows_read, 1);
    drop(query);
    c.set_execution_meter(None).unwrap();
    assert_eq!(meter.snapshot().rows_read, 1);
    c.execute("SELECT n FROM input").unwrap();
    assert_eq!(meter.snapshot().rows_read, 1);
}

#[test]
fn vm_budget_is_exact_shared_across_statements_and_interrupts_writes() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        ":memory:".to_string(),
        dir.path().join("budget.db").to_str().unwrap().to_string(),
    ] {
        let c = connection(&path);
        c.execute("CREATE TABLE output(n INTEGER)").unwrap();
        let meter = Arc::new(ExecutionMeter::new(Some(100)));
        c.set_execution_meter(Some(meter.clone())).unwrap();
        c.execute("SELECT n FROM input").unwrap();
        let before = meter.snapshot();
        assert!(before.vm_steps > 0 && before.vm_steps < 100);
        let mut write = c
            .prepare("INSERT INTO output SELECT a.n FROM input a,input b,input d")
            .unwrap();
        assert!(matches!(
            write.run_with_row_callback(|_| Ok(())),
            Err(LimboError::Interrupt)
        ));
        assert_eq!(meter.snapshot().vm_steps, 100);
        assert!(meter.snapshot().vm_budget_exhausted);
        assert!(meter.snapshot().rows_read > before.rows_read);
        assert!(
            meter.snapshot().rows_written > 0,
            "exercise rollback after actual writes"
        );
        let final_counts = meter.snapshot();
        assert!(matches!(run(&c, "SELECT 1"), Err(LimboError::Interrupt)));
        assert_eq!(meter.snapshot(), final_counts);
        drop(write);
        c.set_execution_meter(None).unwrap();
        let mut rows = 0;
        c.prepare("SELECT n FROM output")
            .unwrap()
            .run_with_row_callback(|_| {
                rows += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(rows, 0, "interrupted writes must roll back");
        c.execute("INSERT INTO output VALUES(9)").unwrap();
        assert_eq!(meter.snapshot(), final_counts);
        let zero = Arc::new(ExecutionMeter::new(Some(0)));
        c.set_execution_meter(Some(zero.clone())).unwrap();
        assert!(matches!(
            run(&c, "SELECT n FROM input"),
            Err(LimboError::Interrupt)
        ));
        assert_eq!(zero.snapshot().vm_steps, 0);
        assert_eq!(zero.snapshot().rows_read, 0);
        assert!(zero.snapshot().vm_budget_exhausted);
    }
}

#[test]
fn physical_write_counts_survive_explicit_rollback() {
    let c = connection(":memory:");
    let meter = Arc::new(ExecutionMeter::default());
    c.set_execution_meter(Some(meter.clone())).unwrap();
    c.execute("BEGIN").unwrap();
    c.execute("INSERT INTO input VALUES(6),(7)").unwrap();
    let written = meter.snapshot();
    assert!(written.rows_written >= 2);
    c.execute("ROLLBACK").unwrap();
    assert_eq!(meter.snapshot().rows_written, written.rows_written);
    c.set_execution_meter(None).unwrap();
    let mut rows = 0;
    c.prepare("SELECT n FROM input")
        .unwrap()
        .run_with_row_callback(|_| {
            rows += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(rows, 5);
}

#[test]
fn explain_modes_obey_zero_budget() {
    let c = connection(":memory:");
    for sql in [
        "EXPLAIN SELECT n FROM input",
        "EXPLAIN QUERY PLAN SELECT n FROM input",
    ] {
        let meter = Arc::new(ExecutionMeter::new(Some(0)));
        c.set_execution_meter(Some(meter.clone())).unwrap();
        assert!(matches!(run(&c, sql), Err(LimboError::Interrupt)));
        assert_eq!(meter.snapshot().vm_steps, 0);
        assert!(meter.snapshot().vm_budget_exhausted);
        c.set_execution_meter(None).unwrap();
    }
}

#[test]
fn exact_completed_statement_budget_succeeds_and_one_less_interrupts() {
    let c = connection(":memory:");
    let mut statement = c.prepare("SELECT n FROM input").unwrap();
    let baseline = Arc::new(ExecutionMeter::default());
    c.set_execution_meter(Some(baseline.clone())).unwrap();
    statement.run_with_row_callback(|_| Ok(())).unwrap();
    let required = baseline.snapshot().vm_steps;
    assert!(required > 0);
    for (limit, succeeds) in [(required, true), (required - 1, false)] {
        let meter = Arc::new(ExecutionMeter::new(Some(limit)));
        c.set_execution_meter(Some(meter.clone())).unwrap();
        statement.reset().unwrap();
        let result = statement.run_with_row_callback(|_| Ok(()));
        assert_eq!(result.is_ok(), succeeds);
        if !succeeds {
            assert!(matches!(result, Err(LimboError::Interrupt)));
        }
        assert_eq!(meter.snapshot().vm_steps, limit);
        assert_eq!(meter.snapshot().vm_budget_exhausted, !succeeds);
    }
}

fn queued_fixture(path: &str) -> Arc<queued_io::QueuedIo> {
    let io = Arc::new(queued_io::QueuedIo::new());
    {
        let db = Database::open_file(
            io.clone(),
            path,
            std::sync::Arc::new(turso_core::SqliteDialect),
        )
        .unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE input(n INTEGER PRIMARY KEY, payload BLOB)")
            .unwrap();
        c.execute("BEGIN").unwrap();
        for n in 0..100 {
            c.execute(format!("INSERT INTO input VALUES({n},zeroblob(1000))"))
                .unwrap();
        }
        c.execute("COMMIT").unwrap();
        c.execute("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    }
    io
}

#[test]
fn pending_io_polls_do_not_charge_additional_work() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("meter-queued.db");
    let path = file.to_str().unwrap();
    let io = queued_fixture(path);
    for (sql, expected_rows) in [
        ("SELECT n FROM input", 100),
        ("SELECT count(*) FROM input", 1),
    ] {
        let db = Database::open_file(
            io.clone(),
            path,
            std::sync::Arc::new(turso_core::SqliteDialect),
        )
        .unwrap();
        let c = db.connect().unwrap();
        let mut statement = c.prepare(sql).unwrap();
        let meter = Arc::new(ExecutionMeter::default());
        c.set_execution_meter(Some(meter.clone())).unwrap();
        let mut rows = 0;
        let mut suspended = 0;
        loop {
            match statement.step().unwrap() {
                StepResult::Row => rows += 1,
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
        assert_eq!(rows, expected_rows);
        assert_eq!(meter.snapshot().rows_read, 100);
        assert_eq!(meter.snapshot().vm_steps, statement.metrics().vm_steps);
    }
}

fn read_meter(limit: u64) -> Arc<ExecutionMeter> {
    Arc::new(ExecutionMeter::with_limits(
        turso_core::execution_meter::ExecutionLimits {
            max_rows_read: Some(limit),
            max_row_mutations: None,
            max_vm_steps: None,
        },
    ))
}

#[test]
fn read_budget_bounds_scan_count_and_index_visits_and_keeps_failed_work() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        ":memory:".to_string(),
        dir.path()
            .join("read-budget.db")
            .to_str()
            .unwrap()
            .to_string(),
    ] {
        let c = connection(&path);
        c.execute("CREATE INDEX input_n ON input(n)").unwrap();
        for sql in [
            "SELECT n FROM input NOT INDEXED",
            "SELECT count(*) FROM input",
            "SELECT n FROM input WHERE n>=1 ORDER BY n",
        ] {
            let mut statement = c.prepare(sql).unwrap();
            for limit in [0, 3, 5] {
                let meter = read_meter(limit);
                c.set_execution_meter(Some(meter.clone())).unwrap();
                let result = statement.run_with_row_callback(|_| Ok(()));
                if limit < 5 {
                    assert!(
                        matches!(result, Err(LimboError::Interrupt)),
                        "{sql}: {result:?}"
                    );
                    assert_eq!(meter.snapshot().rows_read, limit + 1, "{sql}");
                    assert!(meter.snapshot().read_budget_exhausted);
                    let stopped = meter.snapshot();
                    assert!(matches!(run(&c, "SELECT 1"), Err(LimboError::Interrupt)));
                    assert_eq!(
                        meter.snapshot(),
                        stopped,
                        "exhausted scope must not restart"
                    );
                } else {
                    result.unwrap();
                    assert_eq!(meter.snapshot().rows_read, 5, "{sql}");
                    assert!(!meter.snapshot().read_budget_exhausted);
                }
                assert!(!meter.snapshot().vm_budget_exhausted);
                c.set_execution_meter(None).unwrap();
                statement.reset().unwrap();
            }
        }
        c.execute("INSERT INTO input VALUES(6)").unwrap();
    }
}

#[test]
fn read_budget_interrupts_mutation_and_retains_reads_across_rollback() {
    let c = connection(":memory:");
    c.execute("CREATE TABLE output(n INTEGER)").unwrap();
    let meter = read_meter(3);
    c.set_execution_meter(Some(meter.clone())).unwrap();
    assert!(matches!(
        run(&c, "INSERT INTO output SELECT n FROM input"),
        Err(LimboError::Interrupt)
    ));
    let counts = meter.snapshot();
    assert_eq!(counts.rows_read, 4);
    assert!(counts.rows_written > 0);
    c.set_execution_meter(None).unwrap();
    let mut rows = 0;
    c.prepare("SELECT n FROM output")
        .unwrap()
        .run_with_row_callback(|_| {
            rows += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(rows, 0);
    assert_eq!(meter.snapshot(), counts);
    c.execute("INSERT INTO output VALUES(9)").unwrap();
}

#[test]
fn metered_count_preserves_results_and_reset_discards_partial_count() {
    let c = connection(":memory:");
    let mut statement = c.prepare("SELECT count(*) FROM input").unwrap();
    let exhausted = read_meter(2);
    c.set_execution_meter(Some(exhausted)).unwrap();
    assert!(matches!(
        statement.run_with_row_callback(|_| Ok(())),
        Err(LimboError::Interrupt)
    ));
    statement.reset().unwrap();
    let fresh = read_meter(5);
    c.set_execution_meter(Some(fresh.clone())).unwrap();
    let mut results = Vec::new();
    statement
        .run_with_row_callback(|row| {
            results.push(row.get_values().next().unwrap().to_string());
            Ok(())
        })
        .unwrap();
    assert_eq!(results, vec!["5"]);
    assert_eq!(fresh.snapshot().rows_read, 5);
    c.set_execution_meter(None).unwrap();
    c.execute("DELETE FROM input").unwrap();
    let empty = read_meter(0);
    c.set_execution_meter(Some(empty.clone())).unwrap();
    statement.reset().unwrap();
    statement
        .run_with_row_callback(|row| {
            assert_eq!(row.get_values().next().unwrap().to_string(), "0");
            Ok(())
        })
        .unwrap();
    assert_eq!(empty.snapshot().rows_read, 0);
    assert!(!empty.snapshot().read_budget_exhausted);
}

#[test]
fn read_budget_counts_index_and_deferred_table_visits_but_not_missing_keys() {
    let c = connection(":memory:");
    c.execute("CREATE TABLE points(id INTEGER PRIMARY KEY,n INTEGER,payload TEXT)")
        .unwrap();
    c.execute("INSERT INTO points VALUES(1,10,'value')")
        .unwrap();
    c.execute("CREATE INDEX points_n ON points(n)").unwrap();
    for (key, limit, succeeds, reads) in [(10, 2, true, 2), (10, 1, false, 2), (99, 0, true, 0)] {
        let mut statement = c
            .prepare(format!(
                "SELECT payload FROM points INDEXED BY points_n WHERE n={key}"
            ))
            .unwrap();
        let meter = read_meter(limit);
        c.set_execution_meter(Some(meter.clone())).unwrap();
        let result = statement.run_with_row_callback(|_| Ok(()));
        assert_eq!(result.is_ok(), succeeds, "{result:?}");
        assert_eq!(meter.snapshot().rows_read, reads);
        assert_eq!(meter.snapshot().read_budget_exhausted, !succeeds);
        c.set_execution_meter(None).unwrap();
    }
}

#[test]
fn metered_count_retains_partial_reads_on_io_failure() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("meter-queued.db");
    let path = file.to_str().unwrap();
    let io = queued_fixture(path);
    let db = Database::open_file(
        io.clone(),
        path,
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap();
    let c = db.connect().unwrap();
    let mut statement = c.prepare("SELECT count(*) FROM input").unwrap();
    io.fault_after(path, queued_io::QueuedIoOpKind::Pread, 2);
    let meter = Arc::new(ExecutionMeter::default());
    c.set_execution_meter(Some(meter.clone())).unwrap();
    assert!(statement.run_with_row_callback(|_| Ok(())).is_err());
    let failed = meter.snapshot();
    assert!(failed.rows_read > 0 && failed.rows_read < 100, "{failed:?}");
    assert!(!failed.read_budget_exhausted);
    io.clear_fault();
    drop(statement);
    c.set_execution_meter(None).unwrap();
    c.prepare("SELECT count(*) FROM input")
        .unwrap()
        .run_with_row_callback(|row| {
            assert_eq!(row.get_values().next().unwrap().to_string(), "100");
            Ok(())
        })
        .unwrap();
    assert_eq!(meter.snapshot(), failed);
}

#[test]
fn hash_join_read_budget_uses_source_visits_not_hash_copies() {
    let c = connection(":memory:");
    c.execute("CREATE TABLE hash_left(n INTEGER)").unwrap();
    c.execute("CREATE TABLE hash_right(n INTEGER)").unwrap();
    c.execute("BEGIN").unwrap();
    for n in 0..100 {
        c.execute(format!("INSERT INTO hash_left VALUES({n})"))
            .unwrap();
        c.execute(format!("INSERT INTO hash_right VALUES({n})"))
            .unwrap();
    }
    c.execute("COMMIT").unwrap();
    let sql = "SELECT count(*) FROM hash_left a JOIN hash_right b ON a.n=b.n";
    let mut hash_build = false;
    c.prepare(format!("EXPLAIN {sql}"))
        .unwrap()
        .run_with_row_callback(|row| {
            hash_build |= row.get_values().nth(1).unwrap().to_string() == "HashBuild";
            Ok(())
        })
        .unwrap();
    assert!(hash_build);
    let mut query = c.prepare(sql).unwrap();
    for limit in [200, 199] {
        let meter = read_meter(limit);
        c.set_execution_meter(Some(meter.clone())).unwrap();
        let result = query.run_with_row_callback(|row| {
            assert_eq!(row.get_values().next().unwrap().to_string(), "100");
            Ok(())
        });
        if limit == 200 {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(LimboError::Interrupt)));
        }
        assert_eq!(meter.snapshot().rows_read, 200);
        assert_eq!(meter.snapshot().read_budget_exhausted, limit == 199);
        c.set_execution_meter(None).unwrap();
        query.reset().unwrap();
    }
}

#[test]
fn read_budget_interrupt_preserves_prior_explicit_transaction_work() {
    let c = connection(":memory:");
    c.execute("BEGIN").unwrap();
    c.execute("INSERT INTO input VALUES(6)").unwrap();
    let meter = read_meter(1);
    c.set_execution_meter(Some(meter.clone())).unwrap();
    assert!(matches!(
        run(&c, "SELECT n FROM input"),
        Err(LimboError::Interrupt)
    ));
    assert_eq!(meter.snapshot().rows_read, 2);
    c.set_execution_meter(None).unwrap();
    assert!(!c.get_auto_commit());
    let mut rows = 0;
    c.prepare("SELECT n FROM input")
        .unwrap()
        .run_with_row_callback(|_| {
            rows += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(rows, 6);
    c.execute("ROLLBACK").unwrap();
    let mut rows = 0;
    c.prepare("SELECT n FROM input")
        .unwrap()
        .run_with_row_callback(|_| {
            rows += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(rows, 5);
}
