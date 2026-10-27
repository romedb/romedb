use romedb::{Database, Parameters, ResultLimits, TransactionState, Value};

#[test]
fn collection_write_buffer_rows_reject_before_mutation_and_preserve_prior_work() {
    for sql in [
        "INSERT INTO docs(n) VALUES(4),(5)",
        "INSERT OR ROLLBACK INTO docs(n) VALUES(4),(5)",
        "INSERT OR IGNORE INTO docs(n) VALUES(4),(5)",
        "INSERT OR IGNORE INTO docs(n) SELECT n+10 FROM docs",
        "INSERT OR ROLLBACK INTO docs(n) SELECT n+10 FROM docs",
        "INSERT INTO docs(n) SELECT n+10 FROM docs",
        "UPDATE docs SET n=n+10",
        "UPDATE OR ROLLBACK docs SET n=n+10",
        "UPDATE OR IGNORE docs SET n=n+10",
        "UPDATE OR IGNORE docs SET n=docs.n+10 FROM (SELECT 1 AS k) source",
        "UPDATE OR ROLLBACK docs SET n=docs.n+10 FROM (SELECT 1 AS k) source",
        "UPDATE docs SET (n,a)=(SELECT n+10,n)",
        "UPDATE docs SET (n,a)=(SELECT x.n+10,x.n FROM docs x WHERE x.id=docs.id)",
        "UPDATE docs {n:n+10}",
        "UPDATE docs CONTENT {n:n+10}",
        "UPDATE docs MERGE {n:n+10}",
        "UPDATE docs PATCH [{op:'replace',path:'/n',value:n+10}]",
        "DELETE FROM docs",
    ] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let p = Parameters::new();
        for sql in [
            "CREATE TABLE docs",
            "CREATE UNIQUE INDEX docs_n ON docs(n)",
            "INSERT INTO docs {id:docs:a,n:1}",
            "BEGIN",
            "INSERT INTO docs {id:docs:b,n:2}",
        ] {
            c.execute(sql, &p).unwrap();
        }
        let before = c.execute("SELECT * FROM docs ORDER BY n", &p).unwrap().rows;
        let c = c.with_write_buffer_limits(ResultLimits {
            max_rows: 1,
            max_payload_bytes: 10000,
        });
        assert_eq!(c.execute(sql, &p).unwrap_err().code(), "FDB_LIMIT", "{sql}");
        assert_eq!(c.transaction_state(), TransactionState::Active);
        assert_eq!(
            c.execute("SELECT * FROM docs ORDER BY n", &p).unwrap().rows,
            before
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .documents,
            2
        );
        let c = c.with_write_buffer_limits(ResultLimits {
            max_rows: 4,
            max_payload_bytes: 10000,
        });
        c.execute(sql, &p).unwrap();
        c.execute("ROLLBACK", &p).unwrap();
        assert_eq!(
            c.execute("SELECT n FROM docs", &p).unwrap().rows,
            vec![vec![Value::Integer(1)]]
        );
    }
}

#[test]
fn write_snapshot_limit_checks_generated_document_before_retention() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let p = Parameters::new();
    c.execute("CREATE TABLE docs", &p).unwrap();
    c.execute("BEGIN", &p).unwrap();
    // VALUES costs 5-byte record + 8-byte integer. The stored document adds
    // the keys id (2) and n (1), giving a 16-byte snapshot.
    let c = c.with_write_buffer_limits(ResultLimits {
        max_rows: 1,
        max_payload_bytes: 15,
    });
    for sql in [
        "INSERT INTO docs(id,n) VALUES(docs:z,4)",
        "INSERT INTO docs {id:docs:z,n:4}",
        "UPSERT docs:z {n:4}",
    ] {
        assert_eq!(c.execute(sql, &p).unwrap_err().code(), "FDB_LIMIT", "{sql}");
        assert!(c.execute("SELECT * FROM docs", &p).unwrap().rows.is_empty());
        assert_eq!(c.transaction_state(), TransactionState::Active);
    }
    let c = c.with_write_buffer_limits(ResultLimits {
        max_rows: 1,
        max_payload_bytes: 16,
    });
    c.execute("INSERT INTO docs(id,n) VALUES(docs:z,4)", &p)
        .unwrap();
    c.execute("ROLLBACK", &p).unwrap();
    assert!(c.execute("SELECT * FROM docs", &p).unwrap().rows.is_empty());
}

