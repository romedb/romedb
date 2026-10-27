use romedb::{Database, Parameters, Value};

#[test]
fn closed_json_table_functions_match_native_mixed_joins() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let empty = Parameters::new();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE native(n INTEGER)",
        "INSERT INTO native VALUES(1),(2)",
        "INSERT INTO docs(n) SELECT n FROM native",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    for (iterator, params) in [
        ("json_each('[1,2,null]') AS j", empty.clone()),
        ("main.json_each('[1,2,null]') AS j", empty.clone()),
        ("temp.json_tree('[1,2,null]') AS j", empty.clone()),
        (
            "json_each(json_array(1+0,CAST('2' AS INTEGER),NULL)) AS j",
            empty.clone(),
        ),
        (
            "json_tree(json_object('a',json_array(1,2))) AS j",
            empty.clone(),
        ),
        (
            "json_each(CASE WHEN 1 THEN '[1,2]' ELSE 'invalid' END) AS j",
            empty.clone(),
        ),
        (
            "json_each('[' || $values || ']') AS j",
            Parameters::from([("$values".into(), Value::String("1,2,null".into()))]),
        ),
        (
            "json_each($json) AS j",
            Parameters::from([("$json".into(), Value::String("[1,2,null]".into()))]),
        ),
        ("json_tree('{\"a\":[1,2]}') AS j", empty.clone()),
        (
            "json_each(?1) AS j",
            Parameters::from([("?1".into(), Value::String("[1,2,null]".into()))]),
        ),
        (
            "json_each(?) AS j",
            Parameters::from([("?1".into(), Value::String("[1,2,null]".into()))]),
        ),
        ("json_each('{\"a\":[1,2]}','$.a') AS j", empty.clone()),
    ] {
        for (projection, join) in [
            ("d.n,j.key,j.value,j.type,j.atom", "CROSS JOIN"),
            ("d.n,j.*", "CROSS JOIN"),
            ("d.n,j.value", "LEFT JOIN"),
        ] {
            let on = if join == "LEFT JOIN" {
                " ON d.n=j.value"
            } else {
                ""
            };
            let query = |source| {
                format!(
                    "SELECT {projection} FROM {source} d {join} {iterator}{on} ORDER BY d.n,j.id"
                )
            };
            let expected = c.execute(&query("native"), &params).unwrap();
            let sql = query("docs");
            for actual in [
                c.execute(&sql, &params).unwrap(),
                c.profile_select(&sql, &params).unwrap().result,
            ] {
                assert_eq!(actual.columns, expected.columns, "{sql}");
                assert_eq!(actual.rows, expected.rows, "{sql}");
            }
        }
    }
}

#[test]
fn json_table_function_sources_preserve_write_failure_and_retry() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let empty = Parameters::new();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs(n) VALUES(1)",
        "CREATE TABLE output",
        "DEFINE FIELD n ON output TYPE integer CHECK(n<3)",
        "CREATE UNIQUE INDEX output_n ON output(n)",
        "BEGIN",
        "INSERT INTO output {id:output:prior,n:0}",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    let query="SELECT d.n+json_each.value AS n FROM json_each(coalesce($json,'[0]')) CROSS JOIN docs d ORDER BY json_each.key";
    assert_eq!(
        c.execute(query, &empty).unwrap_err().code(),
        "FDB_PARAMETER"
    );
    let sql = format!("INSERT INTO output(n) {query} RETURNING n");
    let bad = Parameters::from([("$json".into(), Value::String("[1,2]".into()))]);
    assert_eq!(c.execute(&sql, &bad).unwrap_err().code(), "FDB_VALIDATION");
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(
        c.execute("SELECT n FROM output", &empty).unwrap().rows,
        vec![vec![Value::Integer(0)]]
    );
    assert!(c
        .lookup_index("output", "output_n", &Value::Integer(2))
        .unwrap()
        .is_empty());
    let good = Parameters::from([("$json".into(), Value::String("[1]".into()))]);
    assert_eq!(
        c.execute(&sql, &good).unwrap().rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        c.check_collection_integrity("output", romedb::IntegrityLimits::default())
            .unwrap()
            .documents,
        2
    );
    c.execute("ROLLBACK", &empty).unwrap();
    assert_eq!(
        c.check_collection_integrity("output", romedb::IntegrityLimits::default())
            .unwrap()
            .documents,
        0
    );
}

