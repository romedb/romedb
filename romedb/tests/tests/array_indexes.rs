use romedb::{Database, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}
fn id(key: &str) -> Value {
    Value::Record(romedb::Record {
        table: "docs".into(),
        key: romedb::Key::String(key.into()),
    })
}

#[test]
fn array_membership_uses_indexed_typed_keys_without_duplicate_documents() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let needles = vec![
        Value::Null,
        Value::Boolean(true),
        Value::Integer(1),
        Value::Number(1.0),
        Value::String("1".into()),
        Value::Binary(vec![1, 0, 255]),
        Value::Record(romedb::Record {
            table: "REFS".into(),
            key: romedb::Key::Integer(1),
        }),
        Value::Number(-0.0),
    ];
    for (i, needle) in needles.iter().enumerate() {
        c.execute(
            "INSERT INTO docs {id:$id,tags:$tags}",
            &Parameters::from([
                ("$id".into(), id(&format!("v{i}"))),
                (
                    "$tags".into(),
                    Value::Array(vec![needle.clone(), needle.clone()]),
                ),
            ]),
        )
        .unwrap();
    }
    for sql in [
        "INSERT INTO docs {id:docs:missing}",
        "INSERT INTO docs {id:docs:null,tags:null}",
        "INSERT INTO docs {id:docs:empty,tags:[]}",
        "CREATE SEARCH INDEX tags ON docs(tags) USING ARRAY",
    ] {
        q(&c, sql);
    }
    let sql = "SELECT id FROM docs WHERE array::contains(tags,$needle) ORDER BY id";
    for (i, needle) in needles.iter().enumerate() {
        let params = Parameters::from([("$needle".into(), needle.clone())]);
        let indexed = c.execute(sql, &params).unwrap();
        let scanned = c
            .execute(
                &sql.replace("FROM docs WHERE", "FROM docs NOT INDEXED WHERE"),
                &params,
            )
            .unwrap();
        assert_eq!(indexed.rows, vec![vec![id(&format!("v{i}"))]]);
        assert_eq!(indexed.rows, scanned.rows);
        let plan = c
            .execute(&format!("EXPLAIN QUERY PLAN {sql}"), &params)
            .unwrap();
        assert!(plan.rows.iter().flatten().any(|value|matches!(value,Value::String(plan) if plan.contains("SEARCH")&&plan.contains("tags")&&plan.contains("key=?"))),"{plan:?}");
    }
    assert_eq!(
        q(&c, "SELECT id FROM docs WHERE array::contains(tags,true)").rows,
        vec![vec![id("v2")]]
    );
    assert_eq!(
        q(&c, "SELECT id FROM docs WHERE array::contains(tags,refs:1)").rows,
        vec![vec![id("v6")]]
    );
    assert!(q(
        &c,
        "SELECT id FROM docs WHERE array::contains(tags,type::record('refs','1'))"
    )
    .rows
    .is_empty());
    assert_eq!(
        c.execute(
            sql,
            &Parameters::from([("$needle".into(), Value::Number(0.0))])
        )
        .unwrap()
        .rows,
        vec![vec![id("v7")]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT id FROM docs NOT INDEXED WHERE array::contains(tags,true)"
        )
        .rows,
        vec![vec![id("v2")]]
    );
    let needle = Value::Record(romedb::Record {
        table: "refs".into(),
        key: romedb::Key::Integer(1),
    });
    assert_eq!(
        c.execute(sql, &Parameters::from([("$needle".into(), needle)]))
            .unwrap()
            .rows,
        vec![vec![id("v6")]]
    );
    let report = c
        .check_collection_integrity("docs", Default::default())
        .unwrap();
    assert_eq!(report.documents, 11);
    assert_eq!(report.index_entries, 8);
    let info = q(&c, "INFO FOR INDEX tags");
    assert!(format!("{info:?}").contains("array"));
    let composite = Value::Object(romedb::Document::from([(
        "ref".into(),
        Value::Record(romedb::Record {
            table: "REFS".into(),
            key: romedb::Key::Integer(1),
        }),
    )]));
    let params = Parameters::from([
        ("$array".into(), Value::Array(vec![composite.clone()])),
        ("$needle".into(), composite),
    ]);
    assert_eq!(
        c.execute("SELECT array::contains($array,$needle)", &params)
            .unwrap()
            .rows,
        vec![vec![Value::Boolean(true)]]
    );
}

