use romedb::{CreateWorkLimits, Database, Parameters, ResultLimits, TransactionState, Value};
fn results() -> ResultLimits {
    ResultLimits {
        max_rows: 0,
        max_payload_bytes: 65536,
    }
}
fn exact() -> CreateWorkLimits {
    CreateWorkLimits {
        max_rows_read: Some(5),
        max_row_mutations: Some(5),
        max_schema_rows_read: Some(1000),
        max_vm_steps: Some(100000),
    }
}
#[test]
fn table_collection_and_scalar_index_creation_have_checked_work() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        ":memory:".to_owned(),
        dir.path().join("create.db").to_str().unwrap().to_owned(),
    ] {
        let c = Database::open(&path).unwrap().connect().unwrap();
        let p = Parameters::new();
        for sql in ["CREATE TABLE source(n INTEGER)", "CREATE TABLE docs"] {
            let m = c.create_metered(sql, &p, results(), exact()).unwrap();
            m.outcome.unwrap();
            assert_eq!(m.work.rows_read, 0, "{sql}");
            assert_eq!(m.work.row_mutations, 0, "{sql}");
            assert!(m.work.vm_steps > 0, "{sql}");
            assert!(m.work.schema_rows_read > 0, "{sql}");
        }
        c.execute("INSERT INTO source VALUES(1),(2),(3),(4),(5)", &p)
            .unwrap();
        for n in 1..=5 {
            c.execute(&format!("INSERT INTO docs {{id:docs:d{n},n:{n}}}"), &p)
                .unwrap();
        }
        for (sql, writes) in [
            ("CREATE INDEX source_n ON source(n)", 0),
            ("CREATE INDEX docs_n ON docs(n)", 0),
            ("CREATE TABLE copied AS SELECT n FROM source NOT INDEXED", 5),
        ] {
            let m = c.create_metered(sql, &p, results(), exact()).unwrap();
            m.outcome.unwrap_or_else(|e| panic!("{sql}: {e}"));
            assert_eq!(m.work.rows_read, 5, "{sql}: {:?}", m.work);
            assert_eq!(m.work.row_mutations, writes, "{sql}");
            assert!(m.work.schema_rows_read > 0, "{sql}");
        }
        assert_eq!(c.execute("SELECT * FROM copied", &p).unwrap().rows.len(), 5);
        assert_eq!(
            c.execute("SELECT * FROM docs WHERE n=3", &p)
                .unwrap()
                .rows
                .len(),
            1
        );
        for sql in [
            "CREATE TABLE IF NOT EXISTS docs",
            "CREATE INDEX IF NOT EXISTS docs_n ON docs(n)",
            "CREATE TABLE IF NOT EXISTS copied(n INTEGER)",
        ] {
            let m = c
                .create_metered(
                    sql,
                    &p,
                    results(),
                    CreateWorkLimits {
                        max_rows_read: Some(0),
                        max_row_mutations: Some(0),
                        ..Default::default()
                    },
                )
                .unwrap();
            m.outcome.unwrap();
            assert_eq!(m.work.rows_read, 0);
            assert_eq!(m.work.row_mutations, 0);
        }
    }
}
#[test]
fn creation_limits_rollback_schema_and_preserve_caller_work() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let p = Parameters::new();
    c.execute("CREATE TABLE source(n INTEGER)", &p).unwrap();
    c.execute("INSERT INTO source VALUES(1),(2),(3),(4),(5)", &p)
        .unwrap();
    c.execute("CREATE TABLE docs", &p).unwrap();
    for n in 1..=5 {
        c.execute(&format!("INSERT INTO docs {{id:docs:d{n},n:{n}}}"), &p)
            .unwrap();
    }
    c.execute("BEGIN", &p).unwrap();
    c.execute("INSERT INTO source VALUES(99)", &p).unwrap();
    for (sql, limits, expected) in [
        (
            "CREATE TABLE stopped AS SELECT * FROM source",
            CreateWorkLimits {
                max_rows_read: Some(2),
                ..Default::default()
            },
            "read",
        ),
        (
            "CREATE TABLE stopped AS SELECT * FROM source",
            CreateWorkLimits {
                max_row_mutations: Some(2),
                ..Default::default()
            },
            "mutation",
        ),
        (
            "CREATE TABLE stopped AS SELECT * FROM source",
            CreateWorkLimits {
                max_schema_rows_read: Some(0),
                ..Default::default()
            },
            "schema",
        ),
        (
            "CREATE TABLE stopped AS SELECT * FROM source",
            CreateWorkLimits {
                max_vm_steps: Some(0),
                ..Default::default()
            },
            "vm",
        ),
        (
            "CREATE INDEX stopped ON docs(n)",
            CreateWorkLimits {
                max_rows_read: Some(2),
                ..Default::default()
            },
            "read",
        ),
        (
            "CREATE TABLE stopped",
            CreateWorkLimits {
                max_vm_steps: Some(0),
                ..Default::default()
            },
            "vm",
        ),
    ] {
        let m = c.create_metered(sql, &p, results(), limits).unwrap();
        assert!(m.outcome.is_err(), "{sql}");
        match expected {
            "read" => {
                assert!(m.work.read_budget_exhausted);
                assert_eq!(m.work.rows_read, 3)
            }
            "mutation" => {
                assert!(m.work.mutation_budget_exhausted);
                assert_eq!(m.work.row_mutations, 3)
            }
            "schema" => {
                assert!(m.work.schema_budget_exhausted);
                assert_eq!(m.work.schema_rows_read, 1)
            }
            "vm" => {
                assert!(m.work.vm_budget_exhausted);
                assert_eq!(m.work.vm_steps, 0)
            }
            _ => unreachable!(),
        }
        assert_eq!(c.transaction_state(), TransactionState::Active);
        assert!(c.execute("INFO FOR TABLE stopped", &p).is_err());
        assert!(c.execute("INFO FOR INDEX stopped", &p).is_err());
        assert_eq!(c.execute("SELECT * FROM source", &p).unwrap().rows.len(), 6);
    }
    let m = c
        .create_metered(
            "CREATE TABLE limited AS SELECT n FROM source",
            &p,
            ResultLimits {
                max_rows: 0,
                max_payload_bytes: 0,
            },
            CreateWorkLimits::default(),
        )
        .unwrap();
    // Turso 0.8.1 CTAS returns no rows or columns, so a zero result budget
    // permits this command; the independent mutation budget still counts it.
    let result = m.outcome.unwrap();
    assert!(result.rows.is_empty());
    assert!(result.columns.is_empty());
    assert_eq!(m.work.row_mutations, 6);
    c.execute("INFO FOR TABLE limited", &p).unwrap();
    assert_eq!(c.transaction_state(), TransactionState::Active);
    c.execute("COMMIT", &p).unwrap();
    let m = c
        .create_metered(
            "CREATE TABLE stopped AS SELECT n FROM source",
            &p,
            results(),
            CreateWorkLimits::default(),
        )
        .unwrap();
    m.outcome.unwrap();
    assert_eq!(m.work.row_mutations, 6);
}
#[test]
fn invalid_or_uncovered_statements_never_execute_and_scopes_remain_reusable() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let p = Parameters::new();
    c.execute("CREATE TABLE docs", &p).unwrap();
    for sql in [
        "SELECT 1",
        "DELETE FROM docs",
        "DROP TABLE docs",
        "ALTER TABLE docs RENAME TO renamed",
        "INFO FOR DB",
        "CREATE FULLTEXT INDEX search ON docs(title)",
        "CREATE FUNCTION f() RETURNS integer RETURN 1",
    ] {
        let m = c.create_metered(sql, &p, results(), CreateWorkLimits::default());
        assert!(
            m.is_none() || m.is_some_and(|m| m.outcome.is_err()),
            "{sql}"
        );
    }
    c.execute("INFO FOR TABLE docs", &p).unwrap();
    for sql in [
        "CREATE TABLE docs; DELETE FROM docs",
        "CREATE TABLE",
        "CREATE TABLE __romedb_bad(n INTEGER)",
    ] {
        assert!(
            c.create_metered(sql, &p, results(), CreateWorkLimits::default())
                .unwrap()
                .outcome
                .is_err(),
            "{sql}"
        );
    }
    let params = Parameters::from([("unused".into(), Value::Integer(1))]);
    assert!(c
        .create_metered(
            "CREATE TABLE unwanted",
            &params,
            results(),
            CreateWorkLimits::default()
        )
        .unwrap()
        .outcome
        .is_err());
    assert!(c.execute("INFO FOR TABLE unwanted", &p).is_err());
    let params = Parameters::from([("$n".into(), Value::Integer(42))]);
    let m = c
        .create_metered(
            "CREATE TABLE copied AS SELECT $n AS n",
            &params,
            results(),
            CreateWorkLimits::default(),
        )
        .unwrap();
    m.outcome.unwrap();
    assert_eq!(m.work.rows_read, 0);
    assert_eq!(m.work.row_mutations, 1);
    assert_eq!(
        c.execute("SELECT n FROM copied", &p).unwrap().rows[0][0],
        Value::Integer(42)
    );
}
#[test]
fn unique_build_failure_and_explicit_rollback_keep_attempts_without_schema_leaks() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let p = Parameters::new();
    for sql in [
        "CREATE TABLE source(n INTEGER)",
        "INSERT INTO source VALUES(1),(1),(2)",
        "CREATE TABLE docs",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:1}",
    ] {
        c.execute(sql, &p).unwrap();
    }
    for table in ["source", "docs"] {
        let m = c
            .create_metered(
                &format!("CREATE UNIQUE INDEX failed ON {table}(n)"),
                &p,
                results(),
                CreateWorkLimits::default(),
            )
            .unwrap();
        assert!(m.outcome.is_err());
        assert!(m.work.rows_read >= 2);
        assert_eq!(m.work.row_mutations, 0);
        assert!(c.execute("INFO FOR INDEX failed", &p).is_err());
    }
    c.execute("BEGIN", &p).unwrap();
    let m = c
        .create_metered(
            "CREATE TABLE copied AS SELECT * FROM source",
            &p,
            results(),
            CreateWorkLimits::default(),
        )
        .unwrap();
    m.outcome.unwrap();
    assert_eq!(m.work.row_mutations, 3);
    c.execute("ROLLBACK", &p).unwrap();
    assert!(c.execute("INFO FOR TABLE copied", &p).is_err());
    assert_eq!(m.work.row_mutations, 3);
    // New catalog entries increase schema work, never source work.
    for i in 0..10 {
        c.execute(&format!("CREATE TABLE extra{i}(n INTEGER)"), &p)
            .unwrap();
    }
    let m = c
        .create_metered(
            "CREATE TABLE copied AS SELECT * FROM source",
            &p,
            results(),
            CreateWorkLimits::default(),
        )
        .unwrap();
    m.outcome.unwrap();
    assert_eq!(m.work.rows_read, 3);
    assert_eq!(m.work.row_mutations, 3);
}

