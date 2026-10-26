use romedb::{Database, Migration, Parameters, TransactionState};
fn m(version: i64, sql: &str) -> Migration {
    Migration {
        version,
        name: format!("{version}_test.sql"),
        sql: sql.into(),
    }
}
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
#[test]
fn migrations_apply_once_and_reject_changed_or_missing_history() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let plan = vec![
        m(1, "CREATE TABLE docs; CREATE TABLE audit(value INTEGER);"),
        m(
            2,
            "INSERT INTO docs {id:docs:p1,value:4}; INSERT INTO audit VALUES (1);",
        ),
    ];
    assert_eq!(c.migrate(&plan).unwrap().applied, vec![1, 2]);
    let report = c.migrate(&plan).unwrap();
    assert_eq!(report.already_applied, 2);
    assert!(report.applied.is_empty());
    assert_eq!(q(&c, "SELECT * FROM audit").rows.len(), 1);
    let mut changed = plan.clone();
    changed[0].sql.push(' ');
    let error = c.migrate(&changed).unwrap_err();
    assert_eq!(error.code(), "FDB_VALIDATION");
    assert!(error.to_string().contains("SQL source differs"));
    changed = plan.clone();
    changed[0].name.push_str("_renamed");
    assert!(c
        .migrate(&changed)
        .unwrap_err()
        .to_string()
        .contains("name differs"));
    changed = plan.clone();
    changed[1].version = 3;
    assert!(c
        .migrate(&changed)
        .unwrap_err()
        .to_string()
        .contains("version does not match the applied sequence"));
    assert!(c.migrate(&plan[..1]).is_err());
    assert_eq!(c.migrate(&plan).unwrap().already_applied, 2);
    assert_eq!(c.transaction_state(), TransactionState::Autocommit);
    assert!(c
        .execute("SELECT * FROM __romedb_migrations", &Parameters::new())
        .is_err());
}
#[test]
fn failed_pending_run_rolls_back_ddl_data_and_history() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let initial = m(
        1,
        "CREATE TABLE docs; DEFINE FIELD value ON docs TYPE integer;",
    );
    c.migrate(std::slice::from_ref(&initial)).unwrap();
    let mut plan = vec![
        initial,
        m(
            2,
            "CREATE TABLE audit(value INTEGER); INSERT INTO docs {id:docs:p1,value:1};",
        ),
        m(3, "INSERT INTO docs {id:docs:p2,value:'bad'};"),
    ];
    let error = c.migrate(&plan).unwrap_err();
    assert_eq!(error.code(), "FDB_MIGRATION");
    assert!(q(&c, "SELECT * FROM docs").rows.is_empty());
    assert!(c
        .execute("SELECT * FROM audit", &Parameters::new())
        .is_err());
    plan[2] = m(3, "INSERT INTO docs {id:docs:p2,value:2};");
    assert_eq!(c.migrate(&plan).unwrap().applied, vec![2, 3]);
    assert_eq!(q(&c, "SELECT * FROM docs").rows.len(), 2);
    plan.push(m(4,"CREATE TABLE runtime_work(value INTEGER); INSERT INTO docs {id:docs:p3,value:3}; SELECT array::append(1,2);"));
    assert_eq!(c.migrate(&plan).unwrap_err().code(), "FDB_MIGRATION");
    assert_eq!(c.transaction_state(), TransactionState::Autocommit);
    assert_eq!(q(&c, "SELECT * FROM docs").rows.len(), 2);
    assert!(c
        .execute("SELECT * FROM runtime_work", &Parameters::new())
        .is_err());
    plan[3] = m(4, "CREATE TABLE runtime_work(value INTEGER);");
    assert_eq!(c.migrate(&plan).unwrap().applied, vec![4]);
}
#[test]
fn migration_transaction_escape_and_invalid_order_fail_before_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for command in [
        "COMMIT",
        "END",
        "ROLLBACK",
        "BEGIN",
        "SAVEPOINT x",
        "RELEASE x",
        "PRAGMA user_version=1",
        "ATTACH ':memory:' AS other",
        "VACUUM",
        "CREATE TEMP TABLE transient(x)",
    ] {
        let plan = [m(1, &format!("CREATE TABLE docs; {command};"))];
        assert!(c.migrate(&plan).is_err(), "{command}");
        // No collection was created by the rejected plan.
        assert!(c
            .execute("SELECT docs:missing", &Parameters::new())
            .is_err());
    }
    assert!(c.migrate(&[m(2, ""), m(1, "")]).is_err());
    q(&c, "BEGIN");
    assert!(c.migrate(&[m(1, "CREATE TABLE docs;")]).is_err());
    assert_eq!(c.transaction_state(), TransactionState::Active);
    q(&c, "ROLLBACK");
}