#[test]
fn correlated_json_iterators_match_native_rows() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let params = Parameters::new();
    for sql in [
        "CREATE TABLE native(n INTEGER,j TEXT,value TEXT)",
        "INSERT INTO native(n,j) VALUES(1,'[1,2]'),(2,'[3]'),(3,'[]'),(4,NULL)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,j) SELECT n,j FROM native",
    ] {
        c.execute(sql, &params).unwrap();
    }
    for arg in ["d.j", "j", "coalesce(d.j,'[]')", "coalesce(j,'[]')"] {
        for join in ["CROSS JOIN", "LEFT JOIN"] {
            let query = |source| {
                format!("SELECT d.n,x.key,x.value FROM {source} d {join} json_each({arg}) x ORDER BY d.n,x.key")
            };
            let expected = c.execute(&query("native"), &params).unwrap();
            let sql = query("docs");
            for actual in [
                c.execute(&sql, &params).unwrap(),
                c.profile_select(&sql, &params).unwrap().result,
            ] {
                assert_eq!(actual.columns, expected.columns, "{sql}");
                assert_eq!(actual.rows, expected.rows, "{sql}");
            }
        }
    }
    let query = |source| {
        format!("SELECT d.n,x.value,y.value FROM {source} d CROSS JOIN json_each(d.j) x CROSS JOIN json_each(json_array(x.value+1)) y ORDER BY d.n,x.key")
    };
    let expected = c.execute(&query("native"), &params).unwrap();
    assert_eq!(
        c.execute(&query("docs"), &params).unwrap().rows,
        expected.rows
    );
    for sql in [
        "CREATE TABLE output",
        "DEFINE FIELD n ON output TYPE integer CHECK(n<3)",
        "CREATE UNIQUE INDEX output_n ON output(n)",
        "BEGIN",
        "INSERT INTO output(n) VALUES(0)",
    ] {
        c.execute(sql, &params).unwrap();
    }
    let sql = "INSERT INTO output(n) SELECT x.value FROM docs d CROSS JOIN json_each(d.j) x WHERE x.value IS NOT NULL ORDER BY d.n,x.key RETURNING n";
    assert_eq!(
        c.execute(sql, &params).unwrap_err().code(),
        "FDB_VALIDATION"
    );
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(
        c.execute("SELECT n FROM output", &params).unwrap().rows,
        vec![vec![Value::Integer(0)]]
    );
    assert!(c
        .lookup_index("output", "output_n", &Value::Integer(1))
        .unwrap()
        .is_empty());
    c.execute("UPDATE docs SET j='[]' WHERE n=2", &params)
        .unwrap();
    assert_eq!(
        c.execute(sql, &params).unwrap().rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    c.execute("ROLLBACK", &params).unwrap();
    assert_eq!(
        c.check_collection_integrity("output", romedb::IntegrityLimits::default())
            .unwrap()
            .documents,
        0
    );
    for source in ["native", "docs"] {
        assert!(c
            .execute(
                &format!("SELECT x.value FROM {source} d CROSS JOIN json_each(value) x"),
                &params
            )
            .is_err());
    }
    // A rejected ambiguous argument does not poison the connection.
    assert_eq!(
        c.execute("SELECT count(*) FROM docs", &params)
            .unwrap()
            .rows,
        vec![vec![Value::Integer(4)]]
    );
}

#[test]
fn deep_path_iterator_arguments_preserve_rows_and_atomic_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let empty = Parameters::new();
    c.execute("CREATE TABLE docs", &empty).unwrap();
    let payload = Value::Object(std::collections::BTreeMap::from([(
        "inner".into(),
        Value::Object(std::collections::BTreeMap::from([(
            "j".into(),
            Value::String("[1,2]".into()),
        )])),
    )]));
    c.execute(
        "INSERT INTO docs(payload) VALUES($p)",
        &Parameters::from([("$p".into(), payload)]),
    )
    .unwrap();
    for arg in [
        "d.payload.inner.j",
        "coalesce(d.payload.inner.j,'[]')",
        r#"d."payload"."inner"."j""#,
    ] {
        for iterator in ["json_each", "json_tree"] {
            let sql = format!(
                "SELECT x.key,x.value FROM docs d CROSS JOIN {iterator}({arg}) x ORDER BY x.id"
            );
            let native = format!("SELECT x.key,x.value FROM {iterator}('[1,2]') x ORDER BY x.id");
            let expected = c.execute(&native, &empty).unwrap();
            for actual in [
                c.execute(&sql, &empty).unwrap(),
                c.profile_select(&sql, &empty).unwrap().result,
            ] {
                assert_eq!(actual.columns, expected.columns, "{sql}");
                assert_eq!(actual.rows, expected.rows, "{sql}");
            }
        }
    }
    for sql in [
        "CREATE TABLE output",
        "DEFINE FIELD n ON output TYPE integer CHECK(n<2)",
        "CREATE UNIQUE INDEX output_n ON output(n)",
        "BEGIN",
        "INSERT INTO output(n) VALUES(0)",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    let sql="INSERT INTO output(n) SELECT x.value FROM docs d CROSS JOIN json_each(d.payload.inner.j) x ORDER BY x.key RETURNING n";
    assert_eq!(c.execute(sql, &empty).unwrap_err().code(), "FDB_VALIDATION");
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(
        c.execute("SELECT n FROM output", &empty).unwrap().rows,
        vec![vec![Value::Integer(0)]]
    );
    assert!(c
        .lookup_index("output", "output_n", &Value::Integer(1))
        .unwrap()
        .is_empty());
    c.execute("UPDATE docs SET payload.inner.j='[1]'", &empty)
        .unwrap();
    assert_eq!(
        c.execute(sql, &empty).unwrap().rows,
        vec![vec![Value::Integer(1)]]
    );
    c.execute("ROLLBACK", &empty).unwrap();
    assert_eq!(
        c.check_collection_integrity("output", romedb::IntegrityLimits::default())
            .unwrap()
            .documents,
        0
    );
}