fn search_seed(c: &romedb::Connection) {
    let p = Parameters::new();
    c.execute("CREATE TABLE docs", &p).unwrap();
    for n in 1..=5 {
        c.execute(&format!("INSERT INTO docs {{id:docs:d{n},title:'hello world',location:geo::point({n},0),v:vector32('[{n},0]')}}"),&p).unwrap();
    }
}
fn search_builds() -> [(&'static str, &'static str, &'static str); 3] {
    [
    ("CREATE SEARCH INDEX docs_search ON docs(title) USING FULLTEXT","SELECT id FROM search::text('docs_search','hello',10)","QUERY INDEX METHOD fts"),
    ("CREATE SEARCH INDEX docs_search ON docs(location) USING SPATIAL","SELECT id FROM search::near('docs_search',geo::point(0,0),1000000)","USING COVERING INDEX docs_search"),
    ("CREATE SEARCH INDEX docs_search ON docs(v) USING VECTOR WITH (metric='l2',dimensions=2)","SELECT id FROM search::vector('docs_search',vector32('[1,0]'),5)",""),
]
}
#[test]
fn search_index_builds_retain_source_work_without_logical_mutations() {
    let dir = tempfile::tempdir().unwrap();
    for (i, (sql, query, plan)) in search_builds().into_iter().enumerate() {
        for path in [
            ":memory:".to_owned(),
            dir.path()
                .join(format!("search-{i}.db"))
                .to_str()
                .unwrap()
                .to_owned(),
        ] {
            let c = Database::open(&path).unwrap().connect().unwrap();
            search_seed(&c);
            let m = c
                .create_metered(
                    sql,
                    &Parameters::new(),
                    results(),
                    CreateWorkLimits {
                        max_row_mutations: Some(0),
                        ..Default::default()
                    },
                )
                .unwrap();
            m.outcome.unwrap_or_else(|e| panic!("{sql}: {e}"));
            eprintln!("build {i}: {:?}", m.work);
            assert_eq!(
                m.work.rows_read,
                [5, 5, 10][i],
                "source plus instrumented maintenance visits"
            );
            assert_eq!(m.work.row_mutations, 0);
            assert!(m.work.vm_steps > 0);
            assert!(m.work.schema_rows_read > 0);
            assert_eq!(c.execute(query, &Parameters::new()).unwrap().rows.len(), 5);
            if !plan.is_empty() {
                let explained = c
                    .execute(&format!("EXPLAIN QUERY PLAN {query}"), &Parameters::new())
                    .unwrap();
                assert!(
                    format!("{:?}", explained.rows).contains(plan),
                    "{:?}",
                    explained.rows
                );
            }
            let no_op = sql.replace("CREATE SEARCH INDEX", "CREATE SEARCH INDEX IF NOT EXISTS");
            let m = c
                .create_metered(
                    &no_op,
                    &Parameters::new(),
                    results(),
                    CreateWorkLimits {
                        max_rows_read: Some(0),
                        max_row_mutations: Some(0),
                        ..Default::default()
                    },
                )
                .unwrap();
            m.outcome.unwrap();
            assert_eq!(m.work.rows_read, 0);
            assert_eq!(m.work.row_mutations, 0);
            if path != ":memory:" {
                drop(c);
                let c = Database::open(&path).unwrap().connect().unwrap();
                assert_eq!(c.execute(query, &Parameters::new()).unwrap().rows.len(), 5);
            }
        }
    }
}
#[test]
fn search_index_build_budget_failures_rollback_and_allow_fresh_builds() {
    for (sql, query, _) in search_builds() {
        let c = Database::open(":memory:").unwrap().connect().unwrap();
        search_seed(&c);
        c.execute("BEGIN", &Parameters::new()).unwrap();
        c.execute("INSERT INTO docs {id:docs:prior,title:'hello',location:geo::point(1,0),v:vector32('[1,0]')}",&Parameters::new()).unwrap();
        for (limits, flag) in [
            (
                CreateWorkLimits {
                    max_rows_read: Some(2),
                    ..Default::default()
                },
                "read",
            ),
            (
                CreateWorkLimits {
                    max_schema_rows_read: Some(0),
                    ..Default::default()
                },
                "schema",
            ),
            (
                CreateWorkLimits {
                    max_vm_steps: Some(0),
                    ..Default::default()
                },
                "vm",
            ),
        ] {
            let m = c
                .create_metered(sql, &Parameters::new(), results(), limits)
                .unwrap();
            assert!(m.outcome.is_err());
            assert_eq!(m.work.row_mutations, 0);
            match flag {
                "read" => {
                    assert!(m.work.read_budget_exhausted);
                    assert_eq!(m.work.rows_read, 3);
                }
                "schema" => assert!(m.work.schema_budget_exhausted),
                "vm" => assert!(m.work.vm_budget_exhausted),
                _ => unreachable!(),
            }
            assert_eq!(c.transaction_state(), TransactionState::Active);
            assert!(c
                .execute("INFO FOR INDEX docs_search", &Parameters::new())
                .is_err());
            assert_eq!(
                c.execute("SELECT * FROM docs", &Parameters::new())
                    .unwrap()
                    .rows
                    .len(),
                6
            );
        }
        let m = c
            .create_metered(
                sql,
                &Parameters::new(),
                results(),
                CreateWorkLimits::default(),
            )
            .unwrap();
        m.outcome.unwrap();
        assert!(c.execute(query, &Parameters::new()).is_ok());
        c.execute("ROLLBACK", &Parameters::new()).unwrap();
        assert!(c
            .execute("INFO FOR INDEX docs_search", &Parameters::new())
            .is_err());
        assert_eq!(
            c.execute("SELECT * FROM docs", &Parameters::new())
                .unwrap()
                .rows
                .len(),
            5
        );
    }
}

#[test]
fn search_build_late_vm_and_invalid_values_leave_no_partial_index() {
    for (i, (sql, query, _)) in search_builds().into_iter().enumerate() {
        let c = Database::open(":memory:").unwrap().connect().unwrap();
        search_seed(&c);
        let p = Parameters::new();
        c.execute("BEGIN", &p).unwrap();
        c.execute("SAVEPOINT baseline", &p).unwrap();
        let baseline = c
            .create_metered(sql, &p, results(), CreateWorkLimits::default())
            .unwrap();
        baseline.outcome.unwrap();
        c.execute("ROLLBACK TO baseline", &p).unwrap();
        c.execute("RELEASE baseline", &p).unwrap();
        let stopped = c
            .create_metered(
                sql,
                &p,
                results(),
                CreateWorkLimits {
                    max_vm_steps: Some(baseline.work.vm_steps - 1),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(stopped.outcome.is_err(), "{sql}: {:?}", stopped.work);
        assert!(stopped.work.vm_budget_exhausted);
        assert_eq!(stopped.work.row_mutations, 0);
        assert!(stopped.work.rows_read >= 5);
        assert_eq!(c.transaction_state(), TransactionState::Active);
        assert!(c.execute("INFO FOR INDEX docs_search", &p).is_err());
        c.execute("ROLLBACK", &p).unwrap();
        let (bad, good) = [
            ("title:99", "title:'hello'"),
            ("location:'bad'", "location:geo::point(1,0)"),
            ("v:vector32('[1,2,3]')", "v:vector32('[1,0]')"),
        ][i];
        c.execute(&format!("UPDATE docs:d5 {{{bad}}}"), &p).unwrap();
        let failed = c
            .create_metered(sql, &p, results(), CreateWorkLimits::default())
            .unwrap();
        assert!(failed.outcome.is_err());
        assert_eq!(failed.work.row_mutations, 0);
        assert!(failed.work.rows_read >= 5);
        assert!(c.execute("INFO FOR INDEX docs_search", &p).is_err());
        c.execute(&format!("UPDATE docs:d5 {{{good}}}"), &p)
            .unwrap();
        c.create_metered(sql, &p, results(), CreateWorkLimits::default())
            .unwrap()
            .outcome
            .unwrap();
        assert_eq!(c.execute(query, &p).unwrap().rows.len(), 5);
    }
}
