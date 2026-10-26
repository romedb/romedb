use romedb::{Database, Document, Parameters, Value};

fn query(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

#[test]
fn unnest_preserves_typed_elements_positions_and_null_empty_semantics() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let values = vec![
        Value::Integer(i64::MAX),
        Value::Boolean(true),
        Value::Binary(vec![0, 255]),
        Value::vector32(&[1.0, 2.0]).unwrap(),
        Value::Record(romedb::Record {
            table: "refs".into(),
            key: romedb::Key::String("a".into()),
        }),
        Value::Object(Document::from([("x".into(), Value::Null)])),
        Value::Array(vec![Value::Integer(1)]),
        Value::Null,
        Value::String("FDB\u{1}{\"Integer\":9}".into()),
    ];
    let params = Parameters::from([("$items".into(), Value::Array(values.clone()))]);
    let expected = values
        .into_iter()
        .enumerate()
        .map(|(i, v)| vec![Value::Integer(i as i64), v])
        .collect::<Vec<_>>();
    for sql in [
        "SELECT u.key,u.value FROM array::unnest($items) u ORDER BY u.key",
        "SELECT u.* FROM array::unnest($items) u ORDER BY u.key",
        "SELECT key,value FROM array::unnest($items) ORDER BY key",
    ] {
        let result = c
            .execute(sql, &params)
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
        assert_eq!(result.columns, vec!["key", "value"], "{sql}");
        assert_eq!(result.rows, expected, "{sql}");
    }
    for value in [Value::Null, Value::Array(vec![])] {
        assert!(c
            .execute(
                "SELECT * FROM array::unnest($items)",
                &Parameters::from([("$items".into(), value)])
            )
            .unwrap()
            .rows
            .is_empty());
    }
    for sql in [
        "SELECT * FROM array::unnest(1)",
        "SELECT * FROM array::unnest()",
        "SELECT * FROM array::unnest(NULL,NULL)",
        "SELECT * FROM array::unnest($missing)",
        "SELECT array::unnest(NULL)",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
}

#[test]
fn unnest_uses_sql_filter_group_order_limit_and_multiple_array_products() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    query(
        &c,
        "INSERT INTO docs {id:docs:a,items:[1,2,2],tags:['x','y']}",
    );
    query(&c, "INSERT INTO docs {id:docs:b,items:[],tags:['z']}");
    query(&c, "INSERT INTO docs {id:docs:c,items:null}");
    assert_eq!(query(&c,"SELECT i.value,count(*) FROM docs d CROSS JOIN array::unnest(d.items) i WHERE i.value>1 GROUP BY i.value ORDER BY i.value LIMIT 1").rows,vec![vec![Value::Integer(2),Value::Integer(2)]]);
    assert_eq!(query(&c,"SELECT i.key,t.key,i.value,t.value FROM docs d CROSS JOIN array::unnest(d.items) i CROSS JOIN array::unnest(d.tags) t ORDER BY i.key,t.key LIMIT 2 OFFSET 1").rows,vec![vec![Value::Integer(0),Value::Integer(1),Value::Integer(1),Value::String("y".into())],vec![Value::Integer(1),Value::Integer(0),Value::Integer(2),Value::String("x".into())]]);
    assert_eq!(query(&c,"SELECT d.id,i.value FROM docs d LEFT JOIN array::unnest(d.items) i ON true ORDER BY d.id,i.key").rows.len(),5);
    query(&c, "CREATE TABLE copies");
    query(
        &c,
        "INSERT INTO copies(n) SELECT i.value FROM docs d CROSS JOIN array::unnest(d.items) i",
    );
    assert_eq!(
        query(&c, "SELECT n FROM copies ORDER BY n").rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(2)]
        ]
    );
    assert_eq!(query(&c,"WITH expanded AS (SELECT i.value AS n FROM docs d CROSS JOIN array::unnest(d.items) i) SELECT sum(n) FROM expanded").rows,vec![vec![Value::Integer(5)]]);
}

