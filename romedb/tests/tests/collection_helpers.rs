use romedb::{Database, Document, Key, Parameters, Record, Value};

#[test]
fn helpers_preserve_types_order_and_missing_members_in_reads_and_writes() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let object = Value::Object(Document::from([
        ("z".into(), Value::Binary(vec![0, 255])),
        (
            "a".into(),
            Value::Record(Record {
                table: "docs".into(),
                key: Key::Integer(1),
            }),
        ),
        ("integer".into(), Value::Integer(i64::MAX)),
        ("nullable".into(), Value::Null),
        ("vector".into(), Value::vector32(&[1.0, 2.0]).unwrap()),
    ]));
    let values = Value::Array(vec![
        object.clone(),
        Value::Null,
        object.clone(),
        Value::Null,
        Value::Integer(1),
        Value::Number(1.0),
        Value::Number(-0.0),
        Value::Number(0.0),
        Value::Record(Record {
            table: "DOCS".into(),
            key: Key::Integer(1),
        }),
        Value::Record(Record {
            table: "docs".into(),
            key: Key::Integer(1),
        }),
        Value::Record(Record {
            table: "docs".into(),
            key: Key::String("1".into()),
        }),
    ]);
    let nested = Value::Array(vec![
        Value::Array(vec![
            Value::Integer(1),
            Value::Array(vec![Value::Integer(2)]),
        ]),
        Value::Null,
        Value::Array(vec![]),
        object.clone(),
    ]);
    let p = Parameters::from([
        ("$object".into(), object.clone()),
        ("$values".into(), values.clone()),
        ("$nested".into(), nested),
    ]);
    c.execute(
        "INSERT INTO docs {id:docs:a,object:$object,items:$values,nested:$nested}",
        &p,
    )
    .unwrap();
    let expressions = [
        "array::distinct(items)",
        "array::flatten(nested)",
        "array::len(items)",
        "doc::keys(object)",
        "doc::values(object)",
        "doc::entries(object)",
        "doc::from_entries(doc::entries(object))",
    ];
    let expected = [
        Value::Array(vec![
            object.clone(),
            Value::Null,
            Value::Integer(1),
            Value::Number(1.0),
            Value::Number(-0.0),
            Value::Record(Record {
                table: "DOCS".into(),
                key: Key::Integer(1),
            }),
            Value::Record(Record {
                table: "docs".into(),
                key: Key::String("1".into()),
            }),
        ]),
        Value::Array(vec![
            Value::Integer(1),
            Value::Array(vec![Value::Integer(2)]),
            Value::Null,
            object.clone(),
        ]),
        Value::Integer(11),
        Value::Array(
            ["a", "integer", "nullable", "vector", "z"]
                .map(|key| Value::String(key.into()))
                .to_vec(),
        ),
        {
            let Value::Object(doc) = &object else {
                panic!()
            };
            Value::Array(doc.values().cloned().collect())
        },
        {
            let Value::Object(doc) = &object else {
                panic!()
            };
            Value::Array(
                doc.iter()
                    .map(|(key, value)| {
                        Value::Array(vec![Value::String(key.clone()), value.clone()])
                    })
                    .collect(),
            )
        },
        object.clone(),
    ];
    for (expression, expected) in expressions.iter().zip(expected) {
        let sql = format!("SELECT {expression} AS result FROM docs");
        let read = c
            .execute(&sql, &Parameters::new())
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
        assert_eq!(read.columns, vec!["result"]);
        assert_eq!(read.rows, vec![vec![expected.clone()]], "{expression}");
        for sql in [
            format!("UPDATE docs:a MERGE {{result:{expression}}} RETURNING result"),
            format!("UPDATE docs SET result={expression} RETURNING result"),
        ] {
            assert_eq!(
                c.execute(&sql, &Parameters::new()).unwrap().rows,
                vec![vec![expected.clone()]],
                "{sql}"
            );
        }
    }
    assert_eq!(c.execute("SELECT doc::has(object,'$.nullable'),doc::has(object,'$.absent'),doc::get(object,'$.nullable'),doc::get(object,'$.absent') FROM docs",&Parameters::new()).unwrap().rows,vec![vec![Value::Boolean(true),Value::Boolean(false),Value::Null,Value::Null]]);
    assert_eq!(c.execute("SELECT array::len(array::new()),array::distinct(array::new()),array::flatten(array::new()),doc::from_entries(array::new())",&Parameters::new()).unwrap().rows,vec![vec![Value::Integer(0),Value::Array(vec![]),Value::Array(vec![]),Value::Object(Document::new())]]);
}

