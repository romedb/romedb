use romedb::{Database, Error, Parameters, ResultLimits};

#[test]
fn result_limits_cover_native_and_typed_rows_and_preserve_pending_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let p = Parameters::new();
    for sql in [
        "CREATE TABLE native(n INTEGER)",
        "INSERT INTO native VALUES(1),(2)",
        "CREATE TABLE docs;",
        "INSERT INTO docs {n:1}",
        "INSERT INTO docs {n:2}",
        "BEGIN",
        "INSERT INTO native VALUES(3)",
    ] {
        c.execute(sql, &p).unwrap();
    }
    for sql in [
        "SELECT n AS n FROM native WHERE n<3 ORDER BY n",
        "SELECT n AS n FROM docs ORDER BY n",
    ] {
        let expected = c.execute(sql, &p).unwrap();
        let exact = ResultLimits {
            max_rows: 2,
            max_payload_bytes: 17,
        };
        assert_eq!(
            c.select_with_limits(sql, &p, exact).unwrap().rows,
            expected.rows
        );
        assert_eq!(
            c.profile_select_with_limits(sql, &p, exact)
                .unwrap()
                .result
                .rows,
            expected.rows
        );
        for limits in [
            ResultLimits {
                max_rows: 1,
                ..exact
            },
            ResultLimits {
                max_payload_bytes: 16,
                ..exact
            },
            ResultLimits {
                max_rows: 0,
                ..exact
            },
        ] {
            assert!(matches!(
                c.select_with_limits(sql, &p, limits),
                Err(Error::Limit(_))
            ));
        }
    }
    let zero = ResultLimits {
        max_rows: 0,
        max_payload_bytes: 1,
    };
    assert!(c
        .select_with_limits("SELECT n FROM native WHERE 0", &p, zero)
        .unwrap()
        .rows
        .is_empty());
    assert!(matches!(
        c.select_with_limits(
            "SELECT n FROM native WHERE 0",
            &p,
            ResultLimits {
                max_payload_bytes: 0,
                ..zero
            }
        ),
        Err(Error::Limit(_))
    ));
    assert!(matches!(
        c.select_with_limits("DELETE FROM native", &p, zero),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(c.execute("SELECT n FROM native", &p).unwrap().rows.len(), 3);
    c.execute("ROLLBACK", &p).unwrap();
    assert_eq!(c.execute("SELECT n FROM native", &p).unwrap().rows.len(), 2);
}

#[test]
fn fetch_limits_charge_expanded_duplicates_missing_targets_and_other_columns() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let p = Parameters::new();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs {id:docs:a,n:7}",
        "CREATE TABLE native(id INTEGER PRIMARY KEY,n TEXT)",
        "INSERT INTO native VALUES(1,'猫')",
        "CREATE TABLE positions(n INTEGER)",
        "INSERT INTO positions VALUES(1),(2)",
        "BEGIN",
        "INSERT INTO docs {id:docs:pending,n:9}",
    ] {
        c.execute(sql, &p).unwrap();
    }
    for (sql, bytes) in [
        // v + {id: docs:a, n:7} = 1 + 2+5+1+8.
        ("SELECT record::fetch(docs:a) AS v", 17),
        ("SELECT record::fetch(docs:a) AS v FROM positions", 33),
        // Two fetched columns plus a native integer column.
        (
            "SELECT record::fetch(docs:a) AS v,record::fetch(docs:a) AS w,1 AS n",
            43,
        ),
        // Native target object: id key+integer, n key+UTF-8 value.
        ("SELECT record::fetch(native:1) AS v", 15),
        ("SELECT record::fetch(docs:absent) AS v", 2),
        ("SELECT record::fetch(unknown:absent) AS v", 2),
        ("SELECT record::fetch(NULL) AS v", 2),
        // Reference length must not count when its resolved value is null.
        (
            "SELECT record::fetch(docs:averylongmissingkey) AS v FROM positions",
            3,
        ),
    ] {
        let expected = c.profile_select(sql, &p).unwrap();
        let limits = ResultLimits {
            max_rows: expected.result.rows.len(),
            max_payload_bytes: bytes,
        };
        let actual = c.profile_select_with_limits(sql, &p, limits).unwrap();
        assert_eq!(actual.result.rows, expected.result.rows, "{sql}");
        assert_eq!(
            actual.metrics.fetch_batches, expected.metrics.fetch_batches,
            "{sql}"
        );
        assert!(
            matches!(
                c.select_with_limits(
                    sql,
                    &p,
                    ResultLimits {
                        max_payload_bytes: bytes - 1,
                        ..limits
                    }
                ),
                Err(Error::Limit(_))
            ),
            "{sql}"
        );
        assert!(
            matches!(
                c.select_with_limits(
                    sql,
                    &p,
                    ResultLimits {
                        max_rows: limits.max_rows - 1,
                        ..limits
                    }
                ),
                Err(Error::Limit(_))
            ),
            "{sql}"
        );
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(
            c.select_with_limits(sql, &p, limits).unwrap().rows,
            expected.result.rows
        );
    }
    assert_eq!(c.execute("SELECT n FROM docs", &p).unwrap().rows.len(), 2);
    c.execute("ROLLBACK", &p).unwrap();
    assert_eq!(c.execute("SELECT n FROM docs", &p).unwrap().rows.len(), 1);
}

