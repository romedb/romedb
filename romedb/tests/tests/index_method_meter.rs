use std::sync::Arc;
use turso_core::{
    execution_meter::{ExecutionLimits, ExecutionMeter},
    Connection, Database, DatabaseOpts, LimboError, OpenFlags, StepResult,
};

#[allow(dead_code)]
#[path = "../../../tests/integration/queued_io.rs"]
mod queued_io;

const COVERED: &str = "SELECT fts_score(body,'hello') AS score FROM docs WHERE fts_match(body,'hello') ORDER BY score DESC LIMIT 10";
const LOOKUPS: &str = "SELECT id,body,fts_score(body,'hello') AS score FROM docs WHERE fts_match(body,'hello') ORDER BY score DESC LIMIT 10";

fn open(io: Arc<dyn turso_core::IO>, path: &str) -> Arc<Connection> {
    Database::open_file_with_flags(
        io,
        path,
        OpenFlags::default(),
        DatabaseOpts::new().with_index_method(true),
        None,
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap()
    .connect()
    .unwrap()
}
fn seed(c: &Arc<Connection>, count: usize) {
    c.execute("CREATE TABLE docs(id INTEGER PRIMARY KEY, body TEXT)")
        .unwrap();
    c.execute("CREATE INDEX docs_fts ON docs USING fts(body)")
        .unwrap();
    for n in 0..count {
        c.execute(format!("INSERT INTO docs VALUES({n},'hello')"))
            .unwrap();
    }
}
fn run(c: &Arc<Connection>, sql: &str) -> turso_core::Result<usize> {
    let mut rows = 0;
    c.prepare(sql)?.run_with_row_callback(|_| {
        rows += 1;
        Ok(())
    })?;
    Ok(rows)
}
fn meter(cap: Option<u64>) -> Arc<ExecutionMeter> {
    Arc::new(ExecutionMeter::with_limits(ExecutionLimits {
        max_rows_read: cap,
        ..Default::default()
    }))
}
fn opcodes(c: &Arc<Connection>, sql: &str) -> Vec<String> {
    let mut result = Vec::new();
    c.prepare(format!("EXPLAIN {sql}"))
        .unwrap()
        .run_with_row_callback(|row| {
            result.push(row.get_values().nth(1).unwrap().to_string());
            Ok(())
        })
        .unwrap();
    result
}

#[test]
fn first_index_method_position_counts_with_and_without_table_lookups() {
    let dir = tempfile::tempdir().unwrap();
    for disk in [false, true] {
        for n in [0, 1, 5] {
            let path = if disk {
                dir.path()
                    .join(format!("fts-{n}.db"))
                    .to_str()
                    .unwrap()
                    .to_owned()
            } else {
                ":memory:".into()
            };
            let c = open(Database::io_for_path(&path).unwrap(), &path);
            seed(&c, n);
            for (sql, per_match) in [(COVERED, 1), (LOOKUPS, 2)] {
                let plan = opcodes(&c, sql);
                assert!(plan.iter().any(|s| s == "IndexMethodQuery"), "{plan:?}");
                assert!(plan.iter().any(|s| s == "DeferredSeek"), "{plan:?}");
                for metered in [false, true] {
                    let m = meter(None);
                    if metered {
                        c.set_execution_meter(Some(m.clone())).unwrap();
                    }
                    let mut query = c.prepare(sql).unwrap();
                    let mut rows = 0;
                    query
                        .run_with_row_callback(|_| {
                            rows += 1;
                            Ok(())
                        })
                        .unwrap();
                    assert_eq!(rows, n);
                    assert_eq!(
                        query.metrics().rows_read,
                        n as u64 * per_match,
                        "{sql}; metered={metered}; plan={plan:?}"
                    );
                    if metered {
                        assert_eq!(m.snapshot().rows_read, n as u64 * per_match);
                    }
                    c.set_execution_meter(None).unwrap();
                }
            }
            let missing = COVERED.replace("'hello'", "'absent'");
            let m = meter(Some(0));
            c.set_execution_meter(Some(m.clone())).unwrap();
            assert_eq!(run(&c, &missing).unwrap(), 0);
            assert_eq!(m.snapshot().rows_read, 0);
            c.set_execution_meter(None).unwrap();
        }
    }
}

#[test]
fn first_and_later_fts_positions_share_sticky_read_budgets_and_preserve_caller_work() {
    let c = open(Database::io_for_path(":memory:").unwrap(), ":memory:");
    seed(&c, 5);
    c.execute("CREATE TABLE pending(n INTEGER)").unwrap();
    c.execute("BEGIN").unwrap();
    c.execute("INSERT INTO pending VALUES(42)").unwrap();
    for (sql, visits) in [(COVERED, 5), (LOOKUPS, 10)] {
        for cap in 0..visits {
            let m = meter(Some(cap));
            c.set_execution_meter(Some(m.clone())).unwrap();
            assert!(
                matches!(run(&c, sql), Err(LimboError::Interrupt)),
                "{sql}: cap={cap}"
            );
            let snapshot = m.snapshot();
            assert_eq!(snapshot.rows_read, cap + 1);
            assert!(snapshot.read_budget_exhausted);
            assert!(matches!(run(&c, sql), Err(LimboError::Interrupt)));
            assert_eq!(m.snapshot(), snapshot);
            c.set_execution_meter(None).unwrap();
            assert!(!c.get_auto_commit());
            assert_eq!(run(&c, "SELECT * FROM pending").unwrap(), 1);
        }
        let m = meter(Some(visits));
        c.set_execution_meter(Some(m.clone())).unwrap();
        assert_eq!(run(&c, sql).unwrap(), 5);
        assert_eq!(m.snapshot().rows_read, visits);
        c.set_execution_meter(None).unwrap();
    }
    c.execute("ROLLBACK").unwrap();
    assert_eq!(run(&c, "SELECT * FROM pending").unwrap(), 0);
}

#[test]
fn queued_fts_io_does_not_repeat_completed_positions() {
    let io = Arc::new(queued_io::QueuedIo::new());
    let path = "fts-meter-queued.db";
    {
        let c = open(io.clone(), path);
        seed(&c, 5);
        c.execute("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    }
    for cap in [None, Some(0), Some(7), Some(10)] {
        let c = open(io.clone(), path);
        let m = meter(cap);
        let mut query = c.prepare(LOOKUPS).unwrap();
        c.set_execution_meter(Some(m.clone())).unwrap();
        let mut rows = 0;
        let mut suspended = 0;
        let mut interrupted = false;
        loop {
            match query.step() {
                Ok(StepResult::Row) => rows += 1,
                Ok(StepResult::Done) => break,
                Ok(StepResult::Yield) => continue,
                Ok(StepResult::IO) => {
                    suspended += 1;
                    let before = m.snapshot();
                    for _ in 0..3 {
                        assert!(matches!(query.step().unwrap(), StepResult::IO));
                        assert_eq!(m.snapshot(), before);
                    }
                    assert!(io.step_one().unwrap().is_some());
                }
                Ok(StepResult::Interrupt) | Err(LimboError::Interrupt) => {
                    interrupted = true;
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(suspended > 0);
        let fails = cap.is_some_and(|n| n < 10);
        assert_eq!(interrupted, fails);
        assert_eq!(
            m.snapshot().rows_read,
            if fails { cap.unwrap() + 1 } else { 10 }
        );
        if !fails {
            assert_eq!(rows, 5);
        }
        drop(query);
        c.set_execution_meter(None).unwrap();
        assert_eq!(run(&c, LOOKUPS).unwrap(), 5);
    }
}

#[test]
fn managed_fulltext_counts_index_table_and_materialized_visits_separately() {
    let c = romedb::Database::open(":memory:")
        .unwrap()
        .connect()
        .unwrap();
    let p = romedb::Parameters::new();
    c.execute("CREATE TABLE docs", &p).unwrap();
    for n in 0..5 {
        c.execute(
            &format!("INSERT INTO docs {{id:docs:d{n},title:'hello'}}"),
            &p,
        )
        .unwrap();
    }
    c.execute(
        "CREATE SEARCH INDEX docs_fts ON docs(title) USING FULLTEXT",
        &p,
    )
    .unwrap();
    for limit in [1, 5] {
        let sql = format!("SELECT id FROM search::text('docs_fts','hello',{limit})");
        let plan = c.execute(&format!("EXPLAIN QUERY PLAN {sql}"), &p).unwrap();
        assert!(format!("{plan:?}").contains("QUERY INDEX METHOD fts"));
        // Stable score/ID ordering scans all five materialized hits before LIMIT.
        let expected = 15;
        for cap in [0, 4, 9, expected - 1, expected] {
            let m = c.select_metered(
                &sql,
                &p,
                romedb::ResultLimits {
                    max_rows: 100,
                    max_payload_bytes: 65536,
                },
                romedb::ReadWorkLimits {
                    max_rows_read: Some(cap),
                    max_vm_steps: None,
                },
            );
            if cap == expected {
                assert_eq!(m.outcome.unwrap().rows.len(), limit as usize);
                assert_eq!(m.work.rows_read, expected);
            } else {
                assert_eq!(m.outcome.unwrap_err().code(), "FDB_CANCELLED");
                assert_eq!(m.work.rows_read, cap + 1);
                assert!(m.work.read_budget_exhausted);
            }
        }
    }
}