#[test]
fn scalar_subquery_iterator_arguments_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let params = Parameters::from([("$json".into(), Value::String("[1,2]".into()))]);
    for sql in [
        "CREATE TABLE native(n INTEGER,j TEXT)",
        "INSERT INTO native VALUES(1,'[1,2]'),(2,'[3]')",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,j) SELECT n,j FROM native",
    ] {
        c.execute(sql, &Parameters::new()).unwrap();
    }
    for arg in [
        "(SELECT '[1,2]')",
        "(SELECT $json)",
        "(SELECT j FROM native WHERE n=d.n)",
        "coalesce((SELECT j FROM native WHERE n=d.n),'[]')",
    ] {
        let params = if arg.contains("$json") {
            params.clone()
        } else {
            Parameters::new()
        };
        let query = |source| {
            format!("SELECT d.n,x.key,x.value FROM {source} d CROSS JOIN json_each({arg}) x ORDER BY d.n,x.key")
        };
        let expected = c.execute(&query("native"), &params).unwrap();
        let sql = query("docs");
        for actual in [
            c.execute(&sql, &params).unwrap(),
            c.profile_select(&sql, &params).unwrap().result,
        ] {
            assert_eq!(actual.columns, expected.columns, "{sql}");
            assert_eq!(actual.rows, expected.rows, "{sql}");
        }
    }
    let empty = Parameters::new();
    assert_eq!(
        c.execute(
            "SELECT x.value FROM docs d CROSS JOIN json_each((SELECT $json)) x",
            &empty
        )
        .unwrap_err()
        .code(),
        "FDB_PARAMETER"
    );
    for sql in [
        "CREATE TABLE output",
        "DEFINE FIELD n ON output TYPE integer CHECK(n<2)",
        "CREATE UNIQUE INDEX output_n ON output(n)",
        "BEGIN",
        "INSERT INTO output(n) VALUES(0)",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    let sql="INSERT INTO output(n) SELECT x.value FROM docs d CROSS JOIN json_each((SELECT j FROM native WHERE n=d.n)) x WHERE x.value IS NOT NULL ORDER BY d.n,x.key RETURNING n";
    assert_eq!(c.execute(sql, &empty).unwrap_err().code(), "FDB_VALIDATION");
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(
        c.execute("SELECT n FROM output", &empty).unwrap().rows,
        vec![vec![Value::Integer(0)]]
    );
    assert!(c
        .lookup_index("output", "output_n", &Value::Integer(1))
        .unwrap()
        .is_empty());
    c.execute(
        "UPDATE native SET j=CASE n WHEN 1 THEN '[1]' ELSE '[]' END",
        &empty,
    )
    .unwrap();
    assert_eq!(
        c.execute(sql, &empty).unwrap().rows,
        vec![vec![Value::Integer(1)]]
    );
    c.execute("ROLLBACK", &empty).unwrap();
    assert_eq!(
        c.check_collection_integrity("output", romedb::IntegrityLimits::default())
            .unwrap()
            .documents,
        0
    );
}