#[test]
fn bound_assignment_payload_is_counted_and_reads_keep_their_policy() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let p = Parameters::new();
    c.execute("CREATE TABLE docs", &p).unwrap();
    c.execute("INSERT INTO docs {id:docs:a,n:1}", &p).unwrap();
    let c = c.with_write_buffer_limits(ResultLimits {
        max_rows: 1,
        max_payload_bytes: 64,
    });
    let params = Parameters::from([("$blob".into(), Value::Binary(vec![7; 128]))]);
    for sql in [
        "UPDATE docs SET payload=$blob",
        "UPDATE docs {payload:$blob}",
    ] {
        assert_eq!(c.execute(sql, &params).unwrap_err().code(), "FDB_LIMIT");
        assert_eq!(
            c.execute("SELECT payload FROM docs", &p).unwrap().rows,
            vec![vec![Value::Null]]
        );
        assert_eq!(c.transaction_state(), TransactionState::Autocommit);
    }
    // This setting does not cap ordinary reads or underlying native SQL buffers.
    assert_eq!(
        c.execute("SELECT $blob", &params).unwrap().rows,
        vec![vec![Value::Binary(vec![7; 128])]]
    );
    let c = c.with_write_buffer_limits(ResultLimits {
        max_rows: 1,
        max_payload_bytes: 48,
    });
    // Restored parameters must share the candidate query's metadata budget.
    // The updated document itself is small enough; dropping column-name bytes
    // during parameter restoration would incorrectly accept this candidate.
    let text = Parameters::from([("$text".into(), Value::String("abcdefghijkl".into()))]);
    assert_eq!(
        c.execute("UPDATE docs SET n=$text", &text)
            .unwrap_err()
            .code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        c.execute("SELECT n FROM docs", &p).unwrap().rows,
        vec![vec![Value::Integer(1)]]
    );
    let c = c.with_write_buffer_limits(ResultLimits {
        max_rows: 1,
        max_payload_bytes: 1024,
    });
    c.execute("UPDATE docs SET payload=$blob", &params).unwrap();
    assert_eq!(
        c.execute("SELECT payload FROM docs", &p).unwrap().rows,
        vec![vec![Value::Binary(vec![7; 128])]]
    );
}

#[test]
fn zero_candidate_rows_allow_empty_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let p = Parameters::new();
    c.execute("CREATE TABLE docs", &p).unwrap();
    let c = c.with_write_buffer_limits(ResultLimits {
        max_rows: 0,
        max_payload_bytes: 1000,
    });
    for sql in [
        "UPDATE docs SET n=1 WHERE 0",
        "DELETE FROM docs WHERE 0",
        "UPDATE docs {n:1} WHERE 0",
    ] {
        assert_eq!(c.execute(sql, &p).unwrap().affected, 0);
    }
    assert_eq!(
        c.execute("INSERT INTO docs {n:1}", &p).unwrap_err().code(),
        "FDB_LIMIT"
    );
    assert!(c.execute("SELECT * FROM docs", &p).unwrap().rows.is_empty());
}

