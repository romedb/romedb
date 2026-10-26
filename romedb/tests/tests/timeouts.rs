use romedb::{CancellationToken, Database, Parameters, ResultLimits, TransactionState, Value};
use std::time::{Duration, Instant};

fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}
fn limits() -> ResultLimits {
    ResultLimits {
        max_rows: 100,
        max_payload_bytes: 100_000,
    }
}
const LONG: &str = "WITH RECURSIVE n(v) AS (VALUES(1) UNION ALL SELECT v+1 FROM n WHERE v<100000000) SELECT sum(v) FROM n";

#[test]
fn statement_deadlines_preserve_names_bindings_and_transaction_control() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs TIMEOUT 1s");
    q(
        &c,
        "DEFINE FIELD n ON docs TYPE integer DEFAULT 1 TIMEOUT 1s",
    );
    q(
        &c,
        "INSERT INTO docs {id:docs:a,timeout:7} RETURNING doc::after() TIMEOUT 1s",
    );
    assert_eq!(
        q(&c, "SELECT timeout FROM docs").rows,
        vec![vec![Value::Integer(7)]]
    );
    assert_eq!(
        q(&c, "SELECT timeout FROM docs TIMEOUT 1s").rows,
        vec![vec![Value::Integer(7)]]
    );
    let params = Parameters::from([("$timeout".into(), Value::Integer(19))]);
    assert_eq!(
        c.execute("SELECT $timeout,'TIMEOUT 0ms' TIMEOUT 1s;", &params)
            .unwrap()
            .rows,
        vec![vec![
            Value::Integer(19),
            Value::String("TIMEOUT 0ms".into())
        ]]
    );
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {id:docs:b,n:2}");
    for sql in [
        "UPDATE docs SET n=100 TIMEOUT 0ms",
        "UPDATE docs:a MERGE {n:100} TIMEOUT 0ms",
        "DELETE FROM docs TIMEOUT 0ms",
        "DROP TABLE docs TIMEOUT 0ms",
        "SELECT * FROM docs TIMEOUT 0ms",
    ] {
        assert_eq!(
            c.execute(sql, &Parameters::new()).unwrap_err().code(),
            "FDB_CANCELLED",
            "{sql}"
        );
        assert_eq!(c.transaction_state(), TransactionState::Active);
    }
    for sql in [
        "COMMIT TIMEOUT 1s",
        "ROLLBACK TIMEOUT 1s",
        "SELECT 1 TIMEOUT 86401s",
        "SELECT 1 TIMEOUT 1.5s",
        "SELECT 1 TIMEOUT 1h",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
        assert_eq!(c.transaction_state(), TransactionState::Active);
    }
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n TIMEOUT 1s").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT n FROM docs").rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn limited_profile_and_every_metered_route_observe_deadlines() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let p = Parameters::new();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {id:docs:a,n:1}");
    for sql in ["SELECT n FROM docs TIMEOUT 0ms", "SELECT 1 TIMEOUT 0ms"] {
        assert_eq!(
            c.profile_select(sql, &p).unwrap_err().code(),
            "FDB_CANCELLED"
        );
        assert_eq!(
            c.select_with_limits(sql, &p, limits()).unwrap_err().code(),
            "FDB_CANCELLED"
        );
        assert_eq!(
            c.profile_select_with_limits(sql, &p, limits())
                .unwrap_err()
                .code(),
            "FDB_CANCELLED"
        );
        let measured = c.select_metered(sql, &p, limits(), Default::default());
        assert_eq!(measured.outcome.unwrap_err().code(), "FDB_CANCELLED");
        assert_eq!(measured.work.rows_read, 0);
    }
    assert_eq!(
        c.select_metered(
            "SELECT * FROM docs:a TIMEOUT 0ms",
            &p,
            limits(),
            Default::default()
        )
        .outcome
        .unwrap_err()
        .code(),
        "FDB_CANCELLED"
    );
    assert_eq!(
        c.write_with_result_limits("UPDATE docs SET n=2 RETURNING * TIMEOUT 0ms", &p, limits())
            .unwrap_err()
            .code(),
        "FDB_CANCELLED"
    );
    let write = c.write_metered(
        "DELETE FROM docs TIMEOUT 0ms",
        &p,
        limits(),
        Default::default(),
    );
    assert_eq!(write.outcome.unwrap_err().code(), "FDB_CANCELLED");
    assert_eq!(write.work.row_mutations, 0);
    assert_eq!(
        c.create_metered(
            "CREATE TABLE unused TIMEOUT 0ms",
            &p,
            limits(),
            Default::default()
        )
        .unwrap()
        .outcome
        .unwrap_err()
        .code(),
        "FDB_CANCELLED"
    );
    assert_eq!(
        c.schema_metered(
            "DEFINE FIELD n ON docs TYPE string TIMEOUT 0ms",
            &p,
            limits(),
            Default::default()
        )
        .unwrap()
        .outcome
        .unwrap_err()
        .code(),
        "FDB_CANCELLED"
    );
    assert_eq!(
        c.ddl_metered(
            "DROP TABLE docs TIMEOUT 0ms",
            &p,
            limits(),
            Default::default()
        )
        .unwrap()
        .outcome
        .unwrap_err()
        .code(),
        "FDB_CANCELLED"
    );
    assert_eq!(
        c.explain_metered(
            "EXPLAIN SELECT n FROM docs TIMEOUT 0ms",
            &p,
            limits(),
            Default::default()
        )
        .unwrap()
        .outcome
        .unwrap_err()
        .code(),
        "FDB_CANCELLED"
    );
    assert_eq!(
        c.info_metered(
            "INFO FOR TABLE docs TIMEOUT 0ms",
            &p,
            limits(),
            Default::default()
        )
        .outcome
        .unwrap_err()
        .code(),
        "FDB_CANCELLED"
    );
    assert!(c
        .create_metered(
            "CREATE TABLE native(n INTEGER) TIMEOUT 1s",
            &p,
            limits(),
            Default::default()
        )
        .unwrap()
        .outcome
        .is_ok());
    assert!(c
        .schema_metered(
            "DEFINE FIELD n ON docs TYPE integer TIMEOUT 1s",
            &p,
            limits(),
            Default::default()
        )
        .unwrap()
        .outcome
        .is_ok());
    assert!(c
        .explain_metered(
            "EXPLAIN QUERY PLAN SELECT n FROM docs TIMEOUT 1s",
            &p,
            limits(),
            Default::default()
        )
        .unwrap()
        .outcome
        .is_ok());
    assert!(c
        .info_metered(
            "INFO FOR TABLE docs TIMEOUT 1s",
            &p,
            limits(),
            Default::default()
        )
        .outcome
        .is_ok());
    assert!(c
        .ddl_metered(
            "DROP TABLE native TIMEOUT 1s",
            &p,
            limits(),
            Default::default()
        )
        .unwrap()
        .outcome
        .is_ok());
    assert_eq!(
        q(&c, "SELECT n FROM docs").rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn timeout_and_caller_cancellation_compose_and_leave_no_expiry_on_reuse() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let p = Parameters::new();
    let generous = CancellationToken::with_deadline(Instant::now() + Duration::from_secs(60));
    let started = Instant::now();
    assert_eq!(
        c.execute_cancellable(&format!("{LONG} TIMEOUT 10ms"), &p, &generous)
            .unwrap_err()
            .code(),
        "FDB_CANCELLED"
    );
    eprintln!("SQL TIMEOUT 10ms observed {:?}", started.elapsed());
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!generous.is_cancelled());
    let caller = CancellationToken::new();
    let cancel = caller.clone();
    let request = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        cancel.cancel();
    });
    let started = Instant::now();
    assert_eq!(
        c.execute_cancellable(&format!("{LONG} TIMEOUT 5s"), &p, &caller)
            .unwrap_err()
            .code(),
        "FDB_CANCELLED"
    );
    request.join().unwrap();
    eprintln!(
        "caller cancellation under TIMEOUT 5s observed {:?}",
        started.elapsed()
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        q(&c, "SELECT 42 TIMEOUT 1s").rows,
        vec![vec![Value::Integer(42)]]
    );
    assert_eq!(q(&c, "SELECT 43").rows, vec![vec![Value::Integer(43)]]);
    let measured = c.select_metered(
        &format!("{LONG} TIMEOUT 10ms"),
        &p,
        limits(),
        Default::default(),
    );
    assert_eq!(measured.outcome.unwrap_err().code(), "FDB_CANCELLED");
    assert!(measured.work.vm_steps > 0);
}