#[test]
fn iterator_correlated_cte_arguments_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let empty = Parameters::new();
    for sql in [
        "CREATE TABLE native(n INTEGER,j TEXT)",
        "INSERT INTO native VALUES(1,'[4]'),(2,'[5]')",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,j) SELECT n,j FROM native",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    for arg in ["(SELECT s.j FROM native s WHERE s.n=x.value)",
        "(WITH a AS (SELECT x.value AS n) SELECT json_array(n) FROM a)",
        "(WITH a AS (SELECT x.value AS n), b AS (SELECT n+1 AS n FROM a) SELECT json_array(n) FROM b)",
        "(WITH a AS (SELECT x.n AS n FROM native x WHERE x.n=2) SELECT json_array(n) FROM a)",
        "(WITH a AS (SELECT x.value+$delta AS n) SELECT json_array(n) FROM a)"] {
        let query=|source|format!("SELECT d.n,x.value,y.value FROM {source} d CROSS JOIN json_each('[1,2]') x CROSS JOIN json_each({arg}) y ORDER BY d.n,x.key,y.key");
        let params=if arg.contains("$delta") {Parameters::from([("$delta".into(),Value::Integer(1))])} else {empty.clone()};
        let expected=c.execute(&query("native"),&params).unwrap();
        let sql=query("docs");
        for actual in [c.execute(&sql,&params).unwrap(),c.profile_select(&sql,&params).unwrap().result] {
            assert_eq!(actual.columns,expected.columns,"{sql}");
            assert_eq!(actual.rows,expected.rows,"{sql}");
        }
    }
    for sql in [
        "CREATE TABLE output",
        "DEFINE FIELD n ON output TYPE integer CHECK(n<2)",
        "CREATE UNIQUE INDEX output_n ON output(n)",
        "BEGIN",
        "INSERT INTO output(n) VALUES(0)",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    let sql="INSERT INTO output(n) SELECT y.value FROM docs d CROSS JOIN json_each('[1,2]') x CROSS JOIN json_each((WITH a AS (SELECT x.value AS n) SELECT json_array(n) FROM a)) y WHERE d.n=1 ORDER BY x.key RETURNING n";
    assert_eq!(c.execute(sql, &empty).unwrap_err().code(), "FDB_VALIDATION");
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(
        c.execute("SELECT n FROM output", &empty).unwrap().rows,
        vec![vec![Value::Integer(0)]]
    );
    assert!(c
        .lookup_index("output", "output_n", &Value::Integer(1))
        .unwrap()
        .is_empty());
    let retry = sql.replace("WHERE d.n=1", "WHERE d.n=1 AND y.value=1");
    assert_eq!(
        c.execute(&retry, &empty).unwrap().rows,
        vec![vec![Value::Integer(1)]]
    );
    c.execute("ROLLBACK", &empty).unwrap();
    assert_eq!(
        c.check_collection_integrity("output", romedb::IntegrityLimits::default())
            .unwrap()
            .documents,
        0
    );
}

#[test]
fn iterator_deadlines_preserve_transaction_work_and_allow_retry() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let empty = Parameters::new();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs(n) VALUES(1)",
        "CREATE TABLE output",
        "CREATE UNIQUE INDEX output_n ON output(n)",
        "BEGIN",
        "INSERT INTO output(n) VALUES(0)",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    let json = format!("[{}]", vec!["1"; 5000].join(","));
    let params = Parameters::from([("$json".into(), Value::String(json))]);
    let select = "SELECT count(*) AS n FROM docs d CROSS JOIN json_each($json) x CROSS JOIN json_each($json) y WHERE d.n=1";
    let write = format!("INSERT INTO output(n) {select} RETURNING n");
    for sql in [select, write.as_str()] {
        let token = romedb::CancellationToken::with_deadline(
            std::time::Instant::now() + std::time::Duration::from_millis(20),
        );
        assert_eq!(
            c.execute_cancellable(sql, &params, &token)
                .unwrap_err()
                .code(),
            "FDB_CANCELLED"
        );
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(
            c.execute("SELECT n FROM output", &empty).unwrap().rows,
            vec![vec![Value::Integer(0)]]
        );
        assert_eq!(
            c.check_collection_integrity("output", romedb::IntegrityLimits::default())
                .unwrap()
                .index_entries,
            1
        );
    }
    let small = Parameters::from([("$json".into(), Value::String("[1,2]".into()))]);
    assert_eq!(
        c.execute(&write, &small).unwrap().rows,
        vec![vec![Value::Integer(4)]]
    );
    let rows = "SELECT x.value AS v FROM docs d CROSS JOIN json_each($json) x";
    let limit = romedb::ResultLimits {
        max_rows: 1,
        max_payload_bytes: 100,
    };
    assert_eq!(
        c.select_with_limits(rows, &small, limit)
            .unwrap_err()
            .code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        c.profile_select_with_limits(rows, &small, limit)
            .unwrap_err()
            .code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        c.execute(rows, &small).unwrap().rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    c.execute("ROLLBACK", &empty).unwrap();
    assert_eq!(
        c.check_collection_integrity("output", romedb::IntegrityLimits::default())
            .unwrap()
            .documents,
        0
    );
}

