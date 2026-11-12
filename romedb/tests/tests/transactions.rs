use romedb::{Database, Parameters, TransactionState as State, Value};
fn q(c: &romedb::Connection, sql: &str) {
    c.execute(sql, &Parameters::new()).expect(sql);
}

#[test]
fn managed_atomic_requests_preserve_typed_values_and_journal_mode() {
    for mvcc in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("atomic.romedb");
        let database = Database::open(path.to_str().unwrap()).unwrap();
        let connection = database.connect().unwrap();
        if mvcc {
            q(&connection, "PRAGMA journal_mode=mvcc");
        }
        let mode = connection
            .execute("PRAGMA journal_mode", &Parameters::new())
            .unwrap()
            .rows;
        let reply = connection.execute_atomic_request(r#"{
            "readOnly":false,"statements":[
                {"sql":"CREATE TABLE data"},
                {"sql":"INSERT INTO data DOCUMENT $doc","params":{"$doc":{"type":"Object","value":{
                    "id":{"type":"Record","value":{"table":"data","key":{"type":"String","value":"a"}}},
                    "n":{"type":"Integer","value":"9223372036854775807"},
                    "f":{"type":"Number","value":"8000000000000000"},
                    "bytes":{"type":"Binary","value":[0,255]}
                }}}},
                {"sql":"SELECT n AS same,n AS same,f,bytes FROM data"}
            ]}"#, &romedb::CancellationToken::new()).unwrap();
        assert!(
            reply.contains(r#""columns":["same","same","f","bytes"]"#),
            "{reply}"
        );
        assert!(reply.contains(r#""value":"9223372036854775807""#));
        assert!(reply.contains(r#""value":"8000000000000000""#));
        assert!(reply.contains(r#""value":[0,255]"#));
        assert_eq!(connection.transaction_state(), State::Autocommit);
        assert_eq!(
            connection
                .execute("PRAGMA journal_mode", &Parameters::new())
                .unwrap()
                .rows,
            mode
        );
        drop(connection);
        drop(database);
        let database = Database::open(path.to_str().unwrap()).unwrap();
        assert_eq!(
            database
                .connect()
                .unwrap()
                .execute("SELECT n FROM data", &Parameters::new())
                .unwrap()
                .rows,
            vec![vec![Value::Integer(i64::MAX)]]
        );
    }
}

#[test]
fn managed_atomic_request_failure_restores_prior_rows_and_indexes() {
    for mvcc in [false, true] {
        let database = Database::open(":memory:").unwrap();
        let connection = database.connect().unwrap();
        if mvcc {
            q(&connection, "PRAGMA journal_mode=mvcc");
        }
        q(&connection, "CREATE TABLE items");
        q(&connection, "DEFINE FIELD n ON items TYPE integer REQUIRED");
        q(&connection, "CREATE UNIQUE INDEX by_n ON items(n)");
        q(&connection, "INSERT INTO items {id:items:a,n:1}");
        let error = connection
            .execute_atomic_request(
                r#"{"readOnly":false,"statements":[
            {"sql":"INSERT INTO items {id:items:b,n:2}"},
            {"sql":"UPDATE items:a SET n=2"}
        ]}"#,
                &romedb::CancellationToken::new(),
            )
            .unwrap_err();
        assert_eq!(error.code(), "RDB_CONSTRAINT");
        assert_eq!(connection.transaction_state(), State::Autocommit);
        assert_eq!(
            connection
                .execute("SELECT n FROM items", &Parameters::new())
                .unwrap()
                .rows,
            vec![vec![Value::Integer(1)]]
        );
        connection
            .execute_atomic_request(
                r#"{"readOnly":false,"statements":[{"sql":"INSERT INTO items {id:items:b,n:2}"}]}"#,
                &romedb::CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(
            connection
                .execute("SELECT n FROM items WHERE n=2", &Parameters::new())
                .unwrap()
                .rows,
            vec![vec![Value::Integer(2)]]
        );
        connection
            .check_collection_integrity("items", romedb::IntegrityLimits::default())
            .unwrap();
    }
}

#[test]
fn managed_atomic_requests_reject_user_transaction_control_before_mutation() {
    let database = Database::open(":memory:").unwrap();
    let connection = database.connect().unwrap();
    q(&connection, "CREATE TABLE data (n INTEGER)");
    for sql in [
        "BEGIN",
        "COMMIT",
        "ROLLBACK",
        "SAVEPOINT caller",
        "PRAGMA journal_mode=mvcc",
        "SELECT 1; SELECT 2",
    ] {
        let body = format!(
            r#"{{"readOnly":false,"statements":[{{"sql":"INSERT INTO data VALUES(1)"}},{{"sql":"{sql}"}}]}}"#
        );
        assert!(
            connection
                .execute_atomic_request(&body, &romedb::CancellationToken::new())
                .is_err(),
            "{sql}"
        );
        assert_eq!(
            connection
                .execute("SELECT n FROM data", &Parameters::new())
                .unwrap()
                .rows
                .len(),
            0
        );
        assert_eq!(connection.transaction_state(), State::Autocommit);
    }
    assert!(connection
        .execute_atomic_request(
            r#"{"readOnly":true,"statements":[{"sql":"SELECT 'a;b' AS text"}]}"#,
            &romedb::CancellationToken::new()
        )
        .unwrap()
        .contains("a;b"));
}

#[test]
fn managed_atomic_request_result_and_work_limits_roll_back_writes() {
    let database = Database::open(":memory:").unwrap();
    let connection = database.connect().unwrap();
    q(&connection, "CREATE TABLE data (n INTEGER)");
    for body in [
        r#"{"readOnly":false,"workLimits":{"rowMutations":1},"statements":[{"sql":"INSERT INTO data VALUES(1)"},{"sql":"INSERT INTO data VALUES(2)"}]}"#,
        r#"{"readOnly":false,"statements":[{"sql":"INSERT INTO data VALUES(1)"},{"sql":"SELECT hex(zeroblob(2200000))"},{"sql":"SELECT hex(zeroblob(2200000))"}]}"#,
        r#"{"readOnly":false,"statements":[{"sql":"INSERT INTO data VALUES(1)"},{"sql":"WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM t WHERE n<10001) SELECT n FROM t"}]}"#,
    ] {
        let error = connection
            .execute_atomic_request(body, &romedb::CancellationToken::new())
            .unwrap_err();
        assert_eq!(error.code(), "RDB_LIMIT", "{body}: {error}");
        assert_eq!(connection.transaction_state(), State::Autocommit);
        assert!(connection
            .execute("SELECT n FROM data", &Parameters::new())
            .unwrap()
            .rows
            .is_empty());
    }
}

#[test]
fn managed_atomic_requests_preserve_caller_work_and_recover_after_cancellation() {
    let database = Database::open(":memory:").unwrap();
    let connection = database.connect().unwrap();
    q(&connection, "CREATE TABLE data (n INTEGER)");
    let body = r#"{"readOnly":false,"statements":[{"sql":"INSERT INTO data VALUES(1)"}]}"#;
    q(&connection, "BEGIN");
    q(&connection, "INSERT INTO data VALUES(7)");
    assert_eq!(
        connection
            .execute_atomic_request(body, &romedb::CancellationToken::new())
            .unwrap_err()
            .code(),
        "RDB_UNSUPPORTED"
    );
    assert_eq!(connection.transaction_state(), State::Active);
    q(&connection, "COMMIT");
    let token = romedb::CancellationToken::new();
    token.cancel();
    assert_eq!(
        connection
            .execute_atomic_request(body, &token)
            .unwrap_err()
            .code(),
        "RDB_CANCELLED"
    );
    assert_eq!(connection.transaction_state(), State::Autocommit);
    connection
        .execute_atomic_request(body, &romedb::CancellationToken::new())
        .unwrap();
    assert_eq!(
        connection
            .execute("SELECT n FROM data ORDER BY n", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(7)]]
    );
}

#[test]
fn managed_atomic_fresh_reads_reject_mutations_and_invalid_envelopes() {
    let database = Database::open(":memory:").unwrap();
    let connection = database.connect().unwrap();
    for body in [
        r#"{"readOnly":true,"statements":[{"sql":"CREATE TABLE hidden"}]}"#,
        r#"{"readOnly":false,"statements":[]}"#,
        r#"{"readOnly":true,"statements":[{"sql":"SELECT 1"}],"unexpected":true}"#,
        r#"{"readOnly":true,"statements":[{"sql":"SELECT $n","params":{"$n":{"type":"Integer","value":"9223372036854775808"}}}]}"#,
    ] {
        assert!(connection
            .execute_atomic_request(body, &romedb::CancellationToken::new())
            .is_err());
        assert_eq!(connection.transaction_state(), State::Autocommit);
    }
    assert!(connection
        .execute("SELECT * FROM hidden", &Parameters::new())
        .is_err());
    let body = format!(
        r#"{{"readOnly":true,"statements":[{{"sql":"SELECT '{}'"}}]}}"#,
        "x".repeat(65536)
    );
    assert_eq!(
        connection
            .execute_atomic_request(&body, &romedb::CancellationToken::new())
            .unwrap_err()
            .code(),
        "RDB_VALIDATION"
    );
    let body = format!(
        r#"{{"readOnly":true,"statements":[{{"sql":"SELECT $v","params":{{"$v":{{"type":"String","value":"{}"}}}}}}]}}"#,
        "x".repeat(romedb::MAX_REQUEST_BYTES)
    );
    assert_eq!(
        connection
            .execute_atomic_request(&body, &romedb::CancellationToken::new())
            .unwrap_err()
            .code(),
        "RDB_LIMIT"
    );
    let body = format!(
        r#"{{"readOnly":true,"statements":[{{"sql":"SELECT length($v) AS n","params":{{"$v":{{"type":"String","value":"{}"}}}}}}]}}"#,
        "x".repeat(romedb::MAX_REQUEST_BYTES - 200)
    );
    let reply = connection
        .execute_atomic_request(&body, &romedb::CancellationToken::new())
        .unwrap();
    assert!(reply.contains(&format!(
        r#""rows":[[{{"type":"Integer","value":"{}"}}]]"#,
        romedb::MAX_REQUEST_BYTES - 200
    )));
}
#[test]
fn validation_errors_preserve_outer_work_and_report_active_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE posts");
        q(&c, "DEFINE FIELD n ON posts TYPE integer REQUIRED");
        assert_eq!(c.transaction_state(), State::Autocommit);
        let begin = c.execute_report("BEGIN", &Parameters::new());
        begin.result.unwrap();
        assert_eq!(
            (begin.transaction_before, begin.transaction_after),
            (State::Autocommit, State::Active)
        );
        q(&c, "INSERT INTO posts {n:1}");
        let error = c.execute_report("INSERT INTO posts {n:'bad'}", &Parameters::new());
        assert_eq!(error.result.unwrap_err().code(), "RDB_VALIDATION");
        assert_eq!(
            (error.transaction_before, error.transaction_after),
            (State::Active, State::Active)
        );
        let commit = c.execute_report("COMMIT", &Parameters::new());
        commit.result.unwrap();
        assert_eq!(commit.transaction_after, State::Autocommit);
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        c.execute("SELECT n FROM posts", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::Integer(1)]]
    );
}
#[test]
fn returning_scalar_error_preserves_outer_work_and_connection_recovers() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE posts");
    q(&c, "INSERT INTO posts {n:1}");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO posts {n:2}");
    let report = c.execute_report(
        "UPDATE posts SET n=n+10 RETURNING array::append(1,2) AS bad",
        &Parameters::new(),
    );
    assert!(report.result.is_err());
    assert_eq!(
        (report.transaction_before, report.transaction_after),
        (State::Active, State::Active)
    );
    assert_eq!(
        c.execute("SELECT n FROM posts", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    q(&c, "ROLLBACK");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO posts {n:3}");
    q(&c, "COMMIT");
    assert_eq!(c.transaction_state(), State::Autocommit);
}
#[test]
fn savepoints_and_independent_connections_have_independent_states() {
    let db = Database::open(":memory:").unwrap();
    let a = db.connect().unwrap();
    let b = db.connect().unwrap();
    q(&a, "SAVEPOINT outer_work");
    assert_eq!(a.transaction_state(), State::Active);
    assert_eq!(b.transaction_state(), State::Autocommit);
    let error = a.execute_report("SELECT (", &Parameters::new());
    assert!(error.result.is_err());
    assert_eq!(error.transaction_after, State::Active);
    q(&a, "SAVEPOINT inner_work");
    q(&a, "ROLLBACK TO inner_work");
    q(&a, "RELEASE inner_work");
    assert_eq!(a.transaction_state(), State::Active);
    q(&a, "RELEASE outer_work");
    assert_eq!(a.transaction_state(), State::Autocommit);
}

#[test]
fn native_constraint_categories_preserve_reports_and_statement_rollback() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(
        &c,
        "CREATE TABLE items(id INTEGER PRIMARY KEY,name TEXT UNIQUE NOT NULL,n INTEGER CHECK(n>0))",
    );
    q(&c, "INSERT INTO items VALUES (1,'one',1)");
    for sql in [
        "INSERT INTO items VALUES (1,'two',2)",
        "INSERT INTO items VALUES (2,'one',2)",
        "INSERT INTO items VALUES (2,NULL,2)",
        "INSERT INTO items VALUES (2,'two',-1)",
    ] {
        let report = c.execute_report(sql, &Parameters::new());
        assert_eq!(report.result.unwrap_err().code(), "RDB_CONSTRAINT", "{sql}");
        assert_eq!(report.transaction_after, State::Autocommit);
    }
    q(&c, "BEGIN");
    q(&c, "INSERT INTO items VALUES (2,'two',2)");
    let report = c.execute_report(
        "INSERT INTO items VALUES (3,'three',-1)",
        &Parameters::new(),
    );
    assert_eq!(report.result.unwrap_err().code(), "RDB_CONSTRAINT");
    assert_eq!(report.transaction_after, State::Active);
    assert_eq!(
        c.execute("SELECT count(*) FROM items", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::Integer(2)]]
    );
    q(&c, "ROLLBACK");
    q(&c, "PRAGMA foreign_keys=ON");
    q(
        &c,
        "CREATE TABLE child(parent INTEGER REFERENCES items(id))",
    );
    assert_eq!(
        c.execute("INSERT INTO child VALUES (99)", &Parameters::new())
            .unwrap_err()
            .code(),
        "RDB_CONSTRAINT"
    );
    q(&c,"CREATE TRIGGER reject_item BEFORE INSERT ON items WHEN new.id=9 BEGIN SELECT RAISE(ABORT,'rejected item'); END");
    assert_eq!(
        c.execute("INSERT INTO items VALUES (9,'nine',9)", &Parameters::new())
            .unwrap_err()
            .code(),
        "RDB_CONSTRAINT"
    );
    assert_eq!(
        c.execute("SELECT missing_function()", &Parameters::new())
            .unwrap_err()
            .code(),
        "RDB_ENGINE"
    );
}

