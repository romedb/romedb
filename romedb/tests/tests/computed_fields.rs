use romedb::{Database, Document, Parameters, Value};

fn query(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

#[test]
fn stored_calculations_follow_dependencies_defaults_and_every_mutation_path() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    for sql in [
        "CREATE TABLE orders",
        "DEFINE FIELD doubled ON orders TYPE number VALUE (total*2)",
        "DEFINE FIELD subtotal ON orders TYPE number REQUIRED DEFAULT 10",
        "DEFINE FIELD tax ON orders TYPE number REQUIRED DEFAULT 2",
        "DEFINE FIELD total ON orders TYPE number REQUIRED VALUE (subtotal+tax) CHECK(total>=0)",
        "CREATE INDEX orders_total ON orders(total)",
        "INSERT INTO orders {id:orders:a,total:999,doubled:999}",
    ] {
        query(&c, sql);
    }
    assert_eq!(
        query(&c, "SELECT total,doubled FROM orders").rows,
        vec![vec![Value::Integer(12), Value::Integer(24)]]
    );
    for (sql,total) in [
        ("UPDATE orders:a {subtotal:11} RETURNING total,doubled",13),
        ("UPDATE orders SET tax=3 WHERE id=orders:a RETURNING total,doubled",14),
        ("UPDATE orders:a MERGE {subtotal:12} RETURNING total,doubled",15),
        ("UPDATE orders:a CONTENT {subtotal:13,tax:3} RETURNING total,doubled",16),
        ("UPDATE orders:a PATCH [{op:'replace',path:'/tax',value:4}] RETURNING total,doubled",17),
        ("UPSERT orders:a {subtotal:14} RETURNING total,doubled",18),
        ("UPDATE orders:a UNSET total RETURNING total,doubled",18),
        ("INSERT OR REPLACE INTO orders(id,subtotal,tax) VALUES(orders:a,15,4) RETURNING total,doubled",19),
    ] {
        assert_eq!(query(&c,sql).rows,vec![vec![Value::Integer(total),Value::Integer(total*2)]],"{sql}");
        assert_eq!(c.lookup_index("orders","orders_total",&Value::Integer(total)).unwrap().len(),1);
    }
    let record = romedb::Record {
        table: "orders".into(),
        key: romedb::Key::String("a".into()),
    };
    let updated = c
        .patch(
            &record,
            Document::from([("subtotal".into(), Value::Integer(16))]),
        )
        .unwrap()
        .unwrap();
    assert_eq!(updated["total"], Value::Integer(20));
    for sql in [
        "INSERT INTO orders(id) VALUES(orders:b)",
        "INSERT INTO orders(id) SELECT type::record('orders','c')",
        "UPSERT orders:d {}",
    ] {
        query(&c, sql);
    }
    assert_eq!(
        query(&c, "SELECT total FROM orders ORDER BY id").rows,
        vec![
            vec![Value::Integer(20)],
            vec![Value::Integer(12)],
            vec![Value::Integer(12)],
            vec![Value::Integer(12)]
        ]
    );
    c.check_collection_integrity("orders", Default::default())
        .unwrap();
}

