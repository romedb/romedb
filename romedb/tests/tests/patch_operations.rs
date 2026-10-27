use romedb::{Database, Document, Parameters, Value};

fn query(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

#[test]
fn patch_orders_operations_escapes_pointers_and_preserves_typed_values() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    query(
        &c,
        r#"INSERT INTO docs {id:docs:a,items:[1,2,3],nested:{"a/b":{"~key":null}},empty:{"":4}}"#,
    );
    let result = query(
        &c,
        "UPDATE docs:a PATCH [
        {op:'test',path:'/nested/a~1b/~0key',value:null},
        {op:'replace',path:'/nested/a~1b/~0key',value:7},
        {op:'add',path:'/items/1',value:9},
        {op:'remove',path:'/items/0'},
        {op:'copy',from:'/nested/a~1b',path:'/copied'},
        {op:'move',from:'/items/0',path:'/items/2'},
        {op:'add',path:'/items/-',value:8},
        {op:'test',path:'/empty/',value:4},
        {op:'replace',path:'/copied/~0key',value:5},
        {op:'test',path:'/nested/a~1b/~0key',value:7},
        {op:'move',from:'/items/1',path:'/items/1'},
        {op:'add',path:'/nested/a~1b/~0key',value:6,ignored:'extension'}
    ] RETURNING items,nested,copied",
    );
    assert_eq!(result.affected, 1);
    assert_eq!(
        result.rows[0][0],
        Value::Array(vec![
            Value::Integer(2),
            Value::Integer(3),
            Value::Integer(9),
            Value::Integer(8)
        ])
    );
    assert_eq!(
        result.rows[0][2],
        Value::Object(Document::from([("~key".into(), Value::Integer(5))]))
    );
    let value = Value::Object(Document::from([
        ("integer".into(), Value::Integer(i64::MAX)),
        ("binary".into(), Value::Binary(vec![0, 255])),
        ("vector".into(), Value::vector32(&[1.0, 2.0]).unwrap()),
        ("null".into(), Value::Null),
    ]));
    let params = Parameters::from([("$v".into(), value.clone())]);
    let result=c.execute("UPDATE docs PATCH [{op:'add',path:'/typed',value:$v},{op:'test',path:'/typed',value:$v},{op:'copy',from:'/typed',path:'/duplicate'}] WHERE id=docs:a RETURNING typed,duplicate", &params).unwrap();
    assert_eq!(result.rows, vec![vec![value.clone(), value]]);
    assert_eq!(
        query(&c, "UPDATE docs:missing PATCH [] RETURNING *").affected,
        0
    );
    assert_eq!(query(&c, "UPDATE docs:a PATCH [] RETURNING id").affected, 1);
}

#[test]
fn patch_rejects_invalid_operations_without_partial_changes() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    query(
        &c,
        "INSERT INTO docs {id:docs:a,items:[1,2],n:1,nested:{child:1},reference:docs:b}",
    );
    query(
        &c,
        "INSERT INTO docs {id:docs:b,items:[3,4],n:2,nested:{child:2}}",
    );
    query(&c, "CREATE UNIQUE INDEX docs_n ON docs(n)");
    query(&c, "BEGIN");
    query(&c, "INSERT INTO docs {id:docs:prior,n:3}");
    let original = query(&c, "SELECT * FROM docs ORDER BY id").rows;
    for operation in [
        "{op:'test',path:'/missing',value:null}",
        "{op:'test',path:'/n',value:2}",
        "{op:'test',path:'/n',value:1.0}",
        "{op:'add',path:'/id',value:docs:other}",
        "{op:'remove',path:'/id'}",
        "{op:'move',from:'/id',path:'/other'}",
        "{op:'replace',path:'/missing',value:1}",
        "{op:'add',path:'/missing/child',value:1}",
        "{op:'remove',path:'/items/-'}",
        "{op:'add',path:'/items/01',value:1}",
        "{op:'add',path:'/items/+1',value:1}",
        "{op:'add',path:'/items/3',value:1}",
        "{op:'replace',path:'/items/2',value:1}",
        "{op:'add',path:'/n/child',value:1}",
        "{op:'add',path:'/reference/n',value:1}",
        "{op:'move',from:'/nested',path:'/nested/child'}",
        "{op:'copy',from:'/absent',path:'/copy'}",
        "{op:'add',path:'',value:{}}",
        "{op:'add',path:'not-a-pointer',value:1}",
        "{op:'add',path:'/bad~2',value:1}",
        "{op:'add',path:'/bad~',value:1}",
        "{op:'add',path:'/v'}",
        "{op:'unknown',path:'/n'}",
        "{op:'add',path:2,value:1}",
        "null",
    ] {
        let sql =
            format!("UPDATE docs:a PATCH [{{op:'add',path:'/temporary',value:true}},{operation}]");
        assert!(
            matches!(
                c.execute(&sql, &Parameters::new()),
                Err(romedb::Error::Validation(_))
            ),
            "{sql}"
        );
        assert_eq!(
            query(&c, "SELECT * FROM docs ORDER BY id").rows,
            original,
            "{sql}"
        );
    }
    for sql in [
        "UPDATE docs PATCH [{op:'replace',path:'/n',value:9}]",
        "UPDATE docs PATCH [{op:'test',path:'/n',value:1},{op:'add',path:'/changed',value:true}] WHERE n<3",
        "UPDATE docs:a PATCH {}",
        "UPDATE docs:a PATCH [{op:'replace',path:'/n',value:7}] RETURNING record::fetch(id)",
    ] {
        assert!(c.execute(sql,&Parameters::new()).is_err(),"{sql}");
        assert_eq!(query(&c,"SELECT * FROM docs ORDER BY id").rows,original);
        c.check_collection_integrity("docs",Default::default()).unwrap();
        assert_eq!(c.transaction_state(),romedb::TransactionState::Active);
    }
    query(&c, "ROLLBACK");
    assert_eq!(query(&c, "SELECT * FROM docs").rows.len(), 2);
}