#[test]
fn array_entry_limits_validation_and_late_writes_preserve_transactions() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(
        &c,
        "INSERT INTO docs {id:docs:a,tags:['old'],n:1,body:'hello',v:vector32('[1,0]')}",
    );
    for sql in [
        "CREATE SEARCH INDEX tags ON docs(tags) USING ARRAY",
        "CREATE UNIQUE INDEX pair ON docs(n,body)",
        "CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT",
        "CREATE SEARCH INDEX vec ON docs(v) USING VECTOR WITH(dimensions=2,metric='l2')",
    ] {
        q(&c, sql);
    }
    q(&c, "BEGIN");
    q(&c, "INSERT INTO prior {n:7}");
    let cases = vec![
        (Value::Integer(7), "FDB_VALIDATION"),
        (Value::Array(vec![Value::Array(vec![])]), "FDB_VALIDATION"),
        (
            Value::Array(vec![Value::Object(Default::default())]),
            "FDB_VALIDATION",
        ),
        (
            Value::Array(vec![Value::vector32(&[1.0]).unwrap()]),
            "FDB_VALIDATION",
        ),
        (Value::Array(vec![Value::Integer(1); 4097]), "FDB_LIMIT"),
        (
            Value::Array(vec![Value::String("x".repeat(1024)); 4096]),
            "FDB_LIMIT",
        ),
    ];
    for (bad, code) in cases {
        let params = Parameters::from([
            (
                "$good".into(),
                Value::Array(vec![Value::String("new".into())]),
            ),
            ("$bad".into(), bad),
        ]);
        assert_eq!(c.execute("INSERT INTO docs(id,tags,n,body) VALUES(docs:b,$good,2,'second'),(docs:c,$bad,3,'third')",&params).unwrap_err().code(),code);
        assert_eq!(
            q(&c, "SELECT count(*) FROM docs").rows,
            vec![vec![Value::Integer(1)]]
        );
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
    c.execute(
        "INSERT INTO docs {id:docs:maximum,tags:$tags}",
        &Parameters::from([("$tags".into(), Value::Array(vec![Value::Integer(1); 4096]))]),
    )
    .unwrap();
    let expanded = Parameters::from([
        (
            "$id".into(),
            Value::Record(romedb::Record {
                table: "docs".into(),
                key: romedb::Key::String("x".repeat(8192)),
            }),
        ),
        (
            "$tags".into(),
            Value::Array((0..256).map(Value::Integer).collect()),
        ),
    ]);
    assert_eq!(
        c.execute("INSERT INTO docs {id:$id,tags:$tags}", &expanded)
            .unwrap_err()
            .code(),
        "FDB_LIMIT"
    );
    q(
        &c,
        "UPDATE docs:a PATCH [{op:'replace',path:'/tags/0',value:'patched'}]",
    );
    assert!(
        q(&c, "SELECT id FROM docs WHERE array::contains(tags,'old')")
            .rows
            .is_empty()
    );
    assert_eq!(
        q(
            &c,
            "SELECT id FROM docs WHERE array::contains(tags,'patched')"
        )
        .rows,
        vec![vec![id("a")]]
    );
    q(&c, "REINDEX tags");
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT id FROM docs WHERE array::contains(tags,'old')").rows,
        vec![vec![id("a")]]
    );
    q(&c, "CREATE TABLE schema_docs");
    q(&c, "DEFINE FIELD tags ON schema_docs TYPE array<object>");
    assert_eq!(
        c.execute(
            "CREATE SEARCH INDEX bad ON schema_docs(tags) USING ARRAY",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_VALIDATION"
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn array_indexes_keep_reader_snapshots_pending_writes_rebuild_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("array.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "INSERT INTO docs {id:docs:a,tags:['a','a','b']}");
        q(&c, "CREATE SEARCH INDEX tags ON docs(tags) USING ARRAY");
        q(
            &c,
            "CREATE SEARCH INDEX IF NOT EXISTS tags ON docs(tags) USING ARRAY",
        );
        let reader = db.connect().unwrap();
        q(&reader, "BEGIN");
        let sql = "SELECT id FROM docs WHERE array::contains(tags,'a')";
        assert_eq!(q(&reader, sql).rows, vec![vec![id("a")]]);
        q(&c, "BEGIN");
        q(&c, "UPDATE docs:a MERGE {tags:['c','c']}");
        assert!(q(&c, sql).rows.is_empty());
        q(&c, "REINDEX tags");
        q(&c, "ROLLBACK");
        assert_eq!(q(&c, sql).rows, vec![vec![id("a")]]);
        q(&c, "UPDATE docs:a CONTENT {tags:['d']}");
        assert!(q(&c, sql).rows.is_empty());
        assert_eq!(q(&reader, sql).rows, vec![vec![id("a")]]);
        q(&reader, "COMMIT");
        q(&c, "REINDEX tags");
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(
        q(&c, "SELECT id FROM docs WHERE array::contains(tags,'d')").rows,
        vec![vec![id("a")]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "DELETE FROM docs:a");
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .index_entries,
        0
    );
    q(&c, "DROP INDEX tags");
    q(&c, "DROP TABLE docs");
}

#[test]
fn indexed_membership_limits_and_cancellation_preserve_prior_work() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "INSERT INTO docs {id:docs:a,tags:['x']}");
    q(&c, "CREATE SEARCH INDEX tags ON docs(tags) USING ARRAY");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO prior {n:7}");
    let sql = "SELECT id FROM docs WHERE array::contains(tags,$needle)";
    let params = Parameters::from([("$needle".into(), Value::String("x".into()))]);
    let limits = romedb::ResultLimits {
        max_rows: 10,
        max_payload_bytes: 4096,
    };
    let full = c.select_metered(sql, &params, limits, Default::default());
    assert_eq!(full.outcome.unwrap().rows, vec![vec![id("a")]]);
    assert!(full.work.rows_read < 10);
    let partial = c.select_metered(
        sql,
        &params,
        limits,
        romedb::ReadWorkLimits {
            max_rows_read: Some(0),
            max_vm_steps: None,
        },
    );
    assert_eq!(partial.outcome.unwrap_err().code(), "FDB_CANCELLED");
    assert!(partial.work.read_budget_exhausted);
    let token = romedb::CancellationToken::new();
    token.cancel();
    assert_eq!(
        c.execute_cancellable(sql, &params, &token)
            .unwrap_err()
            .code(),
        "FDB_CANCELLED"
    );
    assert_eq!(
        c.select_with_limits(
            sql,
            &params,
            romedb::ResultLimits {
                max_rows: 0,
                ..limits
            }
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    for sql in [
        sql.to_owned(),
        sql.replace("FROM docs WHERE", "FROM docs NOT INDEXED WHERE"),
    ] {
        assert_eq!(
            c.execute(
                &sql,
                &Parameters::from([("$needle".into(), Value::String("x".repeat(1024 * 1024)))])
            )
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
    }
    assert_eq!(
        q(&c, "SELECT n FROM prior").rows,
        vec![vec![Value::Integer(7)]]
    );
    q(&c, "COMMIT");
}

#[test]
fn compound_and_array_indexes_recover_commits_and_discard_partial_work_after_exit() {
    const ENV: &str = "ROMEDB_ARRAY_COMPOUND_CRASH_PATH";
    if let Ok(path) = std::env::var(ENV) {
        let c = Database::open(&path).unwrap().connect().unwrap();
        q(&c, "INSERT INTO docs {id:docs:a,n:1,state:1,tags:['old']}");
        q(
            &c,
            "INSERT INTO docs {id:docs:b,n:2,state:1,tags:['b','c']}",
        );
        q(&c, "CREATE SEARCH INDEX tags ON docs(tags) USING ARRAY");
        q(&c, "CREATE UNIQUE INDEX pair ON docs(n,state)");
        q(&c, "BEGIN");
        q(&c, "INSERT INTO docs(n,state,tags) VALUES(1,1,array::new('committed')) ON CONFLICT(n,state) DO UPDATE SET state=2,tags=excluded.tags");
        q(&c, "REINDEX tags");
        q(&c, "REINDEX pair");
        q(&c, "COMMIT");
        q(&c, "BEGIN");
        q(&c, "INSERT INTO docs(n,state,tags) VALUES(1,2,array::new('uncommitted')) ON CONFLICT(n,state) DO UPDATE SET state=3,tags=excluded.tags");
        q(&c, "DELETE FROM docs:b");
        assert_eq!(
            q(
                &c,
                "SELECT id FROM docs WHERE array::contains(tags,'uncommitted')"
            )
            .rows,
            vec![vec![id("a")]]
        );
        std::process::exit(73);
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crash.db");
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "compound_and_array_indexes_recover_commits_and_discard_partial_work_after_exit",
        ])
        .env(ENV, &path)
        .output()
        .unwrap();
    assert_eq!(
        child.status.code(),
        Some(73),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(
        q(
            &c,
            "SELECT id FROM docs WHERE array::contains(tags,'committed')"
        )
        .rows,
        vec![vec![id("a")]]
    );
    assert!(q(
        &c,
        "SELECT id FROM docs WHERE array::contains(tags,'uncommitted')"
    )
    .rows
    .is_empty());
    assert_eq!(
        q(&c, "SELECT id FROM docs WHERE n=1 AND state=2").rows,
        vec![vec![id("a")]]
    );
    let report = c
        .check_collection_integrity("docs", Default::default())
        .unwrap();
    assert_eq!(report.documents, 2);
    assert_eq!(report.index_entries, 5);
}

#[test]
fn array_membership_seeks_bound_query_work_on_a_large_collection() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "BEGIN");
    for n in 0..1000 {
        c.execute(
            "INSERT INTO docs {n:$n,tags:array::new($n,$next,$n)}",
            &Parameters::from([
                ("$n".into(), Value::Integer(n)),
                ("$next".into(), Value::Integer(n + 1)),
            ]),
        )
        .unwrap();
    }
    q(&c, "CREATE SEARCH INDEX tags ON docs(tags) USING ARRAY");
    q(&c, "COMMIT");
    let limits = romedb::ResultLimits {
        max_rows: 10,
        max_payload_bytes: 4096,
    };
    let indexed = c.select_metered(
        "SELECT n FROM docs WHERE array::contains(tags,507) ORDER BY n",
        &Parameters::new(),
        limits,
        Default::default(),
    );
    let scanned = c.select_metered(
        "SELECT n FROM docs NOT INDEXED WHERE array::contains(tags,507) ORDER BY n",
        &Parameters::new(),
        limits,
        Default::default(),
    );
    assert_eq!(indexed.outcome.unwrap().rows, scanned.outcome.unwrap().rows);
    assert!(indexed.work.rows_read < 10, "{:?}", indexed.work);
    assert!(scanned.work.rows_read >= 1000, "{:?}", scanned.work);
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .index_entries,
        2000
    );
}