#[test]
fn calculation_definitions_reject_cycles_unsafe_functions_and_incompatible_existing_values() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(
        &c,
        "DEFINE FIELD a ON docs TYPE integer NULLABLE VALUE (b+1)",
    );
    for sql in [
        "DEFINE FIELD b ON docs TYPE integer VALUE (a+1)",
        "DEFINE FIELD self_ref ON docs TYPE integer VALUE (self_ref+1)",
        "DEFINE FIELD profile.n ON docs TYPE integer VALUE (doc::get(profile,'$.x'))",
        "DEFINE FIELD rand ON docs TYPE integer VALUE (random())",
        "DEFINE FIELD clock ON docs TYPE string VALUE (datetime('now'))",
        "DEFINE FIELD ref ON docs TYPE object VALUE (record::fetch(id))",
        "DEFINE FIELD p ON docs TYPE integer VALUE ($parameter)",
        "DEFINE FIELD bad ON docs TYPE integer DEFAULT 7 VALUE (1)",
        "DEFINE FIELD bad ON docs TYPE integer READONLY VALUE (1)",
        "DEFINE FIELD bad ON docs TYPE string VALUE (upper())",
    ] {
        assert_eq!(
            c.execute(sql, &Parameters::new()).expect_err(sql).code(),
            "FDB_VALIDATION",
            "{sql}"
        );
    }
    query(
        &c,
        "DEFINE FIELD obj ON docs TYPE object FLEXIBLE VALUE ({n:1})",
    );
    assert!(c
        .execute(
            "DEFINE FIELD obj.n ON docs TYPE integer VALUE (1)",
            &Parameters::new()
        )
        .is_err());
    query(&c, "INSERT INTO other {id:other:a,n:3,doubled:6}");
    query(&c, "DEFINE FIELD doubled ON other TYPE integer VALUE (n*2)");
    assert!(c
        .execute(
            "DEFINE FIELD OVERWRITE doubled ON other TYPE integer VALUE (n*3)",
            &Parameters::new()
        )
        .is_err());
    assert!(c
        .execute(
            "DEFINE FIELD extra ON other TYPE integer VALUE (1)",
            &Parameters::new()
        )
        .is_err());
    query(&c, "UPDATE other:a {n:4,doubled:999}");
    assert_eq!(
        query(&c, "SELECT doubled FROM other").rows,
        vec![vec![Value::Integer(8)]]
    );
    let Value::Object(info) = &query(&c, "INFO FOR TABLE other").rows[0][0] else {
        panic!()
    };
    let Value::Array(fields) = &info["fields"] else {
        panic!()
    };
    assert!(fields.iter().any(|field|matches!(field,Value::Object(field) if field["computed"]==Value::String("n*2".into()))));
    query(&c, "REMOVE FIELD doubled ON other");
    query(&c, "UPDATE other:a {n:5}");
    assert_eq!(
        query(&c, "SELECT doubled FROM other").rows,
        vec![vec![Value::Integer(8)]]
    );
}

#[test]
fn typed_results_nested_parents_checks_and_array_rules_apply_after_calculation() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD source ON docs TYPE string DEFAULT 'Alice'",
        "DEFINE FIELD ref ON docs TYPE record<refs> VALUE (type::record('refs',source))",
        "DEFINE FIELD active ON docs TYPE boolean VALUE (CASE WHEN length(source)>0 THEN true ELSE false END)",
        "DEFINE FIELD xs ON docs TYPE array<integer> VALUE (array::new(length(source),7))",
        "DEFINE FIELD profile.slug ON docs TYPE string VALUE (lower(source))",
        "DEFINE FIELD v ON docs TYPE vector<2> VALUE (vector32('[1,2]'))",
        "DEFINE SCHEMA ON docs STRICT",
        "INSERT INTO docs {id:docs:a,profile:{}}",
        "INSERT INTO docs {id:docs:b}",
    ] {query(&c,sql);}
    assert_eq!(
        query(&c, "SELECT active,xs,profile FROM docs ORDER BY id").rows,
        vec![
            vec![
                Value::Boolean(true),
                Value::Array(vec![Value::Integer(5), Value::Integer(7)]),
                Value::Object(Document::from([(
                    "slug".into(),
                    Value::String("alice".into())
                )]))
            ],
            vec![
                Value::Boolean(true),
                Value::Array(vec![Value::Integer(5), Value::Integer(7)]),
                Value::Null
            ],
        ]
    );
    let before = query(&c, "SELECT * FROM docs ORDER BY id").rows;
    assert!(c
        .execute("UPDATE docs:a {profile:1}", &Parameters::new())
        .is_err());
    assert_eq!(query(&c, "SELECT * FROM docs ORDER BY id").rows, before);
    query(&c, "CREATE TABLE bad");
    query(
        &c,
        "DEFINE FIELD xs ON bad TYPE array<integer> VALUE (array::new(1,'wrong'))",
    );
    assert!(c.execute("INSERT INTO bad {}", &Parameters::new()).is_err());
    assert_eq!(
        query(&c, "SELECT count(*) FROM bad").rows,
        vec![vec![Value::Integer(0)]]
    );
}