#[test]
fn write_result_limit_failure_restores_statement_and_prior_work() {
    for outer in [false, true] {
        for (setup, write, read) in [
            (
                "INSERT INTO docs {id:docs:a,n:1}",
                "INSERT INTO native(n) SELECT n FROM docs RETURNING n",
                "SELECT n FROM native ORDER BY n",
            ),
            (
                "INSERT INTO native VALUES(1),(2)",
                "UPDATE native SET n=n+10 RETURNING n",
                "SELECT n FROM native ORDER BY n",
            ),
            (
                "INSERT INTO native VALUES(1),(2)",
                "DELETE FROM native RETURNING n",
                "SELECT n FROM native ORDER BY n",
            ),
            (
                "INSERT INTO native VALUES(1),(2)",
                "INSERT INTO native SELECT n+10 FROM native RETURNING n",
                "SELECT n FROM native ORDER BY n",
            ),
            (
                "INSERT INTO docs {id:docs:a,n:1}",
                "UPDATE docs SET n=n+10 RETURNING n",
                "SELECT n FROM docs",
            ),
            (
                "INSERT INTO docs {id:docs:a,n:1}",
                "UPDATE docs SET (n,a)=(SELECT n+10,n) RETURNING n",
                "SELECT * FROM docs",
            ),
            (
                "INSERT INTO docs {id:docs:a,n:1}",
                "UPDATE docs SET (n,a)=(SELECT x.n+10,x.n FROM docs x WHERE x.id=docs.id) RETURNING n",
                "SELECT * FROM docs",
            ),
            (
                "INSERT INTO docs {id:docs:a,n:1}",
                "UPDATE docs {n:n+10} RETURNING n",
                "SELECT n FROM docs",
            ),
            (
                "INSERT INTO docs {id:docs:a,n:1}",
                "UPDATE docs:a {n:n+10} RETURNING n",
                "SELECT n FROM docs",
            ),
            (
                "INSERT INTO docs {id:docs:a,n:1}",
                "UPSERT docs:a {n:n+10} RETURNING n",
                "SELECT n FROM docs",
            ),
            (
                "INSERT INTO docs {id:docs:a,n:1}",
                "DELETE FROM docs:a RETURNING n",
                "SELECT n FROM docs",
            ),
            (
                "INSERT INTO docs {id:docs:a,n:1}",
                "INSERT INTO docs {id:docs:b,n:2} RETURNING n",
                "SELECT n FROM docs ORDER BY n",
            ),
            (
                "INSERT INTO docs {id:docs:a,n:1}",
                "INSERT INTO docs(n) SELECT n+10 FROM docs RETURNING n",
                "SELECT n FROM docs ORDER BY n",
            ),
        ] {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            let p = Parameters::new();
            for sql in [
                "CREATE TABLE native(n INTEGER)",
                "CREATE TABLE docs",
                "CREATE INDEX by_n ON docs(n)",
                "CREATE TABLE prior(n INTEGER)",
                setup,
            ] {
                c.execute(sql, &p).unwrap();
            }
            if outer {
                c.execute("BEGIN", &p).unwrap();
                c.execute("INSERT INTO prior VALUES(7)", &p).unwrap();
            }
            let before = c.execute(read, &p).unwrap().rows;
            let state = c.transaction_state();
            for limits in [
                ResultLimits {
                    max_rows: 0,
                    max_payload_bytes: 100,
                },
                ResultLimits {
                    max_rows: 100,
                    max_payload_bytes: 1,
                },
            ] {
                assert!(
                    matches!(
                        c.write_with_result_limits(write, &p, limits),
                        Err(Error::Limit(_))
                    ),
                    "{write}"
                );
                assert_eq!(c.transaction_state(), state, "{write}");
                assert_eq!(c.execute(read, &p).unwrap().rows, before, "{write}");
                c.check_collection_integrity("docs", romedb::IntegrityLimits::default())
                    .unwrap();
            }
            let result = c
                .write_with_result_limits(
                    write,
                    &p,
                    ResultLimits {
                        max_rows: 2,
                        max_payload_bytes: 17,
                    },
                )
                .unwrap();
            assert!(!result.rows.is_empty());
            if outer {
                assert_eq!(c.execute("SELECT n FROM prior", &p).unwrap().rows.len(), 1);
                c.execute("ROLLBACK", &p).unwrap();
                assert_eq!(c.execute(read, &p).unwrap().rows, before);
            }
        }
    }
}