#[test]
fn patch_bounds_operation_count_pointer_depth_and_copy_growth() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    query(&c, "INSERT INTO docs {id:docs:a,n:1}");
    let op = Value::Object(Document::from([
        ("op".into(), Value::String("test".into())),
        ("path".into(), Value::String("/n".into())),
        ("value".into(), Value::Integer(1)),
    ]));
    let parameters = Parameters::from([("$ops".into(), Value::Array(vec![op; 1025]))]);
    assert!(matches!(
        c.execute("UPDATE docs:a PATCH $ops", &parameters),
        Err(romedb::Error::Limit(_))
    ));
    for path in ["/x".repeat(65), format!("/{}", "x".repeat(16384))] {
        let p = Parameters::from([("$path".into(), Value::String(path))]);
        assert!(matches!(
            c.execute("UPDATE docs:a PATCH [{op:'add',path:$path,value:1}]", &p),
            Err(romedb::Error::Limit(_))
        ));
    }
    let params = Parameters::from([("$large".into(), Value::String("x".repeat(1024 * 1024)))]);
    let ops = "{op:'copy',from:'/large',path:'/copy'},".repeat(24);
    let sql=format!("UPDATE docs:a PATCH [{{op:'add',path:'/large',value:$large}},{ops}{{op:'test',path:'/n',value:1}}]");
    assert!(matches!(
        c.execute(&sql, &params),
        Err(romedb::Error::Limit(_))
    ));
    assert_eq!(
        query(&c, "SELECT * FROM docs").rows[0][0],
        Value::Object(Document::from([
            (
                "id".into(),
                Value::Record(romedb::Record {
                    table: "docs".into(),
                    key: romedb::Key::String("a".into())
                })
            ),
            ("n".into(), Value::Integer(1))
        ]))
    );
}

#[test]
fn patch_snapshots_returning_limits_cancellation_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("patch.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        let reader = db.connect().unwrap();
        query(&c, "INSERT INTO docs {id:docs:a,n:1,items:[1,2]}");
        query(&c, "CREATE INDEX docs_n ON docs(n)");
        query(&reader, "BEGIN");
        let before = query(&reader, "SELECT * FROM docs").rows;
        let sql="UPDATE docs:a PATCH [{op:'replace',path:'/n',value:2},{op:'add',path:'/items/-',value:3}] RETURNING *";
        assert!(c
            .write_with_result_limits(
                sql,
                &Parameters::new(),
                romedb::ResultLimits {
                    max_rows: 0,
                    max_payload_bytes: 100
                }
            )
            .is_err());
        assert_eq!(query(&c, "SELECT * FROM docs").rows, before);
        let token = romedb::CancellationToken::new();
        token.cancel();
        assert_eq!(
            c.execute_cancellable(sql, &Parameters::new(), &token)
                .unwrap_err()
                .code(),
            "FDB_CANCELLED"
        );
        query(&c, sql);
        assert_eq!(query(&reader, "SELECT * FROM docs").rows, before);
        query(&reader, "COMMIT");
        assert_eq!(
            query(&reader, "SELECT items FROM docs WHERE n=2").rows,
            vec![vec![Value::Array(vec![
                Value::Integer(1),
                Value::Integer(2),
                Value::Integer(3)
            ])]]
        );
    }
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(query(&c, "SELECT id FROM docs WHERE n=2").rows.len(), 1);
    assert!(query(&c, "SELECT id FROM docs WHERE n=1").rows.is_empty());
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}