#[test]
fn computed_index_values_are_atomic_with_snapshots_import_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("computed.db");
    let probes=[
        "SELECT id FROM docs WHERE uid=1",
        "SELECT id FROM docs WHERE author=writers:one ORDER BY id",
        "SELECT id,score FROM search::text('docs_text','old',10) ORDER BY id",
        "SELECT id,distance FROM search::vector('docs_vec',vector32('[1,0]'),2) ORDER BY distance,id",
        "SELECT id FROM search::near('docs_geo',geo::point(0,0),100) ORDER BY id",
    ];
    {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE docs",
            "DEFINE FIELD uid ON docs TYPE integer VALUE (n)",
            "DEFINE FIELD title ON docs TYPE string VALUE (lower(title_input))",
            "DEFINE FIELD author ON docs TYPE record<writers> VALUE (type::record('writers',author_key))",
            "DEFINE FIELD v ON docs TYPE vector<2> VALUE (vector32(v_input))",
            "DEFINE FIELD location ON docs TYPE object FLEXIBLE VALUE (geo::point(coord,coord))",
            "CREATE UNIQUE INDEX docs_uid ON docs(uid)",
            "CREATE INDEX docs_author ON docs(author)",
            "CREATE SEARCH INDEX docs_text ON docs(title) USING FULLTEXT",
            "CREATE SEARCH INDEX docs_vec ON docs(v) USING VECTOR WITH (metric='l2',dimensions=2)",
            "CREATE SEARCH INDEX docs_geo ON docs(location) USING SPATIAL",
            "INSERT INTO docs {id:docs:a,n:1,title_input:'OLD alpha',author_key:'one',v_input:'[1,0]',coord:0}",
            "INSERT INTO docs {id:docs:b,n:2,title_input:'OLD beta',author_key:'two',v_input:'[0,1]',coord:2}",
            "CREATE TABLE prior(n INTEGER)","BEGIN","INSERT INTO prior VALUES(9)",
        ] {query(&c,sql);}
        let before = query(&c, "SELECT * FROM docs ORDER BY id").rows;
        let expected = probes.map(|sql| query(&c, sql).rows);
        assert_eq!(
            c.execute("UPDATE docs SET n=1", &Parameters::new())
                .unwrap_err()
                .code(),
            "FDB_CONSTRAINT"
        );
        let report = c.write_metered(
            "UPDATE docs SET title_input='changed'",
            &Parameters::new(),
            romedb::ResultLimits {
                max_rows: 100,
                max_payload_bytes: 65536,
            },
            romedb::WriteWorkLimits {
                max_row_mutations: Some(1),
                ..Default::default()
            },
        );
        assert!(report.outcome.is_err());
        assert!(report.work.mutation_budget_exhausted);
        assert_eq!(report.work.row_mutations, 2);
        assert_eq!(query(&c, "SELECT * FROM docs ORDER BY id").rows, before);
        for (sql, expected) in probes.iter().zip(&expected) {
            assert_eq!(&query(&c, sql).rows, expected);
        }
        assert_eq!(
            query(&c, "SELECT n FROM prior").rows,
            vec![vec![Value::Integer(9)]]
        );
        query(&c, "COMMIT");
        let reader = db.connect().unwrap();
        query(&reader, "BEGIN");
        assert_eq!(
            query(&reader, "SELECT * FROM docs ORDER BY id").rows,
            before
        );
        query(&c,"UPDATE docs:a MERGE {n:3,title_input:'NEW',author_key:'changed',v_input:'[0,1]',coord:1}");
        for (sql, expected) in probes.iter().zip(&expected) {
            assert_eq!(&query(&reader, sql).rows, expected);
        }
        query(&reader, "COMMIT");
        assert!(query(&c, probes[0]).rows.is_empty());
        assert!(query(&c, probes[1]).rows.is_empty());
        assert!(query(&c, probes[4]).rows.is_empty());
        assert_eq!(
            query(&c, "SELECT id FROM search::text('docs_text','new',10)")
                .rows
                .len(),
            1
        );
        let source = Database::open(":memory:").unwrap().connect().unwrap();
        query(&source,"INSERT INTO docs {id:docs:imported,n:4,title_input:'IMPORTED',author_key:'one',v_input:'[1,0]',coord:0}");
        let payload = source
            .export_documents("docs", romedb::TransferFormat::Json)
            .unwrap();
        assert_eq!(
            c.import_documents("docs", &payload, romedb::TransferFormat::Json)
                .unwrap(),
            1
        );
        assert_eq!(
            query(&c, "SELECT uid,title FROM docs WHERE id=docs:imported").rows,
            vec![vec![Value::Integer(4), Value::String("imported".into())]]
        );
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
    let c = Database::open(file.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    query(&c, "UPDATE docs:a {title_input:'REOPENED'}");
    assert_eq!(
        query(&c, "SELECT id FROM search::text('docs_text','reopened',10)")
            .rows
            .len(),
        1
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn calculation_limits_cancellation_and_write_buffers_preserve_prior_work() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    query(&c, "CREATE TABLE docs");
    for (expression, code) in [
        (format!("'{}'", "x".repeat(16384)), "FDB_LIMIT"),
        (
            format!("array::new({})", vec!["1"; 257].join(",")),
            "FDB_LIMIT",
        ),
    ] {
        assert_eq!(
            c.execute(
                &format!("DEFINE FIELD invalid ON docs TYPE string VALUE ({expression})"),
                &Parameters::new()
            )
            .unwrap_err()
            .code(),
            code
        );
    }
    for sql in [
        "DEFINE FIELD n ON docs TYPE integer DEFAULT 1",
        "DEFINE FIELD value ON docs TYPE integer VALUE (n+1)",
        "CREATE UNIQUE INDEX docs_value ON docs(value)",
        "BEGIN",
        "INSERT INTO docs {id:docs:prior}",
    ] {
        query(&c, sql);
    }
    let before = query(&c, "SELECT * FROM docs").rows;
    assert!(c
        .write_with_result_limits(
            "INSERT INTO docs {id:docs:a,n:2} RETURNING *",
            &Parameters::new(),
            romedb::ResultLimits {
                max_rows: 0,
                max_payload_bytes: 65536
            }
        )
        .is_err());
    let token = romedb::CancellationToken::new();
    token.cancel();
    assert_eq!(
        c.execute_cancellable("UPDATE docs SET n=n+1", &Parameters::new(), &token)
            .unwrap_err()
            .code(),
        "FDB_CANCELLED"
    );
    assert_eq!(query(&c, "SELECT * FROM docs").rows, before);
    query(&c, "COMMIT");
    query(&c, "CREATE TABLE expansion");
    query(
        &c,
        "DEFINE FIELD result ON expansion TYPE string VALUE (replace(source,'x',replacement))",
    );
    let error = c
        .execute(
            "INSERT INTO expansion {source:$source,replacement:$replacement}",
            &Parameters::from([
                ("$source".into(), Value::String("x".repeat(65536))),
                ("$replacement".into(), Value::String("y".repeat(65536))),
            ]),
        )
        .unwrap_err();
    assert_eq!(error.code(), "FDB_LIMIT");
    assert!(error.to_string().contains("computed replace output"));
    assert_eq!(
        query(&c, "SELECT count(*) FROM expansion").rows,
        vec![vec![Value::Integer(0)]]
    );
    query(&c, "INSERT INTO expansion {source:'xx',replacement:'abc'}");
    assert_eq!(
        query(&c, "SELECT result FROM expansion").rows,
        vec![vec![Value::String("abcabc".into())]]
    );
    query(&c, "CREATE TABLE buffered");
    query(
        &c,
        "DEFINE FIELD payload ON buffered TYPE string VALUE (replace(source,'x',replacement))",
    );
    query(
        &c,
        "INSERT INTO buffered {id:buffered:a,source:'x',replacement:'a'}",
    );
    let limited = Database::open(":memory:")
        .unwrap()
        .connect()
        .unwrap()
        .with_write_buffer_limits(romedb::ResultLimits {
            max_rows: 10,
            max_payload_bytes: 256,
        });
    query(&limited, "CREATE TABLE docs");
    query(
        &limited,
        "DEFINE FIELD output ON docs TYPE string VALUE (replace(source,'x',replacement))",
    );
    assert!(limited
        .execute(
            "INSERT INTO docs {source:$s,replacement:$r}",
            &Parameters::from([
                ("$s".into(), Value::String("x".repeat(20))),
                ("$r".into(), Value::String("y".repeat(20)))
            ])
        )
        .is_err());
    assert_eq!(
        query(&limited, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(0)]]
    );
}