#[test]
fn batch_buffer_rejection_stops_before_commit_and_preserves_prior_statement() {
    let db = Database::open(":memory:").unwrap();
    let c = db
        .connect()
        .unwrap()
        .with_write_buffer_limits(ResultLimits {
            max_rows: 1,
            max_payload_bytes: 1000,
        });
    let p = Parameters::new();
    c.execute("CREATE TABLE docs", &p).unwrap();
    c.execute("INSERT INTO docs {id:docs:a,n:1}", &p).unwrap();
    let script = "BEGIN; INSERT INTO docs {id:docs:b,n:2}; UPDATE docs SET n=n+10; COMMIT;";
    let reports = c.execute_batch(script).unwrap();
    assert_eq!(reports.len(), 3);
    let rejected = &reports[2];
    assert_eq!(rejected.offset, script.find("UPDATE").unwrap());
    assert_eq!(
        rejected.execution.result.as_ref().unwrap_err().code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        rejected.execution.transaction_after,
        TransactionState::Active
    );
    assert_eq!(
        c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    c.execute("ROLLBACK", &p).unwrap();
    assert_eq!(
        c.execute("SELECT n FROM docs", &p).unwrap().rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn migration_buffer_rejection_restores_pending_schema_data_and_history() {
    use romedb::{Error, Migration};
    let db = Database::open(":memory:").unwrap();
    let c = db
        .connect()
        .unwrap()
        .with_write_buffer_limits(ResultLimits {
            max_rows: 1,
            max_payload_bytes: 1000,
        });
    let p = Parameters::new();
    let plan = vec![
        Migration {version:1, name:"initial".into(), sql:"CREATE TABLE docs; CREATE UNIQUE INDEX docs_n ON docs(n); INSERT INTO docs {id:docs:a,n:1};".into()},
        Migration {version:2, name:"pending".into(), sql:"CREATE TABLE audit(n); INSERT INTO audit VALUES(9); INSERT INTO docs {id:docs:b,n:2};".into()},
        Migration {version:3, name:"rewrite".into(), sql:"UPDATE docs SET n=n+10;".into()},
    ];
    c.migrate(&plan[..1]).unwrap();
    let error = c.migrate(&plan).unwrap_err();
    assert_eq!(error.code(), "FDB_MIGRATION");
    let Error::Migration { source, .. } = error else {
        panic!("expected migration context")
    };
    assert_eq!(source.code(), "FDB_LIMIT");
    assert_eq!(c.transaction_state(), TransactionState::Autocommit);
    assert!(c.execute("SELECT * FROM audit", &p).is_err());
    assert_eq!(
        c.execute("SELECT n FROM docs", &p).unwrap().rows,
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(c.migrate(&plan[..1]).unwrap().already_applied, 1);
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .documents,
        1
    );
    let c = c.with_write_buffer_limits(ResultLimits {
        max_rows: 2,
        max_payload_bytes: 1000,
    });
    assert_eq!(c.migrate(&plan).unwrap().applied, vec![2, 3]);
    assert_eq!(c.migrate(&plan).unwrap().already_applied, 3);
    assert_eq!(
        c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows,
        vec![vec![Value::Integer(11)], vec![Value::Integer(12)]]
    );
    assert_eq!(
        c.execute("SELECT n FROM audit", &p).unwrap().rows,
        vec![vec![Value::Integer(9)]]
    );
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .documents,
        2
    );
}

#[test]
fn update_snapshot_rejects_before_validating_oversized_new_fields() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let p = Parameters::new();
    let field = "long_field_name_".repeat(8);
    c.execute("CREATE TABLE docs", &p).unwrap();
    c.execute(&format!("DEFINE FIELD {field} ON docs TYPE integer"), &p)
        .unwrap();
    c.execute("INSERT INTO docs {id:docs:a,n:1}", &p).unwrap();
    c.execute("BEGIN", &p).unwrap();
    c.execute("INSERT INTO docs {id:docs:b,n:2}", &p).unwrap();
    // The candidate holds a short scalar assignment; the resulting snapshot
    // also owns the long destination field name, which exceeds this budget.
    let c = c.with_write_buffer_limits(ResultLimits {
        max_rows: 2,
        max_payload_bytes: 100,
    });
    let sql = format!("UPDATE docs SET {field}='bad' WHERE n=2");
    assert_eq!(c.execute(&sql, &p).unwrap_err().code(), "FDB_LIMIT");
    assert_eq!(c.transaction_state(), TransactionState::Active);
    assert_eq!(
        c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    // A larger snapshot budget reaches the actual field validator.
    let c = c.with_write_buffer_limits(ResultLimits {
        max_rows: 2,
        max_payload_bytes: 1000,
    });
    assert_eq!(c.execute(&sql, &p).unwrap_err().code(), "FDB_VALIDATION");
    c.execute(&format!("UPDATE docs SET {field}=3 WHERE n=2"), &p)
        .unwrap();
    c.execute("ROLLBACK", &p).unwrap();
    assert_eq!(
        c.execute("SELECT n FROM docs", &p).unwrap().rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn joined_duplicates_do_not_bypass_raw_candidate_row_limits() {
    {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let p = Parameters::new();
        for sql in [
            "CREATE TABLE docs",
            "CREATE INDEX docs_v ON docs(v)",
            "INSERT INTO docs {id:docs:a,n:1,v:0}",
            "CREATE TABLE source(k INTEGER,v INTEGER)",
            "INSERT INTO source VALUES(1,10),(1,11),(1,12)",
            "BEGIN",
            "INSERT INTO docs {id:docs:b,n:2,v:0}",
        ] {
            c.execute(sql, &p).unwrap();
        }
        let before = c.execute("SELECT * FROM docs ORDER BY n", &p).unwrap().rows;
        let c = c.with_write_buffer_limits(ResultLimits {
            max_rows: 2,
            max_payload_bytes: 10000,
        });
        let sql = "UPDATE docs SET v=s.v FROM source s WHERE docs.n=s.k RETURNING n,v";
        assert_eq!(c.execute(sql, &p).unwrap_err().code(), "FDB_LIMIT");
        assert_eq!(c.transaction_state(), TransactionState::Active);
        assert_eq!(
            c.execute("SELECT * FROM docs ORDER BY n", &p).unwrap().rows,
            before
        );
        let audit = c
            .check_collection_integrity("docs", Default::default())
            .unwrap();
        assert_eq!(audit.documents, 2);
        assert_eq!(audit.index_entries, 2);
        let c = c.with_write_buffer_limits(ResultLimits {
            max_rows: 10,
            max_payload_bytes: 10000,
        });
        assert_eq!(c.execute(sql, &p).unwrap().affected, 1);
        c.execute("ROLLBACK", &p).unwrap();
        assert_eq!(
            c.execute("SELECT n,v FROM docs", &p).unwrap().rows,
            vec![vec![Value::Integer(1), Value::Integer(0)]]
        );
        let audit = c
            .check_collection_integrity("docs", Default::default())
            .unwrap();
        assert_eq!(audit.documents, 1);
        assert_eq!(audit.index_entries, 1);
    }
}

#[test]
fn joined_direct_parameters_consume_payload_budget() {
    {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let empty = Parameters::new();
        for sql in [
            "CREATE TABLE docs",
            "CREATE UNIQUE INDEX docs_n ON docs(n)",
            "INSERT INTO docs {id:docs:a,n:1}",
            "CREATE TABLE source(k INTEGER)",
            "INSERT INTO source VALUES(1),(1)",
            "BEGIN",
            "INSERT INTO docs {id:docs:b,n:2}",
        ] {
            c.execute(sql, &empty).unwrap();
        }
        let before = c
            .execute("SELECT * FROM docs ORDER BY n", &empty)
            .unwrap()
            .rows;
        let payload = Value::Binary(vec![255; 2048]);
        let params = Parameters::from([("$payload".into(), payload.clone())]);
        let sql =
            "UPDATE docs SET payload=$payload FROM source s WHERE docs.n=s.k RETURNING payload";
        let c = c.with_write_buffer_limits(ResultLimits {
            max_rows: 10,
            max_payload_bytes: 512,
        });
        assert_eq!(c.execute(sql, &params).unwrap_err().code(), "FDB_LIMIT");
        assert_eq!(c.transaction_state(), TransactionState::Active);
        assert_eq!(
            c.execute("SELECT * FROM docs ORDER BY n", &empty)
                .unwrap()
                .rows,
            before
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            2
        );
        let c = c.with_write_buffer_limits(ResultLimits {
            max_rows: 10,
            max_payload_bytes: 10000,
        });
        let result = c.execute(sql, &params).unwrap();
        assert_eq!(result.affected, 1);
        assert_eq!(result.rows, vec![vec![payload]]);
        c.execute("ROLLBACK", &empty).unwrap();
        assert_eq!(
            c.execute("SELECT n,payload FROM docs", &empty)
                .unwrap()
                .rows,
            vec![vec![Value::Integer(1), Value::Null]]
        );
    }
}

#[test]
fn ignored_candidates_do_not_hide_returning_limit_failure() {
    for from in ["", " FROM (SELECT 1 AS k) source"] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let p = Parameters::new();
        for sql in [
            "CREATE TABLE docs",
            "DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)",
            "CREATE INDEX docs_v ON docs(v)",
            "CREATE UNIQUE INDEX docs_n ON docs(n)",
            "INSERT INTO docs(n,v) VALUES(1,0),(2,0)",
            "BEGIN",
            "INSERT INTO docs(n,v) VALUES(3,0)",
        ] {
            c.execute(sql, &p).unwrap();
        }
        let before = c.execute("SELECT * FROM docs ORDER BY n", &p).unwrap().rows;
        let sql = format!(
            "UPDATE OR IGNORE docs SET v=CASE WHEN docs.n=2 THEN 10 ELSE 1 END{from} RETURNING n,v"
        );
        let error = c
            .write_with_result_limits(
                &sql,
                &p,
                ResultLimits {
                    max_rows: 1,
                    max_payload_bytes: 10000,
                },
            )
            .unwrap_err();
        assert_eq!(error.code(), "FDB_LIMIT");
        assert_eq!(c.transaction_state(), TransactionState::Active);
        assert_eq!(
            c.execute("SELECT * FROM docs ORDER BY n", &p).unwrap().rows,
            before
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            6
        );
        let result = c
            .write_with_result_limits(
                &sql,
                &p,
                ResultLimits {
                    max_rows: 2,
                    max_payload_bytes: 10000,
                },
            )
            .unwrap();
        assert_eq!(result.affected, 2);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(
            c.execute("SELECT n,v FROM docs ORDER BY n", &p)
                .unwrap()
                .rows,
            vec![
                vec![Value::Integer(1), Value::Integer(1)],
                vec![Value::Integer(2), Value::Integer(0)],
                vec![Value::Integer(3), Value::Integer(1)]
            ]
        );
        c.execute("ROLLBACK", &p).unwrap();
        assert_eq!(
            c.execute("SELECT n,v FROM docs ORDER BY n", &p)
                .unwrap()
                .rows,
            vec![
                vec![Value::Integer(1), Value::Integer(0)],
                vec![Value::Integer(2), Value::Integer(0)]
            ]
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            4
        );
    }
}

#[test]
fn replacement_deletions_are_restored_when_returning_exceeds_limits() {
    for from in ["", " FROM (SELECT 1 AS k) source"] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let p = Parameters::new();
        for sql in [
            "CREATE TABLE docs",
            "CREATE UNIQUE INDEX docs_n ON docs(n)",
            "CREATE INDEX docs_v ON docs(v)",
            "INSERT INTO docs(n,v) VALUES(1,0),(2,0),(3,0)",
            "BEGIN",
            "INSERT INTO docs(n,v) VALUES(4,0)",
        ] {
            c.execute(sql, &p).unwrap();
        }
        let before = c.execute("SELECT * FROM docs ORDER BY n", &p).unwrap().rows;
        let sql = format!("UPDATE OR REPLACE docs SET n=10,v=1{from} WHERE docs.n<4 RETURNING n,v");
        for limits in [
            ResultLimits {
                max_rows: 2,
                max_payload_bytes: 10000,
            },
            ResultLimits {
                max_rows: 10,
                max_payload_bytes: 1,
            },
        ] {
            assert_eq!(
                c.write_with_result_limits(&sql, &p, limits)
                    .unwrap_err()
                    .code(),
                "FDB_LIMIT"
            );
            assert_eq!(c.transaction_state(), TransactionState::Active);
            assert_eq!(
                c.execute("SELECT * FROM docs ORDER BY n", &p).unwrap().rows,
                before
            );
            assert_eq!(
                c.check_collection_integrity("docs", Default::default())
                    .unwrap()
                    .index_entries,
                8
            );
            assert!(c
                .lookup_index("docs", "docs_n", &Value::Integer(10))
                .unwrap()
                .is_empty());
        }
        let result = c
            .write_with_result_limits(
                &sql,
                &p,
                ResultLimits {
                    max_rows: 3,
                    max_payload_bytes: 10000,
                },
            )
            .unwrap();
        assert_eq!(result.affected, 3);
        assert_eq!(
            result.rows,
            vec![vec![Value::Integer(10), Value::Integer(1)]; 3]
        );
        assert_eq!(
            c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows,
            vec![vec![Value::Integer(4)], vec![Value::Integer(10)]]
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            4
        );
        c.execute("ROLLBACK", &p).unwrap();
        assert_eq!(
            c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows,
            vec![
                vec![Value::Integer(1)],
                vec![Value::Integer(2)],
                vec![Value::Integer(3)]
            ]
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            6
        );
    }
}

#[test]
fn ignored_insert_candidates_do_not_hide_result_limit_failures() {
    for source in [
        "VALUES(2,1),(1,2),(3,10),(5,3)",
        "SELECT 2,1 UNION ALL SELECT 1,2 UNION ALL SELECT 3,10 UNION ALL SELECT 5,3",
    ] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let p = Parameters::new();
        for sql in [
            "CREATE TABLE docs",
            "DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)",
            "CREATE INDEX docs_v ON docs(v)",
            "CREATE UNIQUE INDEX docs_n ON docs(n)",
            "INSERT INTO docs(n,v) VALUES(1,0)",
            "BEGIN",
            "INSERT INTO docs(n,v) VALUES(4,0)",
        ] {
            c.execute(sql, &p).unwrap();
        }
        let before = c.execute("SELECT * FROM docs ORDER BY n", &p).unwrap().rows;
        let sql = format!("INSERT OR IGNORE INTO docs(n,v) {source} RETURNING n,v");
        for limits in [
            ResultLimits {
                max_rows: 1,
                max_payload_bytes: 10000,
            },
            ResultLimits {
                max_rows: 10,
                max_payload_bytes: 1,
            },
        ] {
            assert_eq!(
                c.write_with_result_limits(&sql, &p, limits)
                    .unwrap_err()
                    .code(),
                "FDB_LIMIT"
            );
            assert_eq!(c.transaction_state(), TransactionState::Active);
            assert_eq!(
                c.execute("SELECT * FROM docs ORDER BY n", &p).unwrap().rows,
                before
            );
            assert_eq!(
                c.check_collection_integrity("docs", Default::default())
                    .unwrap()
                    .index_entries,
                4
            );
        }
        let result = c
            .write_with_result_limits(
                &sql,
                &p,
                ResultLimits {
                    max_rows: 2,
                    max_payload_bytes: 10000,
                },
            )
            .unwrap();
        assert_eq!(result.affected, 2);
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Integer(2), Value::Integer(1)],
                vec![Value::Integer(5), Value::Integer(3)]
            ]
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            8
        );
        c.execute("ROLLBACK", &p).unwrap();
        assert_eq!(
            c.execute("SELECT n,v FROM docs", &p).unwrap().rows,
            vec![vec![Value::Integer(1), Value::Integer(0)]]
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            2
        );
    }
}

