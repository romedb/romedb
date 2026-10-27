use romedb::{Database, Document, Key, Parameters, Record, Value};

fn query(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn record(table: &str, key: &str) -> Value {
    Value::Record(Record {
        table: table.into(),
        key: Key::String(key.into()),
    })
}

#[test]
fn fetch_expands_only_named_members_and_preserves_types_and_shape() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    query(
        &c,
        "INSERT INTO writers {id:writers:w1,name:'Alice',manager:writers:w2}",
    );
    query(
        &c,
        "INSERT INTO writers {id:writers:w2,name:'Bob',manager:writers:w1}",
    );
    let typed = Value::Object(Document::from([
        ("integer".into(), Value::Integer(i64::MAX)),
        ("binary".into(), Value::Binary(vec![0, 255])),
        ("vector".into(), Value::vector32(&[1.0, 2.0]).unwrap()),
        ("bool".into(), Value::Boolean(false)),
    ]));
    c.execute(
        "UPDATE writers:w1 MERGE {typed:$typed}",
        &Parameters::from([("$typed".into(), typed)]),
    )
    .unwrap();
    query(
        &c,
        r#"INSERT INTO articles {id:articles:a,author:writers:w1,editors:[writers:w1,null,writers:missing,7],metadata:{editor:writers:w2},comments:[{by:writers:w1,flag:true},{absent:true}],"odd.key":writers:w2,nullable:null}"#,
    );
    let source = c
        .get(&Record {
            table: "articles".into(),
            key: Key::String("a".into()),
        })
        .unwrap()
        .unwrap();
    let writer = c
        .get(&Record {
            table: "writers".into(),
            key: Key::String("w1".into()),
        })
        .unwrap()
        .unwrap();
    let manager = c
        .get(&Record {
            table: "writers".into(),
            key: Key::String("w2".into()),
        })
        .unwrap()
        .unwrap();
    let mut expected = source.clone();
    let mut author = writer.clone();
    author.insert("manager".into(), Value::Object(manager.clone()));
    expected.insert("author".into(), Value::Object(author));
    expected.insert(
        "editors".into(),
        Value::Array(vec![
            Value::Object(writer.clone()),
            Value::Null,
            Value::Null,
            Value::Integer(7),
        ]),
    );
    expected.insert(
        "metadata".into(),
        Value::Object(Document::from([(
            "editor".into(),
            Value::Object(manager.clone()),
        )])),
    );
    expected.insert(
        "comments".into(),
        Value::Array(vec![
            Value::Object(Document::from([
                ("by".into(), Value::Object(writer)),
                ("flag".into(), Value::Boolean(true)),
            ])),
            Value::Object(Document::from([("absent".into(), Value::Boolean(true))])),
        ]),
    );
    expected.insert("odd.key".into(), Value::Object(manager));
    for paths in [
        r#"author,author.manager,editors,metadata.editor,comments.by,"odd.key",absent,nullable"#,
        r#"nullable,absent,"odd.key",comments.by,metadata.editor,editors,author.manager,author,author"#,
    ] {
        let sql = format!("SELECT * FROM articles FETCH {paths}");
        let result = c.profile_select(&sql, &Parameters::new()).unwrap();
        assert_eq!(result.result.columns, vec!["document"]);
        assert_eq!(
            result.result.rows,
            vec![vec![Value::Object(expected.clone())]]
        );
        assert_eq!(result.metrics.fetch_batches, 1);
    }
    assert_eq!(
        query(&c, "SELECT * FROM articles").rows,
        vec![vec![Value::Object(source)]]
    );
    let cycle = query(&c, "SELECT * FROM articles FETCH author.manager.manager");
    let Value::Object(doc) = &cycle.rows[0][0] else {
        panic!()
    };
    let Value::Object(first) = &doc["author"] else {
        panic!()
    };
    let Value::Object(second) = &first["manager"] else {
        panic!()
    };
    let Value::Object(third) = &second["manager"] else {
        panic!()
    };
    assert_eq!(third["manager"], record("writers", "w2"));
}

#[test]
fn fetch_keeps_sql_names_and_rejects_ambiguous_composition() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    for sql in [
        "INSERT INTO docs {id:docs:a,fetch:1}",
        "CREATE TABLE native(fetch INTEGER)",
        "INSERT INTO native VALUES(1)",
    ] {
        query(&c, sql);
    }
    for sql in [
        "SELECT * FROM docs fetch WHERE fetch.fetch=1",
        "SELECT * FROM docs AS fetch ORDER BY fetch.id LIMIT 1",
        "SELECT fetch FROM docs",
        "SELECT * FROM docs WHERE fetch=1",
        "SELECT * FROM docs ORDER BY fetch",
    ] {
        assert_eq!(query(&c, sql).rows.len(), 1, "{sql}");
    }
    for sql in [
        "SELECT fetch FROM docs FETCH author",
        "SELECT d.* FROM docs d FETCH author",
        "SELECT * FROM docs a JOIN docs b ON a.id=b.id FETCH author",
        "SELECT * FROM native FETCH fetch",
        "SELECT * FROM (SELECT * FROM docs) FETCH author",
        "SELECT * OMIT fetch FROM docs FETCH author",
        "SELECT * FROM docs FETCH author LIMIT 1",
        "UPDATE docs:a MERGE {fetch:2} RETURNING * FETCH author",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert_eq!(
        query(&c, "SELECT fetch FROM docs").rows,
        vec![vec![Value::Integer(1)]]
    );
    let path = vec!["author"; 65].join(".");
    assert!(c
        .execute(
            &format!("SELECT * FROM docs FETCH {path}"),
            &Parameters::new()
        )
        .is_err());
    let paths = vec!["author"; 1025].join(",");
    assert!(c
        .execute(
            &format!("SELECT * FROM docs FETCH {paths}"),
            &Parameters::new()
        )
        .is_err());
}

