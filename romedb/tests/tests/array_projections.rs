use romedb::{Database, Document, Parameters, Value};

fn query(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

#[test]
fn array_predicates_use_element_scope_parameters_and_composable_paths() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    query(&c,"INSERT INTO docs {id:docs:a,active:false,items:[{active:true,n:1,profile:{city:'A'}},{active:false,n:2},{active:true,n:3,profile:{city:'C'}},null,7],empty:[],scalar:1}");
    let parameters = Parameters::from([
        ("$min".into(), Value::Integer(2)),
        ("$this".into(), Value::Integer(3)),
    ]);
    let result=c.execute("SELECT d.items[WHERE active = true AND n >= $min].profile.{city} AS cities,d.items[WHERE this.n=$this][0].n AS first FROM docs d",&parameters).unwrap();
    assert_eq!(result.columns, vec!["cities", "first"]);
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Array(vec![Value::Object(Document::from([(
                "city".into(),
                Value::String("C".into())
            )]))]),
            Value::Integer(3)
        ]]
    );
    assert_eq!(
        query(
            &c,
            "SELECT empty[WHERE active],missing[WHERE active],scalar[WHERE active] FROM docs"
        )
        .rows,
        vec![vec![Value::Array(vec![]), Value::Null, Value::Null]]
    );
    assert_eq!(
        query(&c, "SELECT items[WHERE this IS NULL] FROM docs").rows,
        vec![vec![Value::Array(vec![Value::Null])]]
    );
    assert_eq!(
        query(&c, "SELECT items[WHERE active IS NULL] FROM docs").rows[0][0],
        Value::Array(vec![Value::Null, Value::Integer(7)])
    );
    let error = c
        .execute(
            "SELECT empty[WHERE n=$missing] FROM docs",
            &Parameters::new(),
        )
        .unwrap_err();
    assert_eq!(error.code(), "FDB_PARAMETER");
    query(&c, "INSERT INTO refs {id:refs:a,active:true}");
    query(&c, "UPDATE docs:a MERGE {links:[refs:a,refs:missing]}");
    let links = c
        .profile_select(
            "SELECT links[WHERE this=refs:a] FROM docs",
            &Parameters::new(),
        )
        .unwrap();
    assert_eq!(links.metrics.fetch_batches, 0);
    assert_eq!(
        links.result.rows[0][0],
        query(&c, "SELECT array::new(refs:a)").rows[0][0]
    );
    assert_eq!(
        query(&c, "SELECT links[WHERE active=true] FROM docs").rows,
        vec![vec![Value::Array(vec![])]]
    );
}

#[test]
fn array_scalar_predicates_match_pinned_sql_comparisons_and_truth() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let values = vec![
        Value::Null,
        Value::Boolean(false),
        Value::Boolean(true),
        Value::Integer(0),
        Value::Integer(2),
        Value::Integer(i64::MAX),
        Value::Integer(9_007_199_254_740_993),
        Value::Number(9_007_199_254_740_992.0),
        Value::Number(-0.0),
        Value::Number(0.5),
        Value::String("0".into()),
        Value::String("2tail".into()),
        Value::String("true".into()),
        Value::Binary(b"1suffix".to_vec()),
    ];
    c.execute(
        "INSERT INTO roots {id:roots:a,items:$items}",
        &Parameters::from([("$items".into(), Value::Array(values.clone()))]),
    )
    .unwrap();
    query(&c, "BEGIN");
    for (position, value) in values.iter().enumerate() {
        c.execute(
            "INSERT INTO elements {position:$position,v:$value}",
            &Parameters::from([
                ("$position".into(), Value::Integer(position as i64)),
                ("$value".into(), value.clone()),
            ]),
        )
        .unwrap();
    }
    query(&c, "COMMIT");
    for predicate in [
        "this",
        "NOT this",
        "this IS TRUE",
        "this IS FALSE",
        "this IS NOT TRUE",
        "this IS NULL",
        "this IS NOT NULL",
        "this AND NULL",
        "this OR NULL",
        "this = 1",
        "this <> 1",
        "this <= 2",
        "this > 9007199254740992",
        "this = 9007199254740993",
        "NOT (this = 0 OR this IS NULL)",
    ] {
        let expected = query(
            &c,
            &format!(
                "SELECT v FROM elements WHERE {} ORDER BY position",
                predicate.replace("this", "v")
            ),
        )
        .rows
        .into_iter()
        .map(|mut row| row.remove(0))
        .collect::<Vec<_>>();
        let actual = query(&c, &format!("SELECT items[WHERE {predicate}] FROM roots"));
        assert_eq!(
            actual.rows,
            vec![vec![Value::Array(expected)]],
            "{predicate}"
        );
    }
    let ordinary = c
        .profile_select("SELECT items FROM roots", &Parameters::new())
        .unwrap();
    let filtered = c
        .profile_select(
            "SELECT items[WHERE this IS NOT NULL] FROM roots",
            &Parameters::new(),
        )
        .unwrap();
    assert_eq!(ordinary.metrics.rows_read, filtered.metrics.rows_read);
    assert_eq!(ordinary.metrics.vm_steps, filtered.metrics.vm_steps);
    assert_eq!(filtered.metrics.fetch_batches, 0);
}

