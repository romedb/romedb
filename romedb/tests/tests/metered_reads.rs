use romedb::InfoWorkLimits;
use romedb::{Database, Parameters, ReadWorkLimits, ResultLimits, TransactionState, Value};

fn limits() -> ResultLimits {
    ResultLimits {
        max_rows: 100,
        max_payload_bytes: 64 * 1024,
    }
}
fn reads(max: u64) -> ReadWorkLimits {
    ReadWorkLimits {
        max_rows_read: Some(max),
        max_vm_steps: None,
    }
}
fn setup(path: &str) -> romedb::Connection {
    let db = Database::open(path).unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE input(n INTEGER)",
        "INSERT INTO input VALUES(1),(2),(3),(4),(5)",
        "CREATE TABLE docs",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
    ] {
        c.execute(sql, &Parameters::new()).unwrap();
    }
    c
}

#[test]
fn checked_reads_retain_failure_work_and_exclude_catalog_queries() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        ":memory:".to_string(),
        dir.path().join("read.db").to_str().unwrap().to_string(),
    ] {
        let c = setup(&path);
        let p = Parameters::new();
        for sql in ["SELECT n FROM input", "SELECT count(*) FROM input"] {
            let result = c.select_metered(sql, &p, limits(), reads(5));
            assert_eq!(
                result.outcome.unwrap().rows,
                c.execute(sql, &p).unwrap().rows
            );
            assert_eq!(result.work.rows_read, 5);
            assert!(!result.work.read_budget_exhausted);
            let failed = c.select_metered(sql, &p, limits(), reads(2));
            assert_eq!(failed.outcome.unwrap_err().code(), "FDB_CANCELLED");
            assert_eq!(failed.work.rows_read, 3);
            assert!(failed.work.read_budget_exhausted);
        }
        for index in 0..10 {
            c.execute(&format!("CREATE TABLE extra_{index}"), &p)
                .unwrap();
        }
        let docs = c.select_metered("SELECT n FROM docs ORDER BY n", &p, limits(), reads(2));
        assert_eq!(docs.outcome.unwrap().rows.len(), 2);
        assert_eq!(
            docs.work.rows_read, 2,
            "catalog scans must not consume customer reads"
        );
        let failed = c.select_metered(
            "SELECT abs(CASE WHEN n=3 THEN -9223372036854775808 ELSE n END) FROM input",
            &p,
            limits(),
            reads(100),
        );
        assert!(failed.outcome.is_err());
        assert_eq!(failed.work.rows_read, 3);
        assert!(!failed.work.read_budget_exhausted);
        assert_eq!(c.execute("SELECT n FROM input", &p).unwrap().rows.len(), 5);
    }
}

#[test]
fn result_budget_and_read_budget_errors_leave_outer_transaction_usable() {
    let c = setup(":memory:");
    let p = Parameters::new();
    c.execute("BEGIN", &p).unwrap();
    c.execute("INSERT INTO docs {id:docs:pending,n:3}", &p)
        .unwrap();
    let limited = c.select_metered(
        "SELECT n FROM docs",
        &p,
        ResultLimits {
            max_rows: 0,
            ..limits()
        },
        reads(100),
    );
    assert_eq!(limited.outcome.unwrap_err().code(), "FDB_LIMIT");
    assert_eq!(limited.work.rows_read, 1);
    let stopped = c.select_metered("SELECT n FROM docs", &p, limits(), reads(0));
    assert!(stopped.work.read_budget_exhausted);
    assert_eq!(stopped.work.rows_read, 1);
    assert!(stopped.outcome.is_err());
    assert_eq!(c.transaction_state(), TransactionState::Active);
    assert_eq!(
        c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(3)]
        ]
    );
    c.execute("ROLLBACK", &p).unwrap();
    assert_eq!(c.execute("SELECT n FROM docs", &p).unwrap().rows.len(), 2);
}

#[test]
fn forward_fetch_work_is_shared_and_cleanup_survives_exhaustion() {
    let c = setup(":memory:");
    let p = Parameters::new();
    c.execute("CREATE TABLE refs", &p).unwrap();
    c.execute("INSERT INTO refs {id:refs:a,target:docs:a}", &p)
        .unwrap();
    c.execute("INSERT INTO refs {id:refs:b,target:docs:b}", &p)
        .unwrap();
    let sql = "SELECT record::fetch(target) FROM refs ORDER BY id";
    let profile = c.profile_select(sql, &p).unwrap();
    assert!(profile.metrics.fetch_rows_read > 0);
    let expected = profile.metrics.rows_read + profile.metrics.fetch_rows_read;
    let completed = c.select_metered(sql, &p, limits(), reads(expected));
    assert_eq!(completed.outcome.unwrap().rows, profile.result.rows);
    assert_eq!(completed.work.rows_read, expected);
    c.execute("BEGIN", &p).unwrap();
    c.execute("INSERT INTO input VALUES(6)", &p).unwrap();
    let stopped = c.select_metered(sql, &p, limits(), reads(profile.metrics.rows_read));
    assert!(stopped.outcome.is_err());
    assert_eq!(stopped.work.rows_read, profile.metrics.rows_read + 1);
    assert!(stopped.work.read_budget_exhausted);
    assert_eq!(c.transaction_state(), TransactionState::Active);
    assert_eq!(c.execute("SELECT n FROM input", &p).unwrap().rows.len(), 6);
    c.execute("ROLLBACK", &p).unwrap();
    assert!(c
        .select_metered(sql, &p, limits(), reads(expected))
        .outcome
        .is_ok());
}

