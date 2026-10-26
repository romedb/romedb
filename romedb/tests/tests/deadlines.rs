use romedb::{CancellationToken, Database, Parameters, TransactionState, Value};
use std::time::{Duration, Instant};

#[test]
fn expired_deadline_rejects_before_parsing_and_preserves_transaction() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let p = Parameters::new();
    c.execute("CREATE TABLE docs", &p).unwrap();
    c.execute("BEGIN", &p).unwrap();
    c.execute("INSERT INTO docs {n:1}", &p).unwrap();
    let token = CancellationToken::with_deadline(Instant::now());
    assert!(token.clone().is_cancelled());
    assert_eq!(
        c.execute_cancellable("invalid SQL", &p, &token)
            .unwrap_err()
            .code(),
        "FDB_CANCELLED"
    );
    assert_eq!(c.transaction_state(), TransactionState::Active);
    assert_eq!(
        c.execute("SELECT n FROM docs", &p).unwrap().rows,
        vec![vec![Value::Integer(1)]]
    );
    let future = CancellationToken::with_deadline(Instant::now() + Duration::from_secs(60));
    let clone = future.clone();
    clone.cancel();
    assert!(future.is_cancelled());
    c.execute("ROLLBACK", &p).unwrap();
}

#[test]
fn deadline_interrupts_engine_work_and_allows_fresh_retry() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let p = Parameters::new();
    c.execute("CREATE TABLE input(n)", &p).unwrap();
    for n in 0..100 {
        c.execute(&format!("INSERT INTO input VALUES({n})"), &p)
            .unwrap();
    }
    c.execute("CREATE TABLE docs", &p).unwrap();
    c.execute("CREATE UNIQUE INDEX docs_n ON docs(n)", &p)
        .unwrap();
    c.execute("BEGIN", &p).unwrap();
    c.execute("INSERT INTO docs {n:1}", &p).unwrap();
    let select = "SELECT count(*) FROM input a,input b,input c,input d,input e";
    let write = "INSERT INTO docs(n) SELECT count(*) FROM input a,input b,input c,input d,input e";
    let limits = romedb::ResultLimits {
        max_rows: 1,
        max_payload_bytes: 1000,
    };
    for operation in 0..6 {
        let token = CancellationToken::with_deadline(Instant::now() + Duration::from_millis(20));
        let error = match operation {
            0 => c.execute_cancellable(select, &p, &token).unwrap_err(),
            1 => c.execute_cancellable(write, &p, &token).unwrap_err(),
            2 => c
                .profile_select_cancellable(select, &p, &token)
                .unwrap_err(),
            3 => c
                .select_with_limits_cancellable(select, &p, limits, &token)
                .unwrap_err(),
            4 => c
                .profile_select_with_limits_cancellable(select, &p, limits, &token)
                .unwrap_err(),
            5 => c
                .write_with_result_limits_cancellable(
                    &format!("{write} RETURNING n"),
                    &p,
                    limits,
                    &token,
                )
                .unwrap_err(),
            _ => unreachable!(),
        };
        assert_eq!(error.code(), "FDB_CANCELLED", "operation {operation}");
        assert!(token.is_cancelled());
        assert_eq!(c.transaction_state(), TransactionState::Active);
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .documents,
            1
        );
    }
    let fresh = CancellationToken::with_deadline(Instant::now() + Duration::from_secs(60));
    c.execute_cancellable("INSERT INTO docs {n:2}", &p, &fresh)
        .unwrap();
    assert_eq!(
        c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    assert_eq!(
        c.select_with_limits_cancellable("SELECT n FROM docs WHERE n=2", &p, limits, &fresh)
            .unwrap()
            .rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        c.profile_select_with_limits_cancellable(
            "SELECT n FROM docs WHERE n=2",
            &p,
            limits,
            &fresh
        )
        .unwrap()
        .result
        .rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        c.write_with_result_limits_cancellable(
            "UPDATE docs SET n=3 WHERE n=2 RETURNING n",
            &p,
            limits,
            &fresh
        )
        .unwrap()
        .rows,
        vec![vec![Value::Integer(3)]]
    );
    c.execute("ROLLBACK", &p).unwrap();
    assert!(c.execute("SELECT * FROM docs", &p).unwrap().rows.is_empty());
}

#[test]
fn deadline_batch_and_migration_boundaries_preserve_state_and_history() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let p = Parameters::new();
    c.execute("CREATE TABLE input(n)", &p).unwrap();
    for n in 0..100 {
        c.execute(&format!("INSERT INTO input VALUES({n})"), &p)
            .unwrap();
    }
    let plan = vec![
        romedb::Migration {version:1,name:"initial".into(),sql:"CREATE TABLE docs; CREATE UNIQUE INDEX docs_n ON docs(n); INSERT INTO docs {n:1};".into()},
        romedb::Migration {version:2,name:"pending".into(),sql:"CREATE TABLE audit(n); INSERT INTO docs {n:2}; SELECT count(*) FROM input a,input b,input c,input d,input e;".into()},
    ];
    c.migrate(&plan[..1]).unwrap();
    c.execute("BEGIN", &p).unwrap();
    c.execute("INSERT INTO docs {n:3}", &p).unwrap();
    let token = CancellationToken::with_deadline(Instant::now() + Duration::from_millis(20));
    let reports = c
        .execute_batch_cancellable(
            "SELECT count(*) FROM input a,input b,input c,input d,input e; COMMIT;",
            &token,
        )
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0].execution.result.as_ref().unwrap_err().code(),
        "FDB_CANCELLED"
    );
    assert_eq!(c.transaction_state(), TransactionState::Active);
    assert_eq!(
        c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(3)]]
    );
    c.execute("ROLLBACK", &p).unwrap();
    let token = CancellationToken::with_deadline(Instant::now() + Duration::from_millis(20));
    assert_eq!(
        c.migrate_cancellable(&plan, &token).unwrap_err().code(),
        "FDB_CANCELLED"
    );
    assert_eq!(c.transaction_state(), TransactionState::Autocommit);
    assert!(c.execute("SELECT * FROM audit", &p).is_err());
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .documents,
        1
    );
    assert_eq!(c.migrate(&plan[..1]).unwrap().already_applied, 1);
    // Empty the external source so the identical pending script completes on retry.
    c.execute("DELETE FROM input", &p).unwrap();
    let fresh = CancellationToken::with_deadline(Instant::now() + Duration::from_secs(60));
    assert_eq!(
        c.migrate_cancellable(&plan, &fresh).unwrap().applied,
        vec![2]
    );
    assert_eq!(
        c.migrate_cancellable(&plan, &fresh)
            .unwrap()
            .already_applied,
        2
    );
    assert_eq!(
        c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .documents,
        2
    );
}