#[test]
fn persistent_migrations_retry_after_writer_conflict_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("migrations.db");
    let initial = m(
        1,
        "CREATE TABLE docs; CREATE TABLE locks(n INTEGER); CREATE UNIQUE INDEX docs_n ON docs(n);",
    );
    let pending = m(2, "INSERT INTO docs {id:docs:first,n:1}; CREATE TABLE applied(n INTEGER); INSERT INTO applied VALUES(2);");
    let plan = [initial, pending];
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        c.migrate(&plan[..1]).unwrap();
        let writer = db.connect().unwrap();
        q(&writer, "BEGIN");
        q(&writer, "INSERT INTO locks VALUES(9)");
        let error = c.migrate(&plan).unwrap_err();
        let cause = match &error {
            romedb::Error::Migration { source, .. } => source.as_ref(),
            other => other,
        };
        assert!(
            matches!(cause.code(), "FDB_BUSY" | "FDB_BUSY_SNAPSHOT"),
            "{error:?}"
        );
        assert_eq!(c.transaction_state(), TransactionState::Autocommit);
        assert_eq!(writer.transaction_state(), TransactionState::Active);
        assert!(q(&c, "SELECT * FROM docs").rows.is_empty());
        assert!(c
            .execute("SELECT * FROM applied", &Parameters::new())
            .is_err());
        q(&writer, "COMMIT");
        let report = c.migrate(&plan).unwrap();
        assert_eq!(report.already_applied, 1);
        assert_eq!(report.applied, vec![2]);
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        let report = c.migrate(&plan).unwrap();
        assert_eq!(report.already_applied, 2);
        assert!(report.applied.is_empty());
        assert_eq!(
            q(&c, "SELECT n FROM docs").rows,
            vec![vec![romedb::Value::Integer(1)]]
        );
        assert_eq!(
            q(&c, "SELECT n FROM applied").rows,
            vec![vec![romedb::Value::Integer(2)]]
        );
        assert_eq!(
            q(&c, "SELECT n FROM locks").rows,
            vec![vec![romedb::Value::Integer(9)]]
        );
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
        let mut changed = plan.clone();
        changed[1].name.push_str("_renamed");
        assert_eq!(c.migrate(&changed).unwrap_err().code(), "FDB_VALIDATION");
        assert_eq!(c.migrate(&plan).unwrap().already_applied, 2);
        assert_eq!(c.transaction_state(), TransactionState::Autocommit);
    }
}

#[test]
fn migration_plan_limits_reject_before_mutation_and_allow_boundary_retry() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let script_limit = 4 * 1024 * 1024;
    let first = m(1, "CREATE TABLE docs;");
    let mut invalid = Vec::new();
    invalid.push((
        vec![first.clone(), m(2, &" ".repeat(script_limit + 1))],
        "FDB_LIMIT",
    ));
    invalid.push((
        (1..=1001).map(|version| m(version, "")).collect(),
        "FDB_LIMIT",
    ));
    let comment = format!("--{}", " ".repeat(script_limit - 2));
    invalid.push((
        (1..=5).map(|version| m(version, &comment)).collect(),
        "FDB_LIMIT",
    ));
    for name in [
        "".to_owned(),
        "a".repeat(256),
        "é".repeat(128),
        "bad\0name".into(),
    ] {
        let mut next = m(2, "INSERT INTO docs {n:1};");
        next.name = name;
        invalid.push((vec![first.clone(), next], "FDB_VALIDATION"));
    }
    for version in [0, -1] {
        invalid.push((vec![m(version, "CREATE TABLE docs;")], "FDB_VALIDATION"));
    }
    for (plan, code) in invalid {
        assert_eq!(c.migrate(&plan).unwrap_err().code(), code);
        assert_eq!(c.transaction_state(), TransactionState::Autocommit);
        assert!(c.execute("SELECT * FROM docs", &Parameters::new()).is_err());
    }
    let mut boundary = first;
    boundary.name = format!("{}a", "é".repeat(127));
    assert_eq!(boundary.name.len(), 255);
    boundary.sql.push_str("--");
    boundary
        .sql
        .push_str(&" ".repeat(script_limit - boundary.sql.len()));
    assert_eq!(boundary.sql.len(), script_limit);
    let report = c.migrate(std::slice::from_ref(&boundary)).unwrap();
    assert_eq!(report.already_applied, 0);
    assert_eq!(report.applied, vec![1]);
    assert_eq!(c.migrate(&[boundary]).unwrap().already_applied, 1);
    assert!(q(&c, "SELECT * FROM docs").rows.is_empty());
}

