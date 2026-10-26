use romedb::{Database, Key, Parameters, Record, ResultLimits, Value};

fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}
fn id(key: i64) -> Record {
    Record {
        table: "docs".into(),
        key: Key::Integer(key),
    }
}
fn setup(c: &romedb::Connection) {
    q(c, "CREATE TABLE docs");
    q(c, "BEGIN");
    for n in 0..64 {
        let params = Parameters::from([
            ("$id".into(), Value::Record(id(n))),
            ("$vector".into(), Value::vector32(&[n as f32, 0.0]).unwrap()),
        ]);
        c.execute("INSERT INTO docs {id:$id,body:'alpha',v:$vector}", &params)
            .unwrap();
    }
    q(c, "CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT");
    q(
        c,
        "CREATE SEARCH INDEX vectors ON docs(v) USING VECTOR WITH(metric='l2',dimensions=2)",
    );
    q(c, "COMMIT");
}
fn source(kind: &str, filter: &str) -> String {
    if kind == "text" {
        format!("search::text('words','alpha',1{filter})")
    } else {
        format!("search::vector('vectors',vector32('[0,0]'),1{filter})")
    }
}

#[test]
fn explicit_filters_select_before_top_k_and_keep_native_index_plans() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    setup(&c);
    for kind in ["text", "vector"] {
        let unfiltered = q(&c, &format!("SELECT id FROM {}", source(kind, "")));
        assert_eq!(unfiltered.rows, vec![vec![Value::Record(id(0))]]);
        assert!(q(
            &c,
            &format!(
                "SELECT id FROM {} WHERE id=type::record('docs',63)",
                source(kind, "")
            )
        )
        .rows
        .is_empty());
        let sql = format!(
            "SELECT id FROM {}",
            source(
                kind,
                ",array::new(type::record('docs',63),type::record('DOCS',63),docs:missing)"
            )
        );
        assert_eq!(
            q(&c, &sql).rows,
            vec![vec![Value::Record(id(63))]],
            "{kind}"
        );
        let plan = q(&c, &format!("EXPLAIN QUERY PLAN {sql}"));
        let expected = if kind == "text" {
            "QUERY INDEX METHOD fts"
        } else {
            "__romedb_ann_hnsw_hits"
        };
        assert!(
            plan.rows
                .iter()
                .flatten()
                .any(|value| matches!(value,Value::String(text) if text.contains(expected))),
            "{plan:?}"
        );
        assert!(q(
            &c,
            &format!("SELECT id FROM {}", source(kind, ",array::new()"))
        )
        .rows
        .is_empty());
        let params = Parameters::from([(
            "$ids".into(),
            Value::Array(vec![Value::Record(id(62)), Value::Record(id(63))]),
        )]);
        assert_eq!(
            c.execute(
                &format!("SELECT id FROM {}", source(kind, ",$ids")),
                &params
            )
            .unwrap()
            .rows,
            vec![vec![Value::Record(id(62))]]
        );
    }
    assert_eq!(
        c.search_vectors_filtered(
            "vectors",
            &Value::vector32(&[0.0, 0.0]).unwrap(),
            1,
            &[id(63)]
        )
        .unwrap()
        .rows,
        vec![vec![Value::Record(id(63)), Value::Number(63.0)]]
    );
    let p = Parameters::from([("$ids".into(), Value::Array(vec![Value::Record(id(63))]))]);
    let profile = c
        .profile_select(
            "SELECT id FROM search::vector('vectors',vector32('[0,0]'),1,$ids)",
            &p,
        )
        .unwrap();
    assert_eq!(profile.result.rows, vec![vec![Value::Record(id(63))]]);
}

#[test]
fn filter_validation_limits_and_failed_writes_preserve_prior_work() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    setup(&c);
    q(&c, "CREATE TABLE copies");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO copies {n:7}");
    for kind in ["text", "vector"] {
        let sql = format!(
            "INSERT INTO copies(ref) SELECT id FROM {}",
            source(kind, ",$ids")
        );
        for (value, code) in [
            (Value::Null, "FDB_VALIDATION"),
            (Value::Array(vec![Value::Null]), "FDB_VALIDATION"),
            (
                Value::Array(vec![Value::String("docs:63".into())]),
                "FDB_VALIDATION",
            ),
            (
                Value::Array(vec![Value::Record(Record {
                    table: "other".into(),
                    key: Key::Integer(63),
                })]),
                "FDB_VALIDATION",
            ),
            (Value::Array(vec![Value::Record(id(63)); 4097]), "FDB_LIMIT"),
            (
                Value::Array(vec![Value::Record(Record {
                    table: "docs".into(),
                    key: Key::String("x".repeat(1024 * 1024)),
                })]),
                "FDB_LIMIT",
            ),
        ] {
            let p = Parameters::from([("$ids".into(), value)]);
            assert_eq!(c.execute(&sql, &p).unwrap_err().code(), code, "{kind}");
            assert_eq!(
                q(&c, "SELECT n FROM copies").rows,
                vec![vec![Value::Integer(7)]]
            );
        }
        let p = Parameters::from([(
            "$ids".into(),
            Value::Array(vec![Value::Record(id(63)); 4096]),
        )]);
        assert_eq!(c.execute(&sql, &p).unwrap().affected, 1);
        q(&c, "DELETE FROM copies WHERE ref IS NOT NULL");
        let read = format!("SELECT id FROM {}", source(kind, ",$ids"));
        assert_eq!(
            c.select_with_limits(
                &read,
                &p,
                ResultLimits {
                    max_rows: 0,
                    max_payload_bytes: 1000
                }
            )
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    }
    let maximum = Parameters::from([(
        "$ids".into(),
        Value::Array((0..4096).map(|n| Value::Record(id(n))).collect()),
    )]);
    for kind in ["text", "vector"] {
        let started = std::time::Instant::now();
        assert_eq!(
            c.execute(
                &format!("SELECT id FROM {}", source(kind, ",$ids")),
                &maximum
            )
            .unwrap()
            .rows,
            vec![vec![Value::Record(id(0))]]
        );
        eprintln!(
            "{kind}: 4096 distinct allowed IDs, 64 indexed documents: {:?}",
            started.elapsed()
        );
    }
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT * FROM copies").rows.is_empty());
}