#[test]
fn destructuring_preserves_typed_members_missing_values_and_source_precedence() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let profile = Value::Object(Document::from([
        ("city".into(), Value::String("Paris".into())),
        ("country".into(), Value::Null),
        ("bytes".into(), Value::Binary(vec![0, 255])),
        ("integer".into(), Value::Integer(i64::MAX)),
        ("vector".into(), Value::vector32(&[1.0, 2.0]).unwrap()),
        ("odd.key".into(), Value::Boolean(true)),
        ("secret".into(), Value::String("hidden".into())),
    ]));
    c.execute("INSERT INTO docs {id:docs:a,name:'root',profile:$profile,items:array::new($profile,null,7)}",&Parameters::from([("$profile".into(),profile.clone())])).unwrap();
    let Value::Object(mut expected) = profile else {
        panic!()
    };
    expected.remove("secret");
    let row = query(
        &c,
        r#"SELECT profile.{city,country,bytes,integer,vector,"odd.key",absent} AS profile,items.{city,country,absent} AS items FROM docs"#,
    );
    let narrow = Value::Object(Document::from([
        ("city".into(), Value::String("Paris".into())),
        ("country".into(), Value::Null),
    ]));
    assert_eq!(
        row.rows,
        vec![vec![
            Value::Object(expected),
            Value::Array(vec![narrow.clone(), Value::Null, Value::Null])
        ]]
    );
    assert_eq!(
        query(&c, "SELECT profile.{name} FROM docs profile").rows,
        vec![vec![Value::Object(Document::from([(
            "name".into(),
            Value::String("root".into())
        )]))]]
    );
    assert_eq!(
        query(
            &c,
            "SELECT profile.profile.{city,country} FROM docs profile"
        )
        .rows,
        vec![vec![narrow]]
    );
    query(
        &c,
        "INSERT INTO refs {id:refs:a,name:'linked',other:refs:missing}",
    );
    query(&c, "UPDATE docs:a MERGE {link:refs:a}");
    assert_eq!(
        query(&c, "SELECT link.{name,absent} FROM docs").rows,
        vec![vec![Value::Object(Document::from([(
            "name".into(),
            Value::String("linked".into())
        )]))]]
    );
    assert_eq!(
        query(&c, "SELECT profile.{city}.city FROM docs").rows,
        vec![vec![Value::String("Paris".into())]]
    );
    for sql in [
        "SELECT profile.{} FROM docs",
        "SELECT profile.{city,city} FROM docs",
        "SELECT profile.{city AS place} FROM docs",
        "SELECT profile.{*} FROM docs",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
}