#[test]
fn maximum_aggregate_utf8_history_survives_reopen_and_exact_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("boundary.db");
    let plan = (1..=4)
        .map(|version| {
            let mut sql = format!("CREATE TABLE docs{version}; --");
            let remaining = 4 * 1024 * 1024 - sql.len();
            sql.push_str(&"é".repeat(remaining / 2));
            if remaining % 2 != 0 {
                sql.push(' ');
            }
            assert_eq!(sql.len(), 4 * 1024 * 1024);
            m(version, &sql)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        plan.iter().map(|entry| entry.sql.len()).sum::<usize>(),
        16 * 1024 * 1024
    );
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        assert_eq!(c.migrate(&plan).unwrap().applied, vec![1, 2, 3, 4]);
        assert_eq!(c.transaction_state(), TransactionState::Autocommit);
    }
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        let report = c.migrate(&plan).unwrap();
        assert_eq!(report.already_applied, 4);
        assert!(report.applied.is_empty());
        for version in 1..=4 {
            assert!(q(&c, &format!("SELECT * FROM docs{version}"))
                .rows
                .is_empty());
        }
        assert_eq!(c.transaction_state(), TransactionState::Autocommit);
    }
}

#[test]
fn migration_failure_reports_utf8_statement_offset_and_underlying_error() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let initial = m(1, "CREATE TABLE docs; DEFINE FIELD n ON docs TYPE integer;");
    c.migrate(std::slice::from_ref(&initial)).unwrap();
    let prefix = "CREATE TABLE staged; -- café 日本語\n  ";
    let failing = "INSERT INTO docs {id:docs:first,n:'bad'};";
    let pending = m(7, &format!("{prefix}{failing} CREATE TABLE skipped;"));
    let mut plan = [initial, pending];
    let error = c.migrate(&plan).unwrap_err();
    assert_eq!(error.code(), "FDB_MIGRATION");
    match error {
        romedb::Error::Migration {
            version,
            offset,
            source,
        } => {
            assert_eq!(version, 7);
            assert_eq!(offset, prefix.len());
            assert_eq!(source.code(), "FDB_VALIDATION");
            assert!(offset > prefix.chars().count());
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(c.transaction_state(), TransactionState::Autocommit);
    assert!(q(&c, "SELECT * FROM docs").rows.is_empty());
    for table in ["staged", "skipped"] {
        assert!(c
            .execute(&format!("SELECT * FROM {table}"), &Parameters::new())
            .is_err());
    }
    plan[1].sql = plan[1].sql.replace("n:'bad'", "n:2");
    let report = c.migrate(&plan).unwrap();
    assert_eq!(report.already_applied, 1);
    assert_eq!(report.applied, vec![7]);
    assert_eq!(
        q(&c, "SELECT n FROM docs").rows,
        vec![vec![romedb::Value::Integer(2)]]
    );
    assert_eq!(c.migrate(&plan).unwrap().already_applied, 2);
}

#[test]
fn migration_preflight_syntax_identifies_version_without_mutation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let source = "-- café 日本語\nSELECT 'unterminated";
    let plan = [m(1, "CREATE TABLE docs;"), m(7, source)];
    let error = c.migrate(&plan).unwrap_err();
    assert_eq!(error.code(), "FDB_SYNTAX");
    match error {
        romedb::Error::Syntax(error) => {
            assert_eq!(error.offset, source.find('\'').unwrap());
            assert!(error.message.contains("migration 7:"));
            assert!(error.message.contains("unterminated quote"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(c.transaction_state(), TransactionState::Autocommit);
    assert!(c.execute("SELECT * FROM docs", &Parameters::new()).is_err());
    assert_eq!(c.migrate(&plan[..1]).unwrap().applied, vec![1]);
}