#[test]
fn constraint_code_does_not_imply_full_rollback_for_native_fail_policy() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE items(n INTEGER UNIQUE)");
    let report = c.execute_report(
        "INSERT OR FAIL INTO items VALUES (1),(2),(1),(3)",
        &Parameters::new(),
    );
    assert_eq!(report.result.unwrap_err().code(), "RDB_CONSTRAINT");
    assert_eq!(report.transaction_after, State::Autocommit);
    assert_eq!(
        c.execute("SELECT n FROM items ORDER BY n", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}

#[test]
fn excessive_sql_delimiters_fail_before_writes_and_preserve_outer_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE native(value INTEGER)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {value:1}");
    let deep = format!("{}2{}", "(".repeat(128), ")".repeat(128));
    for statement in [
        format!("INSERT INTO native VALUES ({deep})"),
        format!("INSERT INTO docs (value) VALUES (2) RETURNING {deep}"),
        format!("UPDATE docs SET value={deep}"),
        format!("SELECT {deep} FROM docs"),
    ] {
        let report = c.execute_report(&statement, &Parameters::new());
        assert_eq!(report.result.unwrap_err().code(), "RDB_SYNTAX");
        assert_eq!(report.transaction_after, State::Active);
        assert_eq!(
            c.execute("SELECT value FROM docs", &Parameters::new())
                .unwrap()
                .rows,
            vec![vec![Value::Integer(1)]]
        );
        assert!(c
            .execute("SELECT * FROM native", &Parameters::new())
            .unwrap()
            .rows
            .is_empty());
    }
    q(&c, "INSERT INTO native VALUES (3)");
    q(&c, "ROLLBACK");
    assert!(c
        .execute("SELECT * FROM docs", &Parameters::new())
        .unwrap()
        .rows
        .is_empty());
    assert!(c
        .execute("SELECT * FROM native", &Parameters::new())
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn oversized_lexer_inputs_fail_before_mutation_and_preserve_active_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE INDEX docs_value ON docs(value)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {value:1}");
    for sql in [
        format!(
            "DELETE FROM docs; /*{}*/",
            "x".repeat(romeql_parser::MAX_INPUT_BYTES)
        ),
        format!(
            "DELETE FROM docs RETURNING {}",
            "1,".repeat(romeql_parser::MAX_TOKENS / 2)
        ),
    ] {
        let report = c.execute_report(&sql, &Parameters::new());
        assert_eq!(report.result.unwrap_err().code(), "RDB_SYNTAX");
        assert_eq!(report.transaction_after, State::Active);
        assert_eq!(c.execute_batch(&sql).unwrap_err().code(), "RDB_SYNTAX");
        assert_eq!(c.transaction_state(), State::Active);
        assert_eq!(
            c.lookup_index("docs", "docs_value", &Value::Integer(1))
                .unwrap()
                .len(),
            1
        );
    }
    q(&c, "ROLLBACK");
    assert!(c
        .execute("SELECT * FROM docs", &Parameters::new())
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn recursive_sql_returns_depth_errors_on_a_small_caller_stack() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            q(&c, "CREATE TABLE docs");
            q(&c, "BEGIN");
            q(&c, "INSERT INTO docs {v:1}");
            for expr in [
                format!("{}1", "NOT ".repeat(2000)),
                format!(
                    "{}1{}",
                    "CASE WHEN 1 THEN ".repeat(200),
                    " ELSE 0 END".repeat(200)
                ),
            ] {
                for suffix in ["", " FROM docs"] {
                    let report =
                        c.execute_report(&format!("SELECT {expr}{suffix}"), &Parameters::new());
                    let error = report.result.unwrap_err();
                    assert_eq!(error.code(), "RDB_ENGINE");
                    assert!(error.to_string().contains("maximum depth 100"));
                    assert!(c
                        .profile_select(&format!("SELECT {expr}{suffix}"), &Parameters::new())
                        .unwrap_err()
                        .to_string()
                        .contains("maximum depth 100"));
                    assert_eq!(report.transaction_after, State::Active);
                }
            }
            assert_eq!(
                c.execute("SELECT v FROM docs", &Parameters::new())
                    .unwrap()
                    .rows,
                vec![vec![Value::Integer(1)]]
            );
            q(&c, "ROLLBACK");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn atomic_requests_on_mvcc_files_survive_automatic_checkpoints() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("site.romedb");
    std::fs::File::create(&path).unwrap();
    let database = Database::create_mvcc(path.to_str().unwrap()).unwrap();
    let connection = database.connect().unwrap();
    q(&connection, "PRAGMA mvcc_checkpoint_threshold=0");
    for (index, sql) in [
        "CREATE TABLE posts",
        "INSERT INTO posts {title:'one'}",
        "INSERT INTO posts {title:'two'}",
    ]
    .into_iter()
    .enumerate()
    {
        let reply = connection
            .execute_atomic_request(
                &format!(r#"{{"readOnly":false,"statements":[{{"sql":"{sql}"}}]}}"#),
                &romedb::CancellationToken::new(),
            )
            .unwrap_or_else(|error| panic!("request {index}: {error}"));
        assert!(reply.contains(r#""results""#), "{reply}");
        assert_eq!(connection.transaction_state(), State::Autocommit);
    }
    assert_eq!(
        connection
            .execute("SELECT count() FROM posts", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn mvcc_opens_refuse_wal_files_before_writing_and_creation_needs_an_empty_file() {
    let directory = tempfile::tempdir().unwrap();
    let wal = directory.path().join("legacy.romedb");
    {
        let database = Database::open(wal.to_str().unwrap()).unwrap();
        q(&database.connect().unwrap(), "CREATE TABLE notes");
    }
    let before: Vec<_> = ["legacy.romedb", "legacy.romedb-wal"]
        .iter()
        .map(|name| std::fs::read(directory.path().join(name)).unwrap_or_default())
        .collect();
    let error = Database::open_mvcc(wal.to_str().unwrap()).err().unwrap();
    assert_eq!(error.code(), "RDB_UNSUPPORTED");
    assert!(error.to_string().contains("migrate-to-mvcc"), "{error}");
    let after: Vec<_> = ["legacy.romedb", "legacy.romedb-wal"]
        .iter()
        .map(|name| std::fs::read(directory.path().join(name)).unwrap_or_default())
        .collect();
    assert_eq!(before, after);
    assert_eq!(
        Database::create_mvcc(wal.to_str().unwrap())
            .err()
            .unwrap()
            .code(),
        "RDB_VALIDATION"
    );
    let fresh = directory.path().join("fresh.romedb");
    std::fs::File::create(&fresh).unwrap();
    drop(Database::create_mvcc(fresh.to_str().unwrap()).unwrap());
    let reopened = Database::open_mvcc(fresh.to_str().unwrap()).unwrap();
    assert_eq!(
        reopened
            .connect()
            .unwrap()
            .execute("PRAGMA journal_mode", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::String("mvcc".into())]]
    );
}

#[cfg(unix)]
#[test]
fn log_files_get_the_database_file_permissions() {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let mode = |name: &str| {
        std::fs::metadata(directory.path().join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    let path = directory.path().join("site.romedb");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    let database = Database::create_mvcc(path.to_str().unwrap()).unwrap();
    q(&database.connect().unwrap(), "CREATE TABLE notes");
    q(
        &database.connect().unwrap(),
        "INSERT INTO notes { text: 'private' }",
    );
    drop(database);
    for name in ["site.romedb", "site.romedb-wal", "site.db-log"] {
        assert_eq!(mode(name), 0o600, "{name}");
    }
    let log = directory.path().join("site.db-log");
    std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::remove_file(directory.path().join("site.romedb-wal")).unwrap();
    drop(Database::open_mvcc(path.to_str().unwrap()).unwrap());
    assert_eq!(mode("site.db-log"), 0o600);
    assert_eq!(mode("site.romedb-wal"), 0o600);

    let wal = directory.path().join("legacy.romedb");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o640)
        .open(&wal)
        .unwrap();
    drop(Database::open(wal.to_str().unwrap()).unwrap());
    assert_eq!(mode("legacy.romedb-wal"), 0o640);
    assert!(Database::open_mvcc(wal.to_str().unwrap()).is_err());
    assert!(!directory.path().join("legacy.db-log").exists());
}