#[test]
fn filtered_queries_follow_pending_changes_reader_snapshots_rebuild_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("filters.db");
    let params = Parameters::from([("$ids".into(), Value::Array(vec![Value::Record(id(63))]))]);
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        setup(&c);
        let reader = db.connect().unwrap();
        q(&reader, "BEGIN");
        let text = "SELECT id FROM search::text('words','alpha',1,$ids)";
        let vector = "SELECT id,distance FROM search::vector('vectors',vector32('[0,0]'),1,$ids)";
        let before_text = reader.execute(text, &params).unwrap().rows;
        let before_vector = reader.execute(vector, &params).unwrap().rows;
        q(&c, "BEGIN");
        q(
            &c,
            "UPDATE docs SET body='beta',v=vector32('[1,0]') WHERE id=type::record('docs',63)",
        );
        assert!(c.execute(text, &params).unwrap().rows.is_empty());
        assert_eq!(
            c.execute(vector, &params).unwrap().rows[0][1],
            Value::Number(1.0)
        );
        q(&c, "REINDEX words");
        q(&c, "REINDEX vectors");
        q(&c, "ROLLBACK");
        assert_eq!(c.execute(text, &params).unwrap().rows, before_text);
        assert_eq!(c.execute(vector, &params).unwrap().rows, before_vector);
        q(&c, "DELETE FROM docs WHERE id=type::record('docs',63)");
        assert!(c.execute(vector, &params).unwrap().rows.is_empty());
        assert_eq!(reader.execute(text, &params).unwrap().rows, before_text);
        assert_eq!(reader.execute(vector, &params).unwrap().rows, before_vector);
        q(&reader, "COMMIT");
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    for kind in ["text", "vector"] {
        assert!(c
            .execute(
                &format!("SELECT id FROM {}", source(kind, ",$ids")),
                &params
            )
            .unwrap()
            .rows
            .is_empty());
    }
}

#[test]
fn filtered_reads_charge_work_cancel_and_preserve_typed_key_identity() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    setup(&c);
    for key in [
        romedb::Key::String("63".into()),
        romedb::Key::Integer(i64::MIN),
        romedb::Key::Integer(i64::MAX),
    ] {
        c.execute(
            "INSERT INTO docs {id:$id,body:'alpha',v:vector32('[0,0]')}",
            &Parameters::from([(
                "$id".into(),
                Value::Record(Record {
                    table: "docs".into(),
                    key,
                }),
            )]),
        )
        .unwrap();
    }
    for kind in ["text", "vector"] {
        for key in ["'63'", "-9223372036854775808", "9223372036854775807"] {
            let filter = format!(",array::new(type::record('docs',{key}))");
            let result = q(&c, &format!("SELECT id FROM {}", source(kind, &filter)));
            let expected = if key == "'63'" {
                Key::String("63".into())
            } else {
                Key::Integer(key.parse().unwrap())
            };
            assert_eq!(
                result.rows,
                vec![vec![Value::Record(Record {
                    table: "docs".into(),
                    key: expected
                })]]
            );
        }
    }
    q(&c, "BEGIN");
    q(&c, "INSERT INTO prior {n:7}");
    let limits = ResultLimits {
        max_rows: 10,
        max_payload_bytes: 4096,
    };
    let params = Parameters::from([("$ids".into(), Value::Array(vec![Value::Record(id(63))]))]);
    for kind in ["text", "vector"] {
        let sql = format!("SELECT id FROM {}", source(kind, ",$ids"));
        let token = romedb::CancellationToken::new();
        token.cancel();
        assert_eq!(
            c.execute_cancellable(&sql, &params, &token)
                .unwrap_err()
                .code(),
            "FDB_CANCELLED"
        );
        let full = c.select_metered(&sql, &params, limits, Default::default());
        assert_eq!(
            full.outcome.unwrap().rows,
            vec![vec![Value::Record(id(63))]]
        );
        if kind == "vector" {
            assert_eq!(full.work.rows_read, 4);
        }
        for cap in [0, full.work.rows_read - 1] {
            let partial = c.select_metered(
                &sql,
                &params,
                limits,
                romedb::ReadWorkLimits {
                    max_rows_read: Some(cap),
                    max_vm_steps: None,
                },
            );
            assert_eq!(partial.outcome.unwrap_err().code(), "FDB_CANCELLED");
            assert!(partial.work.read_budget_exhausted);
            assert_eq!(partial.work.rows_read, cap + 1);
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        }
        assert_eq!(
            q(&c, "SELECT n FROM prior").rows,
            vec![vec![Value::Integer(7)]]
        );
    }
    q(&c, "ROLLBACK");
}
