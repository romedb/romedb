use romedb::{Database, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}
fn id(n: i64) -> Value {
    Value::Record(romedb::Record {
        table: "docs".into(),
        key: romedb::Key::Integer(n),
    })
}

#[test]
fn compound_predicates_use_native_prefix_keys_and_keep_sql_scalar_meanings() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "BEGIN");
    for n in 0..1000 {
        c.execute(
            "INSERT INTO docs {id:$id,a:$a,b:$b}",
            &Parameters::from([
                ("$id".into(), id(n)),
                ("$a".into(), Value::Integer(n / 100)),
                ("$b".into(), Value::Integer(n % 100)),
            ]),
        )
        .unwrap();
    }
    q(&c, "CREATE UNIQUE INDEX pair ON docs(a,b)");
    q(&c, "COMMIT");
    let sql = "SELECT id FROM docs WHERE b=$b AND a=$a";
    let params = Parameters::from([
        ("$a".into(), Value::Integer(5)),
        ("$b".into(), Value::Integer(7)),
    ]);
    let result = c.select_metered(
        sql,
        &params,
        romedb::ResultLimits {
            max_rows: 10,
            max_payload_bytes: 4096,
        },
        Default::default(),
    );
    assert_eq!(result.outcome.unwrap().rows, vec![vec![id(507)]]);
    assert!(result.work.rows_read < 10, "{:?}", result.work);
    assert_eq!(
        c.execute("SELECT id FROM docs WHERE ((b))=$b AND ((a))=$a", &params)
            .unwrap()
            .rows,
        vec![vec![id(507)]]
    );
    let plan = c
        .execute(&format!("EXPLAIN QUERY PLAN {sql}"), &params)
        .unwrap();
    assert!(plan.rows.iter().flatten().any(|v|matches!(v,Value::String(plan) if plan.contains("SEARCH")&&plan.contains("pair")&&plan.contains("key=?")&&plan.contains("f1=?"))),"{plan:?}");
    assert_eq!(
        q(&c, "SELECT count(*) FROM docs WHERE a=5").rows,
        vec![vec![Value::Integer(100)]]
    );
    assert_eq!(
        q(&c, "SELECT count(*) FROM docs WHERE b=7").rows,
        vec![vec![Value::Integer(10)]]
    );
    assert_eq!(
        c.lookup_compound_index("docs", "pair", &[Value::Integer(5), Value::Integer(7)])
            .unwrap()[0]["id"],
        id(507)
    );
    assert!(c.lookup_index("docs", "pair", &Value::Integer(5)).is_err());
    assert!(c
        .lookup_compound_index("docs", "pair", &[Value::Integer(5)])
        .is_err());
    assert_eq!(
        c.execute("INSERT INTO docs {a:5.0,b:7}", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_CONSTRAINT"
    );
    for _ in 0..2 {
        q(&c, "INSERT INTO docs {a:5,b:null}");
        q(&c, "INSERT INTO docs {a:5}");
    }
    q(&c, "INSERT INTO docs {a:'5',b:7}");
    assert_eq!(
        q(&c, "SELECT count(*) FROM docs WHERE a='5' AND b=7").rows,
        vec![vec![Value::Integer(1)]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn compound_constraints_replacements_computed_values_and_rebuild_are_atomic() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD source ON docs TYPE integer DEFAULT 1",
        "DEFINE FIELD b ON docs TYPE integer VALUE(source+1)",
        "CREATE UNIQUE INDEX pair ON docs(a,b)",
        "CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT",
        "CREATE SEARCH INDEX vec ON docs(v) USING VECTOR WITH(dimensions=2,metric='l2')",
    ] {
        q(&c, sql);
    }
    q(
        &c,
        "INSERT INTO docs {id:docs:a,a:1,body:'old',v:vector32('[1,0]')}",
    );
    q(
        &c,
        "INSERT INTO docs {id:docs:b,a:2,source:2,body:'other',v:vector32('[0,1]')}",
    );
    q(&c, "BEGIN");
    q(&c, "INSERT INTO prior {n:7}");
    assert_eq!(
        c.execute("UPDATE docs SET a=1,source=1", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_CONSTRAINT"
    );
    assert_eq!(
        q(&c, "SELECT a,b FROM docs ORDER BY a").rows,
        vec![
            vec![Value::Integer(1), Value::Integer(2)],
            vec![Value::Integer(2), Value::Integer(3)]
        ]
    );
    q(&c,"INSERT OR REPLACE INTO docs(id,a,source,body,v) VALUES(docs:c,1,1,'new',vector32('[2,0]'))");
    assert!(q(&c, "SELECT id FROM docs WHERE id=docs:a").rows.is_empty());
    assert_eq!(
        q(&c, "SELECT id FROM search::text('words','new',10)").rows,
        vec![vec![Value::Record(romedb::Record {
            table: "docs".into(),
            key: romedb::Key::String("c".into())
        })]]
    );
    q(&c, "REINDEX pair");
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(2)]]
    );
    assert!(!q(&c, "SELECT id FROM docs WHERE id=docs:a").rows.is_empty());
    assert!(c
        .execute("DEFINE FIELD a ON docs TYPE object", &Parameters::new())
        .is_err());
    assert!(c
        .execute("DEFINE FIELD b ON docs TYPE object", &Parameters::new())
        .is_err());
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn compound_nested_typed_paths_snapshots_and_reopen_preserve_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("compound.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let writer = db.connect().unwrap();
        writer
            .execute(
                "INSERT INTO docs {id:docs:a,nested:{ref:refs:a},bytes:$bytes}",
                &Parameters::from([("$bytes".into(), Value::Binary(vec![0, 255]))]),
            )
            .unwrap();
        q(
            &writer,
            "CREATE UNIQUE INDEX pair ON docs(nested.ref,bytes)",
        );
        writer
            .execute(
                "INSERT INTO docs {id:docs:b,nested:{ref:refs:a},bytes:$bytes}",
                &Parameters::from([("$bytes".into(), Value::Binary(vec![1, 255]))]),
            )
            .unwrap();
        let reader = db.connect().unwrap();
        q(&reader, "BEGIN");
        let sql = "SELECT id FROM docs WHERE docs.nested.ref=refs:a AND bytes=x'00ff'";
        let before = q(&reader, sql).rows;
        assert_eq!(before.len(), 1);
        writer
            .execute(
                "UPDATE docs:a {bytes:$bytes}",
                &Parameters::from([("$bytes".into(), Value::Binary(vec![2, 255]))]),
            )
            .unwrap();
        assert!(q(&writer, sql).rows.is_empty());
        assert_eq!(q(&reader, sql).rows, before);
        q(&reader, "COMMIT");
        q(&writer, "BEGIN");
        q(&writer, "REINDEX pair");
        q(&writer, "DELETE FROM docs:b");
        q(&writer, "ROLLBACK");
        writer
            .check_collection_integrity("docs", Default::default())
            .unwrap();
    }
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(
        q(&c, "SELECT count(*) FROM docs WHERE docs.nested.ref=refs:a").rows,
        vec![vec![Value::Integer(2)]]
    );
    let info = q(&c, "INFO FOR INDEX pair");
    assert!(format!("{info:?}").contains("paths"));
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn compound_definition_bounds_failed_builds_and_relational_indexes_remain_explicit() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "INSERT INTO docs {a:1,b:2}");
    q(&c, "INSERT INTO docs {a:1,b:2}");
    assert_eq!(
        c.execute("CREATE UNIQUE INDEX pair ON docs(a,b)", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_CONSTRAINT"
    );
    q(&c, "CREATE INDEX pair ON docs(a,b)");
    for fields in [
        "a,a".into(),
        (0..17)
            .map(|i| format!("f{i}"))
            .collect::<Vec<_>>()
            .join(","),
    ] {
        assert_eq!(
            c.execute(
                &format!("CREATE INDEX invalid ON docs({fields})"),
                &Parameters::new()
            )
            .unwrap_err()
            .code(),
            "FDB_VALIDATION"
        );
    }
    let fields = (0..16)
        .map(|i| format!("f{i}"))
        .collect::<Vec<_>>()
        .join(",");
    q(&c, &format!("CREATE INDEX maximum ON docs({fields})"));
    q(&c, "CREATE TABLE relational(a,b)");
    q(&c, "CREATE UNIQUE INDEX native_pair ON relational(a,b)");
    q(&c, "INSERT INTO relational VALUES(1,2)");
    assert_eq!(
        c.execute("INSERT INTO relational VALUES(1,2)", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_CONSTRAINT"
    );
    q(&c, "CREATE TABLE schema_docs");
    q(&c, "CREATE INDEX schema_pair ON schema_docs(a,b)");
    assert_eq!(
        c.execute(
            "DEFINE FIELD b ON schema_docs TYPE object",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_VALIDATION"
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}