#[test]
fn predicate_syntax_preserves_bracket_names_and_rejects_unbounded_expressions() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    query(
        &c,
        r#"INSERT INTO docs {id:docs:a,n:1,items:[1,2],"WHERE active = true":9}"#,
    );
    assert_eq!(
        query(
            &c,
            "SELECT n AS [WHERE active = true],[WHERE active = true] FROM docs"
        )
        .rows,
        vec![vec![Value::Integer(1), Value::Integer(9)]]
    );
    for sql in [
        "SELECT items[WHERE random() > 0] FROM docs",
        "SELECT items[WHERE this + 1 > 2] FROM docs",
        "SELECT items[WHERE this = ?] FROM docs",
        "SELECT items[WHERE this = 1] FROM docs items",
        "SELECT id FROM docs WHERE items[WHERE this = 1] IS NOT NULL",
        "SELECT items[WHERE EXISTS (SELECT 1)] FROM docs",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert_eq!(
        query(&c, "SELECT items.items[WHERE this > 1] FROM docs items").rows,
        vec![vec![Value::Array(vec![Value::Integer(2)])]]
    );
    let mut expression = "this=1".to_owned();
    for _ in 0..6 {
        expression = format!("({expression}) OR ({expression})")
    }
    assert_eq!(
        c.execute(
            &format!("SELECT items[WHERE {expression}] FROM docs"),
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    c.execute(
        "UPDATE docs:a CONTENT {items:$items}",
        &Parameters::from([("$items".into(), Value::Array(vec![Value::Null; 300_000]))]),
    )
    .unwrap();
    assert_eq!(
        c.execute(
            "SELECT items[WHERE this IS NULL] FROM docs",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        query(&c, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn array_projections_preserve_index_use_transactions_limits_and_persisted_types() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("arrays.db");
    let sql = "SELECT items[WHERE active=true].{n,bytes} AS selected FROM docs WHERE group_no=$group ORDER BY id LIMIT 3";
    let params = Parameters::from([("$group".into(), Value::Integer(1))]);
    let expected;
    {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let a = db.connect().unwrap();
        let b = db.connect().unwrap();
        query(&a, "BEGIN");
        for n in 0..100 {
            a.execute(&format!("INSERT INTO docs {{id:type::record('docs',{n}),group_no:{},items:[{{active:true,n:{n},bytes:$bytes}},{{active:false,n:0}}]}}",n%10),&Parameters::from([("$bytes".into(),Value::Binary(vec![0,255]))])).unwrap();
        }
        query(&a, "COMMIT");
        let scan = a.profile_select(sql, &params).unwrap();
        query(&a, "CREATE INDEX docs_group ON docs(group_no)");
        let indexed = a.profile_select(sql, &params).unwrap();
        assert_eq!(indexed.result.rows, scan.result.rows);
        assert_eq!(indexed.result.rows.len(), 3);
        assert!(indexed.metrics.rows_read < scan.metrics.rows_read);
        assert!(indexed.metrics.fullscan_steps < scan.metrics.fullscan_steps);
        query(&a, "BEGIN");
        assert_eq!(a.execute(sql, &params).unwrap().rows, indexed.result.rows);
        query(
            &b,
            "UPDATE docs PATCH [{op:'replace',path:'/items/0/n',value:99}]",
        );
        assert_eq!(a.execute(sql, &params).unwrap().rows, indexed.result.rows);
        query(&a, "COMMIT");
        assert_ne!(a.execute(sql, &params).unwrap().rows, indexed.result.rows);
        query(&a, "BEGIN");
        query(
            &a,
            "UPDATE docs PATCH [{op:'replace',path:'/items/0/n',value:100}]",
        );
        expected = a.execute(sql, &params).unwrap().rows;
        assert_eq!(
            a.select_with_limits(
                sql,
                &params,
                romedb::ResultLimits {
                    max_rows: 3,
                    max_payload_bytes: 1
                }
            )
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        let token = romedb::CancellationToken::new();
        token.cancel();
        assert_eq!(
            a.execute_cancellable(sql, &params, &token)
                .unwrap_err()
                .code(),
            "FDB_CANCELLED"
        );
        assert_eq!(a.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(a.execute(sql, &params).unwrap().rows, expected);
        query(&a, "CREATE TABLE copies");
        a.execute(&format!("INSERT INTO copies(payload) {sql}"), &params)
            .unwrap();
        assert_eq!(query(&a, "SELECT payload FROM copies").rows, expected);
        query(&a, "CREATE TABLE invalid");
        query(&a, "DEFINE FIELD payload ON invalid TYPE string REQUIRED");
        assert!(a
            .execute(&format!("INSERT INTO invalid(payload) {sql}"), &params)
            .is_err());
        assert_eq!(
            query(&a, "SELECT count(*) FROM invalid").rows,
            vec![vec![Value::Integer(0)]]
        );
        query(&a, "COMMIT");
    }
    let c = Database::open(file.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(c.execute(sql, &params).unwrap().rows, expected);
    assert_eq!(query(&c, "SELECT payload FROM copies").rows, expected);
    for collection in ["docs", "copies", "invalid"] {
        c.check_collection_integrity(collection, Default::default())
            .unwrap();
    }
}

#[test]
fn array_predicate_bounds_include_values_even_when_no_element_matches() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let needle = Value::String("x".repeat(2 * 1024 * 1024));
    c.execute(
        "INSERT INTO docs {items:$items}",
        &Parameters::from([("$items".into(), Value::Array(vec![needle.clone(); 10]))]),
    )
    .unwrap();
    let error = c
        .execute(
            "SELECT items[WHERE this=$needle AND this<>$needle] FROM docs",
            &Parameters::from([("$needle".into(), needle)]),
        )
        .unwrap_err();
    assert_eq!(error.code(), "FDB_LIMIT");
    assert!(
        error.to_string().contains("predicate evaluated values"),
        "{error}"
    );
    assert_eq!(
        query(&c, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(1)]]
    );
}