#[test]
fn iterator_predicate_subqueries_preserve_native_nulls() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let params = Parameters::new();
    for sql in [
        "CREATE TABLE native(n INTEGER)",
        "INSERT INTO native VALUES(1),(2),(NULL)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n) SELECT n FROM native",
    ] {
        c.execute(sql, &params).unwrap();
    }
    for arg in [
        "json_array(EXISTS(SELECT 1 FROM native s WHERE s.n=d.n))",
        "json_array(d.n IN (SELECT n FROM native))",
        "json_array(d.n NOT IN (SELECT n FROM native WHERE n=1))",
        "json_array(d.n IN (SELECT n FROM native WHERE 0))",
    ] {
        let query = |source| {
            format!("SELECT d.n,x.value FROM {source} d CROSS JOIN json_each({arg}) x ORDER BY d.n")
        };
        let expected = c.execute(&query("native"), &params).unwrap();
        let sql = query("docs");
        for actual in [
            c.execute(&sql, &params).unwrap(),
            c.profile_select(&sql, &params).unwrap().result,
        ] {
            assert_eq!(actual.rows, expected.rows, "{sql}");
        }
    }
    for sql in [
        "CREATE TABLE output",
        "DEFINE FIELD n ON output TYPE integer CHECK(n<1)",
        "CREATE UNIQUE INDEX output_n ON output(n)",
        "BEGIN",
        "INSERT INTO output(n) VALUES(-1)",
    ] {
        c.execute(sql, &params).unwrap();
    }
    let sql="INSERT INTO output(n) SELECT x.value FROM docs d CROSS JOIN json_each(json_array(d.n IN (SELECT n FROM native WHERE n=2))) x WHERE d.n IS NOT NULL ORDER BY d.n RETURNING n";
    assert_eq!(
        c.execute(sql, &params).unwrap_err().code(),
        "FDB_VALIDATION"
    );
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(
        c.execute("SELECT n FROM output", &params).unwrap().rows,
        vec![vec![Value::Integer(-1)]]
    );
    assert!(c
        .lookup_index("output", "output_n", &Value::Integer(0))
        .unwrap()
        .is_empty());
    let retry = sql.replace("WHERE d.n IS NOT NULL", "WHERE d.n=1");
    assert_eq!(
        c.execute(&retry, &params).unwrap().rows,
        vec![vec![Value::Integer(0)]]
    );
    c.execute("ROLLBACK", &params).unwrap();
    assert_eq!(
        c.check_collection_integrity("output", romedb::IntegrityLimits::default())
            .unwrap()
            .documents,
        0
    );
}