#[test]
fn write_result_limits_reject_transaction_escape_and_charge_empty_metadata() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let p = Parameters::new();
    c.execute("CREATE TABLE native(n INTEGER)", &p).unwrap();
    c.execute("BEGIN", &p).unwrap();
    let zero = ResultLimits {
        max_rows: 0,
        max_payload_bytes: 0,
    };
    for sql in [
        "COMMIT",
        "ROLLBACK",
        "SAVEPOINT user_frame",
        "CREATE TABLE forbidden_table(n)",
        "SELECT 1",
        "PRAGMA user_version=7",
        "EXPLAIN DELETE FROM native",
    ] {
        assert!(
            matches!(
                c.write_with_result_limits(sql, &p, zero),
                Err(Error::Unsupported(_))
            ),
            "{sql}"
        );
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    }
    assert!(c
        .write_with_result_limits("INSERT INTO native VALUES(1); COMMIT", &p, zero)
        .is_err());
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert!(c
        .execute("SELECT n FROM native", &p)
        .unwrap()
        .rows
        .is_empty());
    assert!(matches!(
        c.write_with_result_limits("UPDATE native SET n=2 WHERE 0 RETURNING n", &p, zero),
        Err(Error::Limit(_))
    ));
    let result = c
        .write_with_result_limits("INSERT INTO native VALUES(1)", &p, zero)
        .unwrap();
    assert_eq!(result.affected, 1);
    assert!(result.rows.is_empty());
    c.execute("ROLLBACK", &p).unwrap();
}

#[test]
fn rejected_write_result_rolls_back_trigger_effects_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("write-budget.db");
    let p = Parameters::new();
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE items(n INTEGER UNIQUE)", "CREATE TABLE audit(n INTEGER)",
            "CREATE TRIGGER audit_insert AFTER INSERT ON items BEGIN INSERT INTO audit VALUES(new.n); END",
            "INSERT INTO items VALUES(7)", "BEGIN", "INSERT INTO items VALUES(8)",
        ] { c.execute(sql, &p).unwrap(); }
        let write = "INSERT INTO items VALUES(1),(2) RETURNING n";
        let error = c
            .write_with_result_limits(
                write,
                &p,
                ResultLimits {
                    max_rows: 1,
                    max_payload_bytes: 100,
                },
            )
            .unwrap_err();
        assert_eq!(error.code(), "FDB_LIMIT");
        for table in ["items", "audit"] {
            assert_eq!(
                c.execute(&format!("SELECT n FROM {table} ORDER BY n"), &p)
                    .unwrap()
                    .rows,
                vec![
                    vec![romedb::Value::Integer(7)],
                    vec![romedb::Value::Integer(8)]
                ]
            );
        }
        let exact = ResultLimits {
            max_rows: 2,
            max_payload_bytes: 17,
        };
        assert_eq!(
            c.write_with_result_limits(write, &p, exact)
                .unwrap()
                .affected,
            2
        );
        assert!(c
            .write_with_result_limits("INSERT INTO items VALUES(3),(7) RETURNING n", &p, exact)
            .is_err());
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        c.execute("COMMIT", &p).unwrap();
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    for table in ["items", "audit"] {
        assert_eq!(
            c.execute(&format!("SELECT n FROM {table} ORDER BY n"), &p)
                .unwrap()
                .rows,
            [1, 2, 7, 8]
                .map(|n| vec![romedb::Value::Integer(n)])
                .to_vec()
        );
    }
}

