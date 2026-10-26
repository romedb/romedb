use romedb::{Database, Parameters, TransactionState};

#[test]
fn documented_future_statements_report_versions_without_changing_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in ["CREATE TABLE posts", "BEGIN", "INSERT INTO posts {n:1}"] {
        c.execute(sql, &Parameters::new()).unwrap();
    }
    for (sql, version) in [
        ("DEFINE CHANGEFEED changes ON posts RETAIN '72h'", "V3"),
        ("SHOW CHANGES FOR changes AFTER $cursor LIMIT 10", "V3"),
        ("REMOVE CHANGEFEED changes", "V3"),
        ("LET $x = 1", "V3"),
        ("DO { LET $x = 1; }", "V3"),
    ] {
        let report = c.execute_report(sql, &Parameters::new());
        let error = report.result.unwrap_err();
        assert_eq!(error.code(), "FDB_UNSUPPORTED", "{sql}");
        assert!(error.to_string().contains(version), "{error}");
        assert_eq!(report.transaction_after, TransactionState::Active);
    }
    for sql in [
        "SELECT 1 AS relation, 'CREATE SEARCH INDEX' AS function",
        "CREATE TABLE search (relation TEXT, changefeed TEXT)",
        "SELECT 'posts:p1 {' AS value",
    ] {
        c.execute(sql, &Parameters::new()).unwrap();
    }
    assert_eq!(
        c.execute("SELECT n FROM posts", &Parameters::new())
            .unwrap()
            .rows
            .len(),
        1
    );
    c.execute("ROLLBACK", &Parameters::new()).unwrap();
}