#[test]
fn grouped_and_windowed_iterators_match_native_and_preserve_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let params = Parameters::from([("$json".into(), Value::String("[1,2,2,null]".into()))]);
    let empty = Parameters::new();
    for sql in [
        "CREATE TABLE native(n INTEGER)",
        "INSERT INTO native VALUES(1),(2)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n) SELECT n FROM native",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    for template in [
        "SELECT x.value,count(*) AS total FROM SOURCE d CROSS JOIN json_each($json) x GROUP BY x.value HAVING count(*)>1 ORDER BY x.value",
        "SELECT DISTINCT x.value FROM SOURCE d CROSS JOIN json_each($json) x ORDER BY x.value",
        "SELECT d.n,x.value,row_number() OVER(PARTITION BY d.n ORDER BY x.key) AS r FROM SOURCE d CROSS JOIN json_each($json) x ORDER BY d.n,x.key",
        "SELECT d.n,x.value,sum(x.value) OVER w AS total FROM SOURCE d CROSS JOIN json_each($json) x WINDOW w AS(PARTITION BY d.n ORDER BY x.key) ORDER BY d.n,x.key",
    ] {
        let expected=c.execute(&template.replace("SOURCE","native"),&params).unwrap_or_else(|error|panic!("native {template}: {error}"));
        let sql=template.replace("SOURCE","docs");
        for actual in [c.execute(&sql,&params).unwrap(),c.profile_select(&sql,&params).unwrap().result] {
            assert_eq!(actual.columns,expected.columns,"{sql}");
            assert_eq!(actual.rows,expected.rows,"{sql}");
        }
    }
    for sql in [
        "CREATE TABLE output",
        "DEFINE FIELD n ON output TYPE integer CHECK(n<3)",
        "CREATE UNIQUE INDEX output_n ON output(n)",
        "BEGIN",
        "INSERT INTO output(n) VALUES(0)",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    let sql="INSERT INTO output(n) SELECT row_number() OVER(ORDER BY x.key) FROM docs d CROSS JOIN json_each($json) x WHERE d.n=1 RETURNING n";
    assert_eq!(
        c.execute(sql, &params).unwrap_err().code(),
        "FDB_VALIDATION"
    );
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(
        c.execute("SELECT n FROM output", &empty).unwrap().rows,
        vec![vec![Value::Integer(0)]]
    );
    assert!(c
        .lookup_index("output", "output_n", &Value::Integer(1))
        .unwrap()
        .is_empty());
    let small = Parameters::from([("$json".into(), Value::String("[1,2]".into()))]);
    assert_eq!(
        c.execute(sql, &small).unwrap().rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    c.execute("ROLLBACK", &empty).unwrap();
    assert_eq!(
        c.check_collection_integrity("output", romedb::IntegrityLimits::default())
            .unwrap()
            .documents,
        0
    );
}

#[test]
fn compound_iterator_pages_match_native_and_preserve_write_recovery() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let empty = Parameters::new();
    for sql in [
        "CREATE TABLE native(n INTEGER)",
        "INSERT INTO native VALUES(1),(2)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n) SELECT n FROM native",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    for op in ["UNION ALL", "UNION", "INTERSECT", "EXCEPT"] {
        for (limit, offset) in [(0, 0), (3, 0), (3, 1), (2, 8)] {
            let params = Parameters::from([
                ("$json".into(), Value::String("[1,2,null]".into())),
                ("$limit".into(), Value::Integer(limit)),
                ("$offset".into(), Value::Integer(offset)),
            ]);
            let query = |source| {
                format!("SELECT x.value AS v FROM {source} d CROSS JOIN json_each($json) x {op} SELECT n FROM {source} ORDER BY v LIMIT $limit OFFSET $offset")
            };
            let expected = c.execute(&query("native"), &params).unwrap();
            let sql = query("docs");
            for actual in [
                c.execute(&sql, &params).unwrap(),
                c.profile_select(&sql, &params).unwrap().result,
            ] {
                assert_eq!(actual.columns, expected.columns, "{sql}");
                assert_eq!(actual.rows, expected.rows, "{sql}");
            }
        }
    }
    for sql in [
        "CREATE TABLE output",
        "DEFINE FIELD n ON output TYPE integer CHECK(n<2)",
        "CREATE UNIQUE INDEX output_n ON output(n)",
        "BEGIN",
        "INSERT INTO output(n) VALUES(0)",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    let sql="INSERT INTO output(n) SELECT x.value AS v FROM docs d CROSS JOIN json_each('[1,2,null]') x INTERSECT SELECT n FROM docs ORDER BY v LIMIT $limit RETURNING n";
    let params = Parameters::from([("$limit".into(), Value::Integer(2))]);
    assert_eq!(
        c.execute(sql, &params).unwrap_err().code(),
        "FDB_VALIDATION"
    );
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(
        c.execute("SELECT n FROM output", &empty).unwrap().rows,
        vec![vec![Value::Integer(0)]]
    );
    assert!(c
        .lookup_index("output", "output_n", &Value::Integer(1))
        .unwrap()
        .is_empty());
    assert_eq!(
        c.execute(
            sql,
            &Parameters::from([("$limit".into(), Value::Integer(1))])
        )
        .unwrap()
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    c.execute("ROLLBACK", &empty).unwrap();
    assert_eq!(
        c.check_collection_integrity("output", romedb::IntegrityLimits::default())
            .unwrap()
            .documents,
        0
    );
}

#[test]
fn iterator_subqueries_resolve_outer_collection_fields() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let params = Parameters::new();
    for sql in [
        "CREATE TABLE native(n INTEGER,j TEXT)",
        "INSERT INTO native VALUES(1,'[1,2]'),(2,'[2]'),(3,'[]')",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,j) SELECT n,j FROM native",
    ] {
        c.execute(sql, &params).unwrap();
    }
    for template in [
        "SELECT d.n,(SELECT count(*) FROM json_each('[1,2]') x WHERE x.value=d.n) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n FROM SOURCE d WHERE EXISTS(SELECT 1 FROM json_each('[1,2]') x WHERE x.value=d.n) ORDER BY d.n",
        "SELECT d.n,(SELECT count(*) FROM json_each(d.j) x) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(SELECT count(*) FROM main.json_each(d.j) x) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(WITH a AS (SELECT x.value AS n FROM temp.json_each(d.j) x) SELECT sum(n) FROM a) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(WITH a AS (SELECT d.n+x.value AS v FROM json_each(d.j) x) SELECT sum(v) FROM a) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(WITH a AS (SELECT x.value+d.n AS v FROM json_each(d.j) x), b AS (SELECT v*2 AS v FROM a) SELECT sum(v) FROM b) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(WITH a AS (SELECT x.value+d.n AS v,count(*) AS k FROM json_each(d.j) x GROUP BY x.value HAVING count(*)>0) SELECT sum(v*k) FROM a) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(WITH a AS (SELECT x.value+d.n AS v,row_number() OVER (ORDER BY x.key) AS k FROM json_each(d.j) x) SELECT sum(v*k) FROM a) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(WITH a AS (SELECT x.value AS v,row_number() OVER (PARTITION BY d.n ORDER BY x.value+d.n DESC) AS k FROM json_each(d.j) x) SELECT sum(v*k) FROM a) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(WITH a AS (SELECT sum(x.value) AS v FROM json_each(d.j) x GROUP BY x.value HAVING d.n<3) SELECT sum(v) FROM a) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(WITH a AS (SELECT x.value+d.n AS v FROM json_each(d.j) x ORDER BY v DESC LIMIT 1) SELECT sum(v) FROM a) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(WITH a AS (SELECT x.value AS v FROM json_each(d.j) x ORDER BY x.value+d.n DESC LIMIT 1 OFFSET 1) SELECT sum(v) FROM a) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(WITH a AS (SELECT x.value+d.n AS v FROM json_each(d.j) x UNION ALL SELECT d.n ORDER BY 1 DESC LIMIT 2 OFFSET 1) SELECT sum(v) FROM a) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(WITH a AS (SELECT d.value AS v FROM json_each('[4,5]') d) SELECT sum(v) FROM a) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(WITH a AS (SELECT x.value+d.n AS v FROM json_each(d.j) x WHERE x.value>9) SELECT sum(v) FROM a) AS total FROM SOURCE d ORDER BY d.n",

        "SELECT d.n,(WITH a AS (SELECT x.value AS v FROM json_each(d.j) x UNION ALL SELECT d.n) SELECT sum(v) FROM a) AS total FROM SOURCE d ORDER BY d.n",


        "SELECT d.n FROM SOURCE d WHERE d.n IN(SELECT x.value FROM json_each(d.j) x) ORDER BY d.n",
        "SELECT d.n,(SELECT count(*) FROM json_each('[1,2]') d WHERE d.value=1) AS total FROM SOURCE d ORDER BY d.n",
        "SELECT d.n,(SELECT count(*) FROM json_each(d.j) x WHERE EXISTS(SELECT 1 FROM json_each('[2]') x WHERE x.value=d.n)) AS total FROM SOURCE d ORDER BY d.n",

    ] {
        let expected=c.execute(&template.replace("SOURCE","native"),&params).unwrap_or_else(|error|panic!("native {template}: {error}"));
        let sql=template.replace("SOURCE","docs");
        for actual in [c.execute(&sql,&params).unwrap_or_else(|error|panic!("{sql}: {error}")),c.profile_select(&sql,&params).unwrap().result] {assert_eq!(actual.columns,expected.columns,"{sql}");assert_eq!(actual.rows,expected.rows,"{sql}");}
    }
    for sql in [
        "CREATE TABLE output",
        "DEFINE FIELD n ON output TYPE integer CHECK(n<2)",
        "CREATE UNIQUE INDEX output_n ON output(n)",
        "BEGIN",
        "INSERT INTO output(n) VALUES(-1)",
    ] {
        c.execute(sql, &params).unwrap();
    }
    let sql="INSERT INTO output(n) SELECT (SELECT count(*) FROM json_each(d.j) x) FROM docs d ORDER BY d.n DESC RETURNING n";
    assert_eq!(
        c.execute(sql, &params).unwrap_err().code(),
        "FDB_VALIDATION"
    );
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(
        c.execute("SELECT n FROM output", &params).unwrap().rows,
        vec![vec![Value::Integer(-1)]]
    );
    assert!(c
        .lookup_index("output", "output_n", &Value::Integer(0))
        .unwrap()
        .is_empty());
    let retry = sql.replace("FROM docs d ORDER", "FROM docs d WHERE d.n>1 ORDER");
    assert_eq!(
        c.execute(&retry, &params).unwrap().rows,
        vec![vec![Value::Integer(0)], vec![Value::Integer(1)]]
    );
    c.execute("ROLLBACK", &params).unwrap();
    assert_eq!(
        c.check_collection_integrity("output", romedb::IntegrityLimits::default())
            .unwrap()
            .documents,
        0
    );
}

#[test]
fn malformed_iterator_input_reports_native_rollback_and_allows_retry() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let params = Parameters::new();
    for sql in [
        "CREATE TABLE native(n INTEGER,j TEXT)",
        "INSERT INTO native VALUES(1,'[1]'),(2,'invalid')",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,j) SELECT n,j FROM native",
        "CREATE TABLE output",
        "CREATE UNIQUE INDEX output_n ON output(n)",
        "INSERT INTO output(n) VALUES(-1)",
    ] {
        c.execute(sql, &params).unwrap();
    }
    for source in ["native", "docs"] {
        for iterator in ["json_each", "json_tree"] {
            let query=format!("SELECT x.value AS n FROM {source} d CROSS JOIN {iterator}(d.j) x WHERE x.type='integer' ORDER BY d.n,x.id");
            let insert = format!("INSERT INTO output(n) {query} RETURNING n");
            for mode in 0..3 {
                c.execute("BEGIN", &params).unwrap();
                c.execute("INSERT INTO output(n) VALUES(0)", &params)
                    .unwrap();
                let error = match mode {
                    0 => c.execute(&query, &params).unwrap_err(),
                    1 => c.profile_select(&query, &params).unwrap_err(),
                    _ => c.execute(&insert, &params).unwrap_err(),
                };
                assert_eq!(error.code(), "FDB_ENGINE");
                assert_eq!(c.transaction_state(), romedb::TransactionState::Autocommit);
                assert_eq!(
                    c.execute("SELECT n FROM output", &params).unwrap().rows,
                    vec![vec![Value::Integer(-1)]]
                );
                assert!(c
                    .lookup_index("output", "output_n", &Value::Integer(1))
                    .unwrap()
                    .is_empty());
                assert_eq!(
                    c.check_collection_integrity("output", romedb::IntegrityLimits::default())
                        .unwrap()
                        .index_entries,
                    1
                );
            }
            c.execute(&format!("UPDATE {source} SET j='[2]' WHERE n=2"), &params)
                .unwrap();
            assert_eq!(
                c.execute(&insert, &params).unwrap().rows,
                vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
            );
            c.execute("DELETE FROM output WHERE n>0", &params).unwrap();
            c.execute(
                &format!("UPDATE {source} SET j='invalid' WHERE n=2"),
                &params,
            )
            .unwrap();
        }
    }
}

