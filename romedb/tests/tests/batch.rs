use romedb::{Database, Parameters, TransactionState, Value};
#[test]
fn multiline_scripts_preserve_literals_comments_offsets_and_final_statement() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let script="-- Unicode ไทย\nCREATE TABLE posts;\n/* ; */ INSERT INTO posts {\n id:posts:p1,\n text:'semi;colon', tags:[';', [1,2]]\n};\nSELECT text FROM posts";
    let reports = c.execute_batch(script).unwrap();
    assert_eq!(reports.len(), 3);
    assert_eq!(reports[0].offset, script.find("CREATE").unwrap());
    assert!(reports.iter().all(|r| r.execution.result.is_ok()));
    assert_eq!(
        reports[2].execution.result.as_ref().unwrap().rows,
        vec![vec![Value::String("semi;colon".into())]]
    );
    assert!(c
        .execute_batch("; -- nothing\n /* ; */ ;")
        .unwrap()
        .is_empty());
}
#[test]
fn batches_stop_on_error_and_leave_explicit_transaction_control_to_caller() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    c.execute("CREATE TABLE posts", &Parameters::new()).unwrap();
    let reports = c
        .execute_batch(
            "BEGIN; INSERT INTO posts {id:posts:p1}; INSERT INTO posts {id:posts:p1}; COMMIT;",
        )
        .unwrap();
    assert_eq!(reports.len(), 3);
    assert!(reports[2].execution.result.is_err());
    assert_eq!(
        reports[2].execution.transaction_after,
        TransactionState::Active
    );
    c.execute("ROLLBACK", &Parameters::new()).unwrap();
    assert!(c
        .execute("SELECT * FROM posts", &Parameters::new())
        .unwrap()
        .rows
        .is_empty());
    assert!(c
        .execute_batch("INSERT INTO posts {id:posts:p2}; SELECT 'unterminated")
        .is_err());
    assert!(c
        .execute("SELECT * FROM posts", &Parameters::new())
        .unwrap()
        .rows
        .is_empty());
}
#[test]
fn sql_trigger_bodies_and_case_end_are_single_statements() {
    let script="CREATE TABLE source(n); CREATE TABLE events(n); CREATE TRIGGER log_insert AFTER INSERT ON source BEGIN INSERT INTO events VALUES(CASE WHEN NEW.n=1 THEN 10 ELSE 20 END); INSERT INTO events VALUES(30); END; INSERT INTO source VALUES(1); SELECT n FROM events ORDER BY n;";
    let pieces = romeql_parser::split_script(script).unwrap();
    assert_eq!(pieces.len(), 5);
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let reports = c.execute_batch(script).unwrap();
    for report in &reports {
        assert!(
            report.execution.result.is_ok(),
            "{:?}",
            report.execution.result
        );
    }
    assert_eq!(reports.len(), 5);
    assert_eq!(
        reports[4].execution.result.as_ref().unwrap().rows,
        vec![vec![Value::Integer(10)], vec![Value::Integer(30)]]
    );
}

#[test]
fn batch_visitors_can_stop_without_executing_later_statements() {
    let db = romedb::Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let mut visited = 0;
    c.visit_batch(
        "CREATE TABLE samples(value INTEGER); INSERT INTO samples VALUES (1);",
        |entry| {
            entry.execution.result?;
            visited += 1;
            Ok(false)
        },
    )
    .unwrap();
    assert_eq!(visited, 1);
    assert!(c
        .execute("SELECT * FROM samples", &romedb::Parameters::new())
        .unwrap()
        .rows
        .is_empty());
    let result = c.visit_batch(
        "INSERT INTO samples VALUES (1); INSERT INTO samples VALUES (2);",
        |entry| {
            entry.execution.result?;
            Err(romedb::Error::Validation("consumer stopped".into()))
        },
    );
    assert!(result.is_err());
    assert_eq!(
        c.execute("SELECT value FROM samples", &romedb::Parameters::new())
            .unwrap()
            .rows,
        vec![vec![romedb::Value::Integer(1)]]
    );
    assert!(c
        .visit_batch(
            "INSERT INTO samples VALUES (3); SELECT 'unfinished",
            |_| panic!("lexical failure must precede visitation")
        )
        .is_err());
}

#[test]
fn cancellable_batches_stop_at_statement_boundaries_and_preserve_prior_work() {
    for outer in [false, true] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE docs", &Parameters::new()).unwrap();
        c.execute("CREATE UNIQUE INDEX docs_n ON docs(n)", &Parameters::new())
            .unwrap();
        if outer {
            c.execute("BEGIN", &Parameters::new()).unwrap();
        }
        let state = c.transaction_state();
        let token = romedb::CancellationToken::new();
        let script = "-- ไทย\nINSERT INTO docs {id:docs:first,n:1}; INSERT INTO docs {id:docs:second,n:2}; DELETE FROM docs;";
        let mut entries = Vec::new();
        c.visit_batch_cancellable(script, &token, |entry| {
            entries.push(entry);
            token.cancel();
            Ok(true)
        })
        .unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].execution.result.is_ok());
        assert_eq!(
            entries[1].offset,
            script.find("INSERT INTO docs {id:docs:second").unwrap()
        );
        assert_eq!(
            entries[1].execution.result.as_ref().unwrap_err().code(),
            "FDB_CANCELLED"
        );
        assert_eq!(entries[1].execution.transaction_before, state);
        assert_eq!(entries[1].execution.transaction_after, state);
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .documents,
            1
        );
        // Pre-cancellation precedes even malformed-script parsing and visitation.
        assert_eq!(
            c.execute_batch_cancellable("SELECT '", &token)
                .unwrap_err()
                .code(),
            "FDB_CANCELLED"
        );
        let retry = c
            .execute_batch_cancellable(
                "INSERT INTO docs {id:docs:second,n:2}; SELECT n FROM docs ORDER BY n",
                &romedb::CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(retry.len(), 2);
        assert_eq!(
            retry[1].execution.result.as_ref().unwrap().rows,
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        );
        if outer {
            c.execute("ROLLBACK", &Parameters::new()).unwrap();
            assert_eq!(
                c.check_collection_integrity("docs", Default::default())
                    .unwrap()
                    .documents,
                0
            );
        }
    }
}