#[test]
fn checked_meter_rejects_writes_and_zero_vm_budget_preserves_reuse() {
    let c = setup(":memory:");
    let p = Parameters::new();
    let rejected = c.select_metered("DELETE FROM input", &p, limits(), reads(100));
    assert!(rejected.outcome.is_err());
    assert_eq!(rejected.work.rows_read, 0);
    assert_eq!(rejected.work.vm_steps, 0);
    let stopped = c.select_metered(
        "SELECT n FROM input",
        &p,
        limits(),
        ReadWorkLimits {
            max_rows_read: None,
            max_vm_steps: Some(0),
        },
    );
    assert!(stopped.outcome.is_err());
    assert!(stopped.work.vm_budget_exhausted);
    assert_eq!(stopped.work.vm_steps, 0);
    assert_eq!(stopped.work.rows_read, 0);
    assert_eq!(
        c.select_metered("SELECT n FROM input", &p, limits(), reads(5))
            .outcome
            .unwrap()
            .rows
            .len(),
        5
    );
}

#[test]
fn direct_records_share_checked_result_and_work_limits() {
    let c = setup(":memory:");
    let p = Parameters::new();
    let expected = c.execute("SELECT docs:a", &p).unwrap();
    let found = c.select_metered("SELECT docs:a", &p, limits(), reads(2));
    let result = found.outcome.unwrap();
    assert_eq!(result.rows, expected.rows);
    assert_eq!(result.columns, expected.columns);
    assert_eq!(found.work.rows_read, 2);
    let missing = c.select_metered("SELECT docs:missing", &p, limits(), reads(0));
    assert!(missing.outcome.unwrap().rows.is_empty());
    assert_eq!(missing.work.rows_read, 0);
    let stopped = c.select_metered("SELECT docs:a", &p, limits(), reads(1));
    assert!(stopped.outcome.is_err());
    assert_eq!(stopped.work.rows_read, 2);
    assert!(stopped.work.read_budget_exhausted);
    let result_limited = c.select_metered(
        "SELECT docs:a",
        &p,
        ResultLimits {
            max_rows: 0,
            ..limits()
        },
        reads(2),
    );
    assert_eq!(result_limited.outcome.unwrap_err().code(), "FDB_LIMIT");
    assert_eq!(result_limited.work.rows_read, 2);
    assert!(c
        .select_metered("SELECT docs:a", &p, limits(), reads(2))
        .outcome
        .is_ok());
}

#[test]
fn inverse_fetch_and_native_forward_targets_share_the_statement_budget() {
    let c = setup(":memory:");
    let p = Parameters::new();
    for sql in [
        "CREATE TABLE native_targets(id INTEGER PRIMARY KEY,label TEXT)",
        "INSERT INTO native_targets VALUES(1,'Native')",
        "INSERT INTO refs {id:refs:a,target:docs:a}",
        "INSERT INTO refs {id:refs:b,target:type::record('native_targets',1)}",
        "CREATE INDEX refs_target ON refs(target)",
        "DEFINE RELATION referenced ON docs FROM refs.target",
    ] {
        c.execute(sql, &p).unwrap();
    }
    for sql in [
        "SELECT record::fetch(target) FROM refs ORDER BY id",
        "SELECT relation::fetch(docs:a,'referenced')",
    ] {
        let profile = c.profile_select(sql, &p).unwrap();
        assert!(profile.metrics.fetch_rows_read > 0);
        let expected = profile.metrics.rows_read + profile.metrics.fetch_rows_read;
        let result = c.select_metered(sql, &p, limits(), reads(expected));
        assert_eq!(result.outcome.unwrap().rows, profile.result.rows);
        assert_eq!(result.work.rows_read, expected);
        let stopped = c.select_metered(sql, &p, limits(), reads(expected - 1));
        assert!(stopped.outcome.is_err());
        assert_eq!(stopped.work.rows_read, expected);
        assert!(stopped.work.read_budget_exhausted);
        assert!(c
            .select_metered(sql, &p, limits(), reads(expected))
            .outcome
            .is_ok());
    }
}