#[test]
fn correlated_cte_projection_writes_preserve_validation_and_retry() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let empty = Parameters::new();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs(n,j) VALUES(1,'[1]'),(2,'[2]')",
        "CREATE TABLE output",
        "DEFINE FIELD n ON output TYPE integer CHECK(n<4)",
        "CREATE UNIQUE INDEX output_n ON output(n)",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    for expression in ["(WITH a AS (SELECT d.n+x.value AS v FROM json_each(d.j) x) SELECT sum(v) FROM a)","(WITH a AS (SELECT x.value AS v FROM json_each(d.j) x UNION ALL SELECT d.n) SELECT sum(v) FROM a)"] {
        c.execute("BEGIN",&empty).unwrap();
        c.execute("INSERT INTO output(n) VALUES(0)",&empty).unwrap();
        let sql=format!("INSERT INTO output(n) SELECT {expression} FROM docs d ORDER BY d.n RETURNING n");
        assert_eq!(c.execute(&sql,&empty).unwrap_err().code(),"FDB_VALIDATION");
        assert_eq!(c.transaction_state(),romedb::TransactionState::Active);
        assert_eq!(c.execute("SELECT n FROM output",&empty).unwrap().rows,vec![vec![Value::Integer(0)]]);
        assert!(c.lookup_index("output","output_n",&Value::Integer(2)).unwrap().is_empty());
        let retry=sql.replace("FROM docs d ORDER","FROM docs d WHERE d.n=1 ORDER");
        assert_eq!(c.execute(&retry,&empty).unwrap().rows,vec![vec![Value::Integer(2)]]);
        assert_eq!(c.check_collection_integrity("output",romedb::IntegrityLimits::default()).unwrap().index_entries,2);
        c.execute("ROLLBACK",&empty).unwrap();
        assert_eq!(c.check_collection_integrity("output",romedb::IntegrityLimits::default()).unwrap().documents,0);
    }
}