#[test]
fn insert_replacement_victims_survive_result_limit_failures() {
    for source in [
        "VALUES(docs:a,2,30),(docs:d,5,50)",
        "SELECT docs:a,2,30 UNION ALL SELECT docs:d,5,50",
    ] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let p = Parameters::new();
        for sql in [
            "CREATE TABLE docs",
            "CREATE UNIQUE INDEX docs_n ON docs(n)",
            "CREATE UNIQUE INDEX docs_v ON docs(v)",
            "INSERT INTO docs(id,n,v) VALUES(docs:a,1,10),(docs:b,2,20),(docs:c,3,30)",
            "BEGIN",
            "INSERT INTO docs(id,n,v) VALUES(docs:pending,4,40)",
        ] {
            c.execute(sql, &p).unwrap();
        }
        let before = c.execute("SELECT * FROM docs ORDER BY n", &p).unwrap().rows;
        let sql = format!("INSERT OR REPLACE INTO docs(id,n,v) {source} RETURNING n,v");
        for limits in [
            ResultLimits {
                max_rows: 1,
                max_payload_bytes: 10000,
            },
            ResultLimits {
                max_rows: 10,
                max_payload_bytes: 1,
            },
        ] {
            assert_eq!(
                c.write_with_result_limits(&sql, &p, limits)
                    .unwrap_err()
                    .code(),
                "FDB_LIMIT"
            );
            assert_eq!(c.transaction_state(), TransactionState::Active);
            assert_eq!(
                c.execute("SELECT * FROM docs ORDER BY n", &p).unwrap().rows,
                before
            );
            assert_eq!(
                c.check_collection_integrity("docs", Default::default())
                    .unwrap()
                    .index_entries,
                8
            );
            for n in 1..=4 {
                assert_eq!(
                    c.lookup_index("docs", "docs_n", &Value::Integer(n))
                        .unwrap()
                        .len(),
                    1
                );
                assert_eq!(
                    c.lookup_index("docs", "docs_v", &Value::Integer(n * 10))
                        .unwrap()
                        .len(),
                    1
                );
            }
            assert!(c
                .lookup_index("docs", "docs_n", &Value::Integer(5))
                .unwrap()
                .is_empty());
        }
        let result = c
            .write_with_result_limits(
                &sql,
                &p,
                ResultLimits {
                    max_rows: 2,
                    max_payload_bytes: 10000,
                },
            )
            .unwrap();
        assert_eq!(result.affected, 2);
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Integer(2), Value::Integer(30)],
                vec![Value::Integer(5), Value::Integer(50)]
            ]
        );
        assert_eq!(
            c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows,
            vec![
                vec![Value::Integer(2)],
                vec![Value::Integer(4)],
                vec![Value::Integer(5)]
            ]
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            6
        );
        c.execute("ROLLBACK", &p).unwrap();
        assert_eq!(
            c.execute("SELECT * FROM docs ORDER BY n", &p).unwrap().rows,
            before[..3]
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            6
        );
    }
}