#[test]
fn helper_errors_and_limits_leave_documents_unchanged() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    c.execute("INSERT INTO docs {id:docs:a,n:1}", &Parameters::new())
        .unwrap();
    for expression in [
        "array::distinct(null)",
        "array::flatten(1)",
        "array::len('abc')",
        "doc::keys(null)",
        "doc::entries(array::new())",
        "doc::values(4)",
        "doc::from_entries(array::new(1))",
        "doc::from_entries(array::new(array::new(1,2)))",
        "doc::from_entries(array::new(array::new('a',1),array::new('a',2)))",
        "array::distinct()",
        "doc::keys(1,2)",
    ] {
        for sql in [
            format!("SELECT {expression} FROM docs"),
            format!("UPDATE docs MERGE {{n:2,value:{expression}}}"),
        ] {
            assert!(c.execute(&sql, &Parameters::new()).is_err(), "{sql}");
        }
    }
    let params = Parameters::from([("$values".into(), Value::Array(vec![Value::Null; 100_001]))]);
    for expression in [
        "array::distinct($values)",
        "array::flatten($values)",
        "doc::from_entries($values)",
    ] {
        assert!(c
            .execute(
                &format!("UPDATE docs MERGE {{n:2,result:{expression}}}"),
                &params
            )
            .is_err());
    }
    assert_eq!(
        c.execute("SELECT n FROM docs", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::Integer(1)]]
    );
    let params = Parameters::from([("$values".into(), Value::Array(vec![Value::Null; 100_000]))]);
    assert_eq!(
        c.execute("SELECT array::distinct($values)", &params)
            .unwrap()
            .rows,
        vec![vec![Value::Array(vec![Value::Null])]]
    );
}

#[test]
fn helper_predicates_preserve_index_filters_limits_and_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("helpers.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "INSERT INTO docs {id:docs:a,n:1,items:[1,1]}",
            "INSERT INTO docs {id:docs:b,n:2,items:[2,2]}",
            "CREATE INDEX docs_n ON docs(n)",
            "BEGIN",
        ] {
            c.execute(sql, &Parameters::new()).unwrap();
        }
        let result = c
            .execute(
                "SELECT id FROM docs WHERE n=1 AND array::len(array::distinct(items))=1",
                &Parameters::new(),
            )
            .unwrap();
        assert_eq!(result.rows.len(), 1);
        c.execute(
            "UPDATE docs SET items=array::distinct(items)",
            &Parameters::new(),
        )
        .unwrap();
        c.execute("ROLLBACK", &Parameters::new()).unwrap();
        assert_eq!(
            c.execute(
                "SELECT array::len(items) FROM docs ORDER BY n",
                &Parameters::new()
            )
            .unwrap()
            .rows,
            vec![vec![Value::Integer(2)], vec![Value::Integer(2)]]
        );
        c.execute(
            "UPDATE docs:a MERGE {items:array::distinct(items)}",
            &Parameters::new(),
        )
        .unwrap();
    }
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(
        c.execute("SELECT items FROM docs WHERE n=1", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::Array(vec![Value::Integer(1)])]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn helper_queries_use_scalar_index_and_obey_snapshots_and_result_limits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("helper-query.db");
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let writer = db.connect().unwrap();
    let reader = db.connect().unwrap();
    let p = Parameters::new();
    writer.execute("BEGIN", &p).unwrap();
    for n in 0..100 {
        writer
            .execute(
                &format!("INSERT INTO docs {{id:type::record('docs',{n}),n:{n},items:[1,1,2]}}"),
                &p,
            )
            .unwrap();
    }
    writer.execute("COMMIT", &p).unwrap();
    let sql = "SELECT array::distinct(items) FROM docs WHERE n=50";
    let scan = reader.profile_select(sql, &p).unwrap();
    writer
        .execute("CREATE INDEX docs_n ON docs(n)", &p)
        .unwrap();
    let index = reader.profile_select(sql, &p).unwrap();
    assert_eq!(scan.result.rows, index.result.rows);
    assert!(index.metrics.rows_read < scan.metrics.rows_read);
    assert!(index.metrics.fullscan_steps < scan.metrics.fullscan_steps);
    reader.execute("BEGIN", &p).unwrap();
    assert_eq!(reader.execute(sql, &p).unwrap().rows, index.result.rows);
    writer
        .execute("UPDATE docs SET items=array::new(3,3) WHERE n=50", &p)
        .unwrap();
    assert_eq!(reader.execute(sql, &p).unwrap().rows, index.result.rows);
    reader.execute("COMMIT", &p).unwrap();
    assert_eq!(
        reader.execute(sql, &p).unwrap().rows,
        vec![vec![Value::Array(vec![Value::Integer(3)])]]
    );
    assert!(reader
        .select_with_limits(
            sql,
            &p,
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
            .execute_cancellable(sql, &p, &token)
            .unwrap_err()
            .code(),
        "FDB_CANCELLED"
    );
    assert_eq!(reader.execute(sql, &p).unwrap().rows.len(), 1);
}

#[test]
fn object_namespace_remains_available_to_existing_stored_functions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("namespace.db");
    {
        let c = Database::open(path.to_str().unwrap())
            .unwrap()
            .connect()
            .unwrap();
        c.execute("CREATE FUNCTION object::keys(value any) RETURNS string LANGUAGE JAVASCRIPT AS 'return \"user function\";'",&Parameters::new()).unwrap();
    }
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(
        c.execute("SELECT object::keys(null)", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::String("user function".into())]]
    );
    assert_eq!(
        c.execute(
            "SELECT doc::keys(doc::from_entries(array::new()))",
            &Parameters::new()
        )
        .unwrap()
        .rows,
        vec![vec![Value::Array(vec![])]]
    );
}