#[test]
fn collection_returning_star_uses_document_payload_budget() {
    for write in [
        "UPDATE docs SET n=2 RETURNING *",
        "UPDATE docs {n:2} RETURNING *",
        "UPSERT docs:a {n:2} RETURNING *",
        "DELETE FROM docs:a RETURNING *",
    ] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let p = Parameters::new();
        for sql in [
            "CREATE TABLE docs",
            "CREATE INDEX docs_n ON docs(n)",
            "INSERT INTO docs {id:docs:a,n:1}",
            "BEGIN",
        ] {
            c.execute(sql, &p).unwrap();
        }
        let before = c.execute("SELECT * FROM docs", &p).unwrap().rows;
        for limits in [
            ResultLimits {
                max_rows: 0,
                max_payload_bytes: 24,
            },
            ResultLimits {
                max_rows: 1,
                max_payload_bytes: 23,
            },
        ] {
            assert_eq!(
                c.write_with_result_limits(write, &p, limits)
                    .unwrap_err()
                    .code(),
                "FDB_LIMIT"
            );
            assert_eq!(c.execute("SELECT * FROM docs", &p).unwrap().rows, before);
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
            c.check_collection_integrity("docs", Default::default())
                .unwrap();
        }
        let result = c
            .write_with_result_limits(
                write,
                &p,
                ResultLimits {
                    max_rows: 1,
                    max_payload_bytes: 24,
                },
            )
            .unwrap();
        assert_eq!(result.columns, vec!["document"]);
        assert_eq!(result.rows.len(), 1);
        c.execute("ROLLBACK", &p).unwrap();
        assert_eq!(c.execute("SELECT * FROM docs", &p).unwrap().rows, before);
    }
}

#[test]
fn maximum_depth_payload_limits_preserve_atomic_returning_and_indexes() {
    use romedb::{IntegrityLimits, Key, Record, TransactionState, Value};
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let empty = Parameters::new();
    let mut value = Value::Record(Record {
        table: "docs".into(),
        key: Key::Integer(i64::MAX),
    });
    // Record table (4) + integer key (8); arrays add no logical payload bytes.
    let mut bytes = 12;
    for depth in 0..63 {
        value = if depth % 2 == 0 {
            Value::Array(vec![value])
        } else {
            bytes += "nested".len();
            Value::Object([("nested".into(), value)].into())
        };
    }
    for sql in [
        "CREATE TABLE docs",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
        "INSERT INTO docs {id:docs:saved,n:1}",
        "BEGIN",
        "INSERT INTO docs {id:docs:pending,n:2}",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    let params = Parameters::from([("$v".into(), value.clone())]);
    let exact = ResultLimits {
        max_rows: 1,
        max_payload_bytes: bytes + 1,
    };
    let short = ResultLimits {
        max_payload_bytes: exact.max_payload_bytes - 1,
        ..exact
    };
    let write = "UPDATE docs SET payload=$v,n=3 WHERE id=docs:saved RETURNING payload AS v";
    assert_eq!(
        c.write_with_result_limits(write, &params, short)
            .unwrap_err()
            .code(),
        "FDB_LIMIT"
    );
    assert_eq!(c.transaction_state(), TransactionState::Active);
    assert_eq!(
        c.lookup_index("docs", "docs_n", &Value::Integer(1))
            .unwrap()
            .len(),
        1
    );
    assert!(c
        .lookup_index("docs", "docs_n", &Value::Integer(3))
        .unwrap()
        .is_empty());
    assert_eq!(
        c.execute("SELECT payload FROM docs WHERE id=docs:saved", &empty)
            .unwrap()
            .rows,
        vec![vec![Value::Null]]
    );
    assert_eq!(
        c.write_with_result_limits(write, &params, exact)
            .unwrap()
            .rows,
        vec![vec![value.clone()]]
    );
    for (sql, params) in [
        ("SELECT $v AS v", &params),
        ("SELECT payload AS v FROM docs WHERE id=docs:saved", &empty),
    ] {
        assert_eq!(
            c.select_with_limits(sql, params, exact).unwrap().rows,
            vec![vec![value.clone()]]
        );
        assert_eq!(
            c.profile_select_with_limits(sql, params, exact)
                .unwrap()
                .result
                .rows,
            vec![vec![value.clone()]]
        );
        assert_eq!(
            c.select_with_limits(sql, params, short).unwrap_err().code(),
            "FDB_LIMIT"
        );
        assert_eq!(
            c.profile_select_with_limits(sql, params, short)
                .unwrap_err()
                .code(),
            "FDB_LIMIT"
        );
        assert_eq!(c.transaction_state(), TransactionState::Active);
    }
    assert_eq!(
        c.check_collection_integrity("docs", IntegrityLimits::default())
            .unwrap()
            .documents,
        2
    );
    assert_eq!(
        c.lookup_index("docs", "docs_n", &Value::Integer(3))
            .unwrap()
            .len(),
        1
    );
    c.execute("ROLLBACK", &empty).unwrap();
    assert_eq!(
        c.execute("SELECT n,payload FROM docs", &empty)
            .unwrap()
            .rows,
        vec![vec![Value::Integer(1), Value::Null]]
    );
    assert_eq!(
        c.check_collection_integrity("docs", IntegrityLimits::default())
            .unwrap()
            .documents,
        1
    );
}