#[test]
fn fetch_batches_duplicates_after_index_filter_and_pagination() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    query(&c, "INSERT INTO writers {id:writers:w1,name:'Alice'}");
    query(&c, "BEGIN");
    for n in 0..100 {
        query(
            &c,
            &format!(
                "INSERT INTO docs {{id:type::record('docs',{n}),group_no:{},author:writers:w1}}",
                n % 10
            ),
        );
    }
    query(&c, "COMMIT");
    let sql = "SELECT * FROM docs WHERE group_no=1 ORDER BY id LIMIT 3 OFFSET 1 FETCH author";
    let scan = c.profile_select(sql, &Parameters::new()).unwrap();
    query(&c, "CREATE INDEX docs_group ON docs(group_no)");
    let index = c.profile_select(sql, &Parameters::new()).unwrap();
    assert_eq!(scan.result.rows, index.result.rows);
    assert_eq!(index.result.rows.len(), 3);
    assert!(index.metrics.rows_read < scan.metrics.rows_read);
    assert!(index.metrics.fullscan_steps < scan.metrics.fullscan_steps);
    assert_eq!(index.metrics.fetch_batches, 1);
    let plan = query(&c, &format!("EXPLAIN QUERY PLAN {sql}"));
    assert!(format!("{:?}", plan.rows).contains("docs_group"));
    let single = c
        .profile_select(
            "SELECT * FROM docs LIMIT 1 FETCH author",
            &Parameters::new(),
        )
        .unwrap();
    assert_eq!(
        single.metrics.fetch_rows_read,
        index.metrics.fetch_rows_read
    );
}

#[test]
fn fetch_bounds_references_traversal_bytes_and_expanded_value_depth() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    query(&c, "CREATE TABLE targets");
    query(&c, "INSERT INTO docs {id:docs:a}");
    let cases = [
        Value::Array(
            (0..16385)
                .map(|i| {
                    Value::Record(Record {
                        table: "targets".into(),
                        key: Key::Integer(i),
                    })
                })
                .collect(),
        ),
        Value::Array(vec![Value::Null; 500_001]),
    ];
    for value in cases {
        c.execute(
            "UPDATE docs:a CONTENT {items:$items}",
            &Parameters::from([("$items".into(), value)]),
        )
        .unwrap();
        let error = c
            .execute("SELECT * FROM docs FETCH items", &Parameters::new())
            .unwrap_err();
        assert_eq!(error.code(), "FDB_LIMIT", "{error}");
        assert_eq!(query(&c, "SELECT id FROM docs").rows.len(), 1);
    }
    c.execute(
        "INSERT INTO targets {id:targets:large,value:$value}",
        &Parameters::from([("$value".into(), Value::String("x".repeat(1024 * 1024)))]),
    )
    .unwrap();
    c.execute(
        "UPDATE docs:a CONTENT {items:$items}",
        &Parameters::from([(
            "$items".into(),
            Value::Array(vec![record("targets", "large"); 70]),
        )]),
    )
    .unwrap();
    assert_eq!(
        c.execute("SELECT * FROM docs FETCH items", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_LIMIT"
    );
    let mut nested = Value::Integer(1);
    for _ in 0..60 {
        nested = Value::Object(Document::from([("child".into(), nested)]));
    }
    c.execute(
        "INSERT INTO targets {id:targets:deep,next:targets:deep,value:$value}",
        &Parameters::from([("$value".into(), nested)]),
    )
    .unwrap();
    query(&c, "UPDATE docs:a CONTENT {author:targets:deep}");
    assert_eq!(
        c.execute(
            "SELECT * FROM docs FETCH author.next.next.next.next",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn fetch_keeps_snapshots_pending_writes_limits_cancellation_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("fetch.db");
    let sql = "SELECT * FROM docs FETCH author";
    {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let reader = db.connect().unwrap();
        let writer = db.connect().unwrap();
        query(&writer, "INSERT INTO writers {id:writers:w1,name:'Old'}");
        query(&writer, "INSERT INTO docs {id:docs:a,author:writers:w1}");
        query(&reader, "BEGIN");
        let old = query(&reader, sql).rows;
        query(&writer, "UPDATE writers:w1 MERGE {name:'New'}");
        assert_eq!(query(&reader, sql).rows, old);
        query(&reader, "COMMIT");
        assert_ne!(query(&reader, sql).rows, old);
        query(&reader, "BEGIN");
        query(&reader, "UPDATE writers:w1 MERGE {name:'Pending'}");
        let pending = query(&reader, sql).rows;
        assert!(reader
            .select_with_limits(
                sql,
                &Parameters::new(),
                romedb::ResultLimits {
                    max_rows: 1,
                    max_payload_bytes: 1
                }
            )
            .is_err());
        let token = romedb::CancellationToken::new();
        token.cancel();
        assert_eq!(
            reader
                .execute_cancellable(sql, &Parameters::new(), &token)
                .unwrap_err()
                .code(),
            "FDB_CANCELLED"
        );
        assert_eq!(query(&reader, sql).rows, pending);
        query(&reader, "ROLLBACK");
    }
    let c = Database::open(file.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    let Value::Object(doc) = &query(&c, sql).rows[0][0] else {
        panic!()
    };
    let Value::Object(author) = &doc["author"] else {
        panic!()
    };
    assert_eq!(author["name"], Value::String("New".into()));
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}