#[test]
fn correlated_cte_pagination_preserves_bound_limits_and_lazy_errors() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let empty = Parameters::new();
    for sql in [
        "CREATE TABLE native(n INTEGER,j TEXT)",
        "INSERT INTO native VALUES(1,'[1,2,2,null]'),(2,'[]')",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,j) SELECT n,j FROM native",
    ] {
        c.execute(sql, &empty).unwrap();
    }
    for body in [
        "SELECT x.value+d.n AS v FROM json_each(d.j) x ORDER BY v DESC",
        "SELECT DISTINCT x.value+d.n AS v FROM json_each(d.j) x ORDER BY 1 DESC",
        "SELECT x.value+d.n AS v FROM json_each(d.j) x UNION ALL SELECT d.n ORDER BY 1 DESC",
    ] {
        for (limit, offset) in [(0, 0), (1, 0), (2, 1), (-1, 1), (2, 9)] {
            let params = Parameters::from([
                ("$limit".into(), Value::Integer(limit)),
                ("$offset".into(), Value::Integer(offset)),
            ]);
            let query = format!("SELECT d.n,(WITH a AS ({body} LIMIT $limit OFFSET $offset) SELECT sum(v) FROM a) AS total FROM SOURCE d ORDER BY d.n");
            let expected = c
                .execute(&query.replace("SOURCE", "native"), &params)
                .unwrap();
            let sql = query.replace("SOURCE", "docs");
            for actual in [
                c.execute(&sql, &params).unwrap(),
                c.profile_select(&sql, &params).unwrap().result,
            ] {
                assert_eq!(actual.columns, expected.columns, "{sql}: {limit}/{offset}");
                assert_eq!(actual.rows, expected.rows, "{sql}: {limit}/{offset}");
            }
            assert_eq!(c.execute(&sql, &empty).unwrap_err().code(), "FDB_PARAMETER");
        }
    }
    for source in ["native", "docs"] {
        c.execute(&format!("UPDATE {source} SET j='malformed'"), &empty)
            .unwrap();
        let sql = format!("SELECT d.n,(WITH a AS (SELECT x.value+d.n AS v FROM json_each(d.j) x LIMIT $limit) SELECT sum(v) FROM a) AS total FROM {source} d ORDER BY d.n");
        let zero = Parameters::from([("$limit".into(), Value::Integer(0))]);
        let rows = vec![
            vec![Value::Integer(1), Value::Null],
            vec![Value::Integer(2), Value::Null],
        ];
        assert_eq!(c.execute(&sql, &zero).unwrap().rows, rows);
        assert_eq!(c.profile_select(&sql, &zero).unwrap().result.rows, rows);
        let one = Parameters::from([("$limit".into(), Value::Integer(1))]);
        assert_eq!(c.execute(&sql, &one).unwrap_err().code(), "FDB_ENGINE");
        assert_eq!(c.execute(&sql, &zero).unwrap().rows, rows);
    }
}