#[test]
fn info_meter_retains_catalog_work_without_visiting_customer_rows() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        ":memory:".to_string(),
        dir.path().join("info.db").to_str().unwrap().to_string(),
    ] {
        let c = setup(&path);
        let p = Parameters::new();
        for sql in [
            "CREATE INDEX input_n ON input(n)",
            "CREATE INDEX docs_n ON docs(n)",
            "CREATE TABLE refs",
            "CREATE INDEX refs_target ON refs(target)",
            "DEFINE RELATION referenced ON docs FROM refs.target",
            "CREATE FUNCTION app::hello() RETURNS string LANGUAGE JAVASCRIPT AS 'return \"hello\";'",
        ] { c.execute(sql, &p).unwrap(); }
        for sql in [
            "INFO FOR DB",
            "INFO FOR TABLE input",
            "INFO FOR INDEX input_n",
            "INFO FOR TABLE docs",
            "INFO FOR INDEX docs_n",
            "INFO FOR RELATION referenced",
            "INFO FOR FUNCTION app::hello",
        ] {
            let expected = c.execute(sql, &p).unwrap();
            let result = c.info_metered(sql, &p, limits(), InfoWorkLimits::default());
            assert_eq!(result.outcome.unwrap().rows, expected.rows, "{sql}");
            assert!(result.work.catalog_rows_read > 0, "{sql}");
            assert!(result.work.vm_steps > 0, "{sql}");
            let exact = c.info_metered(
                sql,
                &p,
                limits(),
                InfoWorkLimits {
                    max_catalog_rows_read: Some(result.work.catalog_rows_read),
                    max_vm_steps: None,
                },
            );
            assert!(exact.outcome.is_ok(), "{sql}: {:?}", exact.outcome);
            let stopped = c.info_metered(
                sql,
                &p,
                limits(),
                InfoWorkLimits {
                    max_catalog_rows_read: Some(result.work.catalog_rows_read - 1),
                    max_vm_steps: None,
                },
            );
            assert!(stopped.outcome.is_err(), "{sql}");
            assert!(stopped.work.catalog_budget_exhausted);
            assert_eq!(
                stopped.work.catalog_rows_read,
                result.work.catalog_rows_read
            );
        }
        let before = c.info_metered("INFO FOR DB", &p, limits(), InfoWorkLimits::default());
        c.execute("INSERT INTO input SELECT n+10 FROM input", &p)
            .unwrap();
        let after = c.info_metered("INFO FOR DB", &p, limits(), InfoWorkLimits::default());
        assert_eq!(before.outcome.unwrap().rows, after.outcome.unwrap().rows);
        assert_eq!(before.work.catalog_rows_read, after.work.catalog_rows_read);
        assert_eq!(
            c.select_metered("SELECT n FROM input", &p, limits(), reads(10))
                .work
                .rows_read,
            10,
            "later customer execution must still exclude catalog lookups"
        );
    }
}

#[test]
fn info_failure_budgets_and_rejected_statements_preserve_caller_transaction() {
    let c = setup(":memory:");
    let p = Parameters::new();
    c.execute("BEGIN", &p).unwrap();
    c.execute("INSERT INTO input VALUES(99)", &p).unwrap();
    let stopped = c.info_metered(
        "INFO FOR DB",
        &p,
        limits(),
        InfoWorkLimits {
            max_catalog_rows_read: None,
            max_vm_steps: Some(0),
        },
    );
    assert!(stopped.outcome.is_err());
    assert!(stopped.work.vm_budget_exhausted);
    assert_eq!(stopped.work.vm_steps, 0);
    assert_eq!(stopped.work.catalog_rows_read, 0);
    let small = c.info_metered(
        "INFO FOR DB",
        &p,
        ResultLimits {
            max_payload_bytes: 1,
            ..limits()
        },
        InfoWorkLimits::default(),
    );
    assert_eq!(small.outcome.unwrap_err().code(), "FDB_LIMIT");
    assert!(small.work.catalog_rows_read > 0);
    let missing = c.info_metered(
        "INFO FOR TABLE absent",
        &p,
        limits(),
        InfoWorkLimits::default(),
    );
    assert_eq!(missing.outcome.unwrap_err().code(), "FDB_NOT_FOUND");
    assert!(missing.work.catalog_rows_read > 0);
    for sql in [
        "DELETE FROM input",
        "SELECT n FROM input",
        "INFO FOR DB; DELETE FROM input",
    ] {
        let rejected = c.info_metered(sql, &p, limits(), InfoWorkLimits::default());
        assert!(rejected.outcome.is_err());
        assert_eq!(rejected.work.catalog_rows_read, 0);
        assert_eq!(rejected.work.vm_steps, 0);
    }
    let mut params = Parameters::new();
    params.insert("unused".into(), Value::Integer(1));
    assert!(c
        .info_metered("INFO FOR DB", &params, limits(), InfoWorkLimits::default())
        .outcome
        .is_err());
    assert_eq!(c.transaction_state(), TransactionState::Active);
    assert_eq!(c.execute("SELECT n FROM input", &p).unwrap().rows.len(), 6);
    c.execute("COMMIT", &p).unwrap();
    assert!(c
        .info_metered("INFO FOR DB", &p, limits(), InfoWorkLimits::default())
        .outcome
        .is_ok());
}