#[test]
fn unnest_limits_reject_products_even_when_aggregate_output_is_one_row() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let huge = Parameters::from([("$items".into(), Value::Array(vec![Value::Null; 100_001]))]);
    assert_eq!(
        c.execute("SELECT count(*) FROM array::unnest($items)", &huge)
            .unwrap_err()
            .code(),
        "FDB_LIMIT"
    );
    let product =
        Parameters::from([("$items".into(), Value::Array(vec![Value::Integer(1); 1001]))]);
    assert_eq!(
        c.execute(
            "SELECT count(*) FROM array::unnest($items) a CROSS JOIN array::unnest($items) b",
            &product
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        query(&c, "SELECT count(*) FROM array::unnest(array::new(1,2))").rows,
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn unnest_preserves_source_indexes_snapshots_atomic_writes_limits_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("unnest.db");
    let sql="SELECT d.id,i.key,i.value FROM docs d CROSS JOIN array::unnest(d.items) i WHERE d.group_no=1 ORDER BY d.id,i.key LIMIT 4";
    let expected;
    {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let a = db.connect().unwrap();
        let b = db.connect().unwrap();
        query(&a, "BEGIN");
        for n in 0..100 {
            query(
                &a,
                &format!(
                    "INSERT INTO docs {{id:type::record('docs',{n}),group_no:{},items:[1,2]}}",
                    n % 10
                ),
            );
        }
        query(&a, "COMMIT");
        let scan = a.profile_select(sql, &Parameters::new()).unwrap();
        query(&a, "CREATE INDEX docs_group ON docs(group_no)");
        let indexed = a.profile_select(sql, &Parameters::new()).unwrap();
        assert_eq!(scan.result.rows, indexed.result.rows);
        assert!(indexed.metrics.rows_read < scan.metrics.rows_read);
        assert!(indexed.metrics.fullscan_steps < scan.metrics.fullscan_steps);
        query(&a, "BEGIN");
        assert_eq!(query(&a, sql).rows, indexed.result.rows);
        query(&b, "UPDATE docs SET items=array::new(3,4)");
        assert_eq!(query(&a, sql).rows, indexed.result.rows);
        query(&a, "COMMIT");
        assert_ne!(query(&a, sql).rows, indexed.result.rows);
        query(&a, "CREATE TABLE copies");
        query(&a, "DEFINE FIELD n ON copies TYPE integer CHECK(n<4)");
        query(&a, "BEGIN");
        query(&a, "INSERT INTO copies {id:copies:prior,n:0}");
        assert!(a.execute("INSERT INTO copies(n) SELECT i.value FROM docs d CROSS JOIN array::unnest(d.items) i",&Parameters::new()).is_err());
        assert_eq!(
            query(&a, "SELECT n FROM copies").rows,
            vec![vec![Value::Integer(0)]]
        );
        assert_eq!(
            a.select_with_limits(
                sql,
                &Parameters::new(),
                romedb::ResultLimits {
                    max_rows: 2,
                    max_payload_bytes: 10000
                }
            )
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        let token = romedb::CancellationToken::new();
        token.cancel();
        assert_eq!(
            a.execute_cancellable(sql, &Parameters::new(), &token)
                .unwrap_err()
                .code(),
            "FDB_CANCELLED"
        );
        assert_eq!(a.transaction_state(), romedb::TransactionState::Active);
        expected = query(&a, sql).rows;
        query(&a, "COMMIT");
    }
    let c = Database::open(file.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(query(&c, sql).rows, expected);
    assert_eq!(
        query(&c, "SELECT n FROM copies").rows,
        vec![vec![Value::Integer(0)]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    c.check_collection_integrity("copies", Default::default())
        .unwrap();
}

#[test]
fn unnest_byte_limits_and_explain_preserve_caller_transaction_work() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let items = Value::Array(vec![Value::String("x".repeat(2 * 1024 * 1024)); 10]);
    let params = Parameters::from([("$items".into(), items)]);
    query(&c, "BEGIN");
    for id in ["a", "b"] {
        c.execute(
            &format!("INSERT INTO docs {{id:docs:{id},items:$items}}"),
            &params,
        )
        .unwrap();
    }
    let sql = "SELECT count(*) FROM docs d CROSS JOIN array::unnest(d.items) i";
    for explain in ["EXPLAIN", "EXPLAIN QUERY PLAN"] {
        assert!(!query(&c, &format!("{explain} {sql}")).rows.is_empty());
    }
    let error = c.execute(sql, &Parameters::new()).unwrap_err();
    assert_eq!(error.code(), "FDB_LIMIT", "{error}");
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(
        query(&c, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(2)]]
    );
    query(&c, "COMMIT");
    assert_eq!(
        query(&c, "SELECT count(*) FROM array::unnest(array::new(NULL))").rows,
        vec![vec![Value::Integer(1)]]
    );
}