#[test]
fn computed_metadata_and_integrity_reject_corruption_while_reads_use_stored_values() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("computed-metadata.db");
    let path = file.to_str().unwrap();
    let c = Database::open(path).unwrap().connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD n ON docs TYPE integer",
        "DEFINE FIELD total ON docs TYPE integer VALUE (n+1)",
        "INSERT INTO docs {id:docs:a,n:1}",
    ] {
        query(&c, sql);
    }
    let engine = turso_core::Database::open_file(
        turso_core::Database::io_for_path(path).unwrap(),
        path,
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap();
    let raw = engine.connect().unwrap();
    raw.execute("CREATE TABLE metadata_backup AS SELECT metadata FROM __romedb_catalog")
        .unwrap();
    for expression in [
        "json_set(metadata,'$.version',4)",
        "json_set(metadata,'$.field_policies[0].options.computed','total+1')",
        "json_set(metadata,'$.field_policies[0].options.computed','random()')",
        "json_set(metadata,'$.field_policies[0].options.readonly',json('true'))",
        "json_set(metadata,'$.field_policies[0].options.computed','$parameter')",
    ] {
        raw.execute(format!("UPDATE __romedb_catalog SET metadata={expression}"))
            .unwrap();
        assert_eq!(
            c.execute("INFO FOR TABLE docs", &Parameters::new())
                .expect_err(expression)
                .code(),
            "FDB_STORAGE"
        );
        raw.execute("UPDATE __romedb_catalog SET metadata=(SELECT metadata FROM metadata_backup)")
            .unwrap();
    }
    raw.execute("UPDATE __romedb_c_646f6373 SET doc=CAST('FDB'||char(1)||json_set(CAST(substr(doc,5) AS TEXT),'$.value.total.value',999) AS BLOB)").unwrap();
    assert_eq!(
        query(&c, "SELECT total FROM docs").rows,
        vec![vec![Value::Integer(999)]]
    );
    assert!(c
        .check_collection_integrity("docs", Default::default())
        .is_err());
    query(&c, "UPDATE docs:a {n:2}");
    assert_eq!(
        query(&c, "SELECT total FROM docs").rows,
        vec![vec![Value::Integer(3)]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn computed_budgets_are_stable_for_rewrites_and_bound_definition_counts() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    query(&c, "CREATE TABLE big");
    query(
        &c,
        "DEFINE FIELD payload ON big TYPE string VALUE (replace(source,'x',replacement))",
    );
    c.execute(
        "INSERT INTO big {id:big:a,source:$s,replacement:$r}",
        &Parameters::from([
            ("$s".into(), Value::String("x".repeat(8192))),
            ("$r".into(), Value::String("y".repeat(4096))),
        ]),
    )
    .unwrap();
    query(&c, "UPDATE big:a {other:1}");
    assert_eq!(
        query(&c, "SELECT length(payload) FROM big").rows,
        vec![vec![Value::Integer(32 * 1024 * 1024)]]
    );
    c.check_collection_integrity("big", Default::default())
        .unwrap();
    query(&c, "CREATE TABLE many");
    for n in 0..128 {
        query(
            &c,
            &format!("DEFINE FIELD f{n} ON many TYPE integer VALUE (1)"),
        );
    }
    assert_eq!(
        c.execute(
            "DEFINE FIELD too_many ON many TYPE integer VALUE (1)",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    query(&c, "INSERT INTO many {id:many:a}");
    assert_eq!(
        query(&c, "SELECT f127 FROM many").rows,
        vec![vec![Value::Integer(1)]]
    );
}
