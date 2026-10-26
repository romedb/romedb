use romedb::{Database, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
#[test]
fn union_all_preserves_duplicates_types_order_and_nested_sources() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {id:docs:a,n:10,flag:true}");
    q(&c, "INSERT INTO docs {id:docs:b,n:2,flag:false}");
    let baseline = q(&c, "SELECT id,n,flag FROM docs ORDER BY n");
    let rows = q(
        &c,
        "SELECT id,n,flag FROM docs UNION ALL SELECT id,n,flag FROM docs ORDER BY n",
    );
    assert_eq!(rows.columns, baseline.columns);
    assert_eq!(
        rows.rows,
        vec![
            baseline.rows[0].clone(),
            baseline.rows[0].clone(),
            baseline.rows[1].clone(),
            baseline.rows[1].clone()
        ]
    );
    assert_eq!(
        q(
            &c,
            "SELECT n FROM docs UNION ALL SELECT 3 ORDER BY 1 LIMIT 2 OFFSET 1"
        )
        .rows,
        vec![vec![Value::Integer(3)], vec![Value::Integer(10)]]
    );
    assert_eq!(
        q(
            &c,
            "WITH v(x) AS (SELECT n FROM docs UNION ALL SELECT 3) SELECT x FROM v ORDER BY x"
        )
        .rows,
        vec![
            vec![Value::Integer(2)],
            vec![Value::Integer(3)],
            vec![Value::Integer(10)]
        ]
    );
    assert_eq!(
        q(
            &c,
            "SELECT v.x FROM (SELECT n AS x FROM docs UNION ALL SELECT 3) v ORDER BY x"
        )
        .rows,
        vec![
            vec![Value::Integer(2)],
            vec![Value::Integer(3)],
            vec![Value::Integer(10)]
        ]
    );
    q(&c, "CREATE TABLE native(data BLOB)");
    q(&c, "INSERT INTO native VALUES (X'00FF')");
    assert_eq!(
        q(
            &c,
            "SELECT data AS x FROM native UNION ALL SELECT id FROM docs WHERE n=2"
        )
        .rows,
        vec![
            vec![Value::Binary(vec![0, 255])],
            vec![baseline.rows[0][0].clone()]
        ]
    );
    assert_eq!(
        q(
            &c,
            "SELECT id AS x FROM docs WHERE n=2 UNION ALL SELECT data FROM native"
        )
        .rows,
        vec![
            vec![baseline.rows[0][0].clone()],
            vec![Value::Binary(vec![0, 255])]
        ]
    );
    let params = Parameters::from([
        ("$flag".into(), Value::Boolean(true)),
        ("$data".into(), Value::Binary(vec![0, 255])),
    ]);
    assert_eq!(
        c.execute("SELECT $data AS x UNION ALL SELECT $flag", &params)
            .unwrap()
            .rows,
        vec![
            vec![Value::Binary(vec![0, 255])],
            vec![Value::Boolean(true)]
        ]
    );
    assert_eq!(
        q(&c, "VALUES (docs:a) UNION ALL VALUES (docs:b)").rows,
        vec![
            vec![baseline.rows[1][0].clone()],
            vec![baseline.rows[0][0].clone()]
        ]
    );
    assert!(!q(&c, "EXPLAIN SELECT n FROM docs UNION ALL SELECT 3")
        .rows
        .is_empty());
    assert!(!q(
        &c,
        "EXPLAIN QUERY PLAN SELECT n FROM docs UNION ALL SELECT 3"
    )
    .rows
    .is_empty());
    assert_eq!(
        q(
            &c,
            "SELECT '__romedb_harmless' UNION SELECT '__romedb_harmless'"
        )
        .rows
        .len(),
        1
    );
}
#[test]
fn union_all_insert_sources_are_atomic_and_native_conflicts_remain_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE source");
    q(&c, "INSERT INTO source {n:1}");
    q(&c, "CREATE TABLE target");
    q(&c, "CREATE UNIQUE INDEX target_n ON target(n)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO target {n:9}");
    assert!(c
        .execute(
            "INSERT INTO target(n) SELECT n FROM source UNION ALL SELECT n FROM source",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(
        q(&c, "SELECT n FROM target").rows,
        vec![vec![Value::Integer(9)]]
    );
    assert_eq!(
        c.check_collection_integrity("target", Default::default())
            .unwrap()
            .documents,
        1
    );
    assert_eq!(
        q(
            &c,
            "INSERT INTO target(n) SELECT n FROM source UNION ALL SELECT 2 RETURNING n"
        )
        .affected,
        2
    );
    q(&c, "CREATE TABLE native(n INTEGER UNIQUE)");
    assert_eq!(
        q(
            &c,
            "INSERT OR IGNORE INTO native SELECT n FROM source UNION ALL SELECT n FROM source"
        )
        .affected,
        1
    );
    assert_eq!(
        q(&c, "SELECT n FROM native").rows,
        vec![vec![Value::Integer(1)]]
    );
    for sql in [
        "INSERT INTO target(n) SELECT n FROM source UNION ALL SELECT 1,2",
        "INSERT INTO target(n) SELECT n FROM source UNION SELECT 1,2",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert_eq!(
        q(
            &c,
            "INSERT INTO target(n) VALUES (4) UNION ALL SELECT 5 RETURNING n"
        )
        .affected,
        2
    );
    q(&c, "ROLLBACK");
    assert_eq!(
        c.check_collection_integrity("target", Default::default())
            .unwrap()
            .documents,
        0
    );
}

#[test]
fn union_all_scalar_ordering_matches_native_sql_and_errors_preserve_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE baseline(n,t,data)");
    for values in ["(2,'z',X'00')", "(10,'A',X'FF')", "(NULL,NULL,NULL)"] {
        q(&c, &format!("INSERT INTO docs(n,t,data) VALUES {values}"));
        q(&c, &format!("INSERT INTO baseline VALUES {values}"));
    }
    for suffix in [
        "ORDER BY n",
        "ORDER BY n DESC NULLS FIRST",
        "ORDER BY t COLLATE NOCASE,n",
        "ORDER BY 3 DESC,1",
        "ORDER BY n LIMIT 3 OFFSET 1",
    ] {
        let query =
            format!("SELECT n,t,data FROM docs UNION ALL SELECT n,t,data FROM docs {suffix}");
        // The pinned engine rejects COLLATE directly in compound ORDER BY.
        let baseline = if suffix.contains("COLLATE") {
            format!("SELECT * FROM (SELECT n,t,data FROM baseline UNION ALL SELECT n,t,data FROM baseline) {suffix}")
        } else {
            query.replace("docs", "baseline")
        };
        assert_eq!(q(&c, &query).rows, q(&c, &baseline).rows, "{query}");
    }
    let empty=q(&c,"WITH v(x) AS (SELECT n FROM docs WHERE 0 UNION ALL SELECT n FROM docs WHERE 0) SELECT x FROM v");
    assert_eq!(empty.columns, vec!["x"]);
    assert!(empty.rows.is_empty());
    let profile = c
        .profile_select(
            "SELECT n FROM docs UNION ALL SELECT n FROM docs",
            &Parameters::new(),
        )
        .unwrap();
    assert_eq!(profile.result.rows.len(), 6);
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs(n) VALUES (99)");
    for (sql, params) in [
        (
            "INSERT INTO docs(n) SELECT n FROM docs UNION ALL SELECT $missing",
            Parameters::new(),
        ),
        (
            "INSERT INTO docs(n) SELECT n FROM docs UNION ALL SELECT 1",
            Parameters::from([("$unused".into(), Value::Integer(1))]),
        ),
        (
            "INSERT INTO docs(n) SELECT n FROM docs UNION ALL SELECT 1 ORDER BY 2",
            Parameters::new(),
        ),
    ] {
        assert!(c.execute(sql, &params).is_err(), "{sql}");
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .documents,
            4
        );
    }
    q(&c, "ROLLBACK");
}

#[test]
fn union_all_order_names_search_every_arm_left_to_right() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE baseline(a,b)");
    for values in ["(2,10)", "(1,20)"] {
        q(&c, &format!("INSERT INTO docs(a,b) VALUES {values}"));
        q(&c, &format!("INSERT INTO baseline VALUES {values}"));
    }
    for sql in [
        "SELECT a AS left_value,b AS left_other FROM docs UNION ALL SELECT a AS right_value,b AS right_other FROM docs ORDER BY right_other,right_value",
        "SELECT a AS chosen,b AS other FROM docs UNION ALL SELECT a AS other,b AS chosen FROM docs ORDER BY chosen,other",
        "SELECT a,b FROM docs UNION ALL SELECT a AS \"Later Name\",b AS later_b FROM docs ORDER BY \"later name\" DESC,later_b",
        "SELECT a AS first,b FROM docs UNION ALL SELECT a AS second,b FROM docs UNION ALL SELECT a AS third,b FROM docs ORDER BY third,b",
        "WITH v AS (SELECT a AS first,b FROM docs UNION ALL SELECT a AS later,b FROM docs ORDER BY later LIMIT 2) SELECT first,b FROM v ORDER BY first,b",
    ] {
        let actual = q(&c, sql);
        let baseline = q(&c, &sql.replace("docs", "baseline"));
        assert_eq!((actual.columns, actual.rows), (baseline.columns, baseline.rows), "{sql}");
    }
}

#[test]
fn union_all_parameters_keep_statement_positions_and_binary_identity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let record = Value::Record(romedb::Record {
        table: "docs".into(),
        key: romedb::Key::Integer(7),
    });
    let params = Parameters::from([
        ("?1".into(), record.clone()),
        ("?2".into(), Value::Boolean(true)),
        ("?3".into(), Value::Integer(2)),
    ]);
    assert_eq!(
        c.execute("SELECT ? AS x UNION ALL SELECT ? LIMIT ?", &params)
            .unwrap()
            .rows,
        vec![vec![record], vec![Value::Boolean(true)]]
    );
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE INDEX docs_data ON docs(data)");
    q(&c, "CREATE TABLE native(data BLOB)");
    let bytes = Value::Binary(b"FDB\x01{\"type\":\"Integer\",\"value\":7}".to_vec());
    let params = Parameters::from([("$data".into(), bytes.clone())]);
    c.execute("INSERT INTO docs(data) VALUES ($data)", &params)
        .unwrap();
    c.execute("INSERT INTO native VALUES ($data)", &params)
        .unwrap();
    for sql in [
        "SELECT data FROM docs WHERE data=$data UNION ALL SELECT data FROM native WHERE data=$data",
        "SELECT data FROM native WHERE data=$data UNION ALL SELECT data FROM docs WHERE data=$data",
        "WITH v(x) AS (VALUES ($data)) SELECT x FROM v UNION ALL SELECT data FROM docs WHERE data=$data",
    ] {
        assert_eq!(c.execute(sql,&params).unwrap().rows,vec![vec![bytes.clone()],vec![bytes.clone()]],"{sql}");
    }
    q(&c, "CREATE TABLE copied");
    q(&c, "CREATE TABLE native_copy(data BLOB)");
    let source =
        "SELECT data FROM docs WHERE data=$data UNION ALL SELECT data FROM native WHERE data=$data";
    q(&c, "BEGIN");
    for target in ["copied", "native_copy"] {
        let sql = format!("INSERT INTO {target}(data) {source}");
        assert_eq!(c.execute(&sql, &params).unwrap().affected, 2);
        assert_eq!(
            q(&c, &format!("SELECT data FROM {target}")).rows,
            vec![vec![bytes.clone()], vec![bytes.clone()]]
        );
    }
    q(&c, "ROLLBACK");
    q(&c, "CREATE UNIQUE INDEX copied_data ON copied(data)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO copied(data) VALUES (X'31')");
    assert!(c
        .execute(&format!("INSERT INTO copied(data) {source}"), &params)
        .is_err());
    assert_eq!(
        q(&c, "SELECT data FROM copied").rows,
        vec![vec![Value::Binary(vec![49])]]
    );
    assert!(c
        .lookup_index("copied", "copied_data", &bytes)
        .unwrap()
        .is_empty());
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT data FROM copied").rows.is_empty());
    assert!(q(&c, "SELECT data FROM native_copy").rows.is_empty());
}

#[test]
fn set_operators_use_logical_scalar_equality_and_left_association() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for table in ["a", "b"] {
        q(&c, &format!("CREATE TABLE {table}"));
    }
    q(&c, "CREATE TABLE na(v)");
    q(&c, "CREATE TABLE nb(v)");
    for (table, native, values) in [
        ("a", "na", vec!["1", "1", "2", "NULL", "'A'", "X'00FF'"]),
        ("b", "nb", vec!["1", "3", "NULL", "'a'", "X'00FF'"]),
    ] {
        for value in values {
            q(&c, &format!("INSERT INTO {table}(v) VALUES ({value})"));
            q(&c, &format!("INSERT INTO {native} VALUES ({value})"));
        }
    }
    for op in ["UNION", "INTERSECT", "EXCEPT"] {
        let sql = format!("SELECT v FROM a {op} SELECT v FROM b ORDER BY 1");
        let native = format!("SELECT v FROM na {op} SELECT v FROM nb ORDER BY 1");
        assert_eq!(q(&c, &sql).rows, q(&c, &native).rows, "{sql}");
        let sql =
            format!("SELECT v FROM a {op} SELECT v FROM b UNION ALL SELECT v FROM a ORDER BY 1");
        let native =
            format!("SELECT v FROM na {op} SELECT v FROM nb UNION ALL SELECT v FROM na ORDER BY 1");
        assert_eq!(q(&c, &sql).rows, q(&c, &native).rows, "{sql}");
    }
    for (op, count) in [("UNION", 1), ("INTERSECT", 1), ("EXCEPT", 0)] {
        let params = Parameters::from([
            ("$left".into(), Value::Integer(1)),
            ("$right".into(), Value::Number(1.0)),
        ]);
        assert_eq!(
            c.execute(
                &format!("SELECT $left AS v FROM a WHERE a.v=2 {op} SELECT $right"),
                &params
            )
            .unwrap()
            .rows
            .len(),
            count
        );
    }
    for tail in [
        "UNION SELECT v FROM b",
        "INTERSECT SELECT v FROM b",
        "EXCEPT SELECT v FROM b",
        "UNION SELECT v FROM b INTERSECT SELECT v FROM a",
    ] {
        let sql = format!("SELECT v FROM a UNION ALL SELECT v FROM a {tail} ORDER BY 1");
        let native = sql
            .replace("FROM a", "FROM na")
            .replace("FROM b", "FROM nb");
        assert_eq!(q(&c, &sql).rows, q(&c, &native).rows, "{sql}");
    }
    q(&c, "CREATE TABLE copied");
    assert_eq!(q(&c,"WITH v AS (SELECT v FROM a UNION SELECT v FROM b) INSERT INTO copied(v) SELECT v FROM v").affected,7);
}

#[test]
fn set_operators_preserve_record_binary_identity_and_collation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let record = Value::Record(romedb::Record {
        table: "Docs".into(),
        key: romedb::Key::Integer(7),
    });
    let same = Value::Record(romedb::Record {
        table: "docs".into(),
        key: romedb::Key::Integer(7),
    });
    let bytes = Value::Binary(
        b"FDB\x01{\"type\":\"Record\",\"value\":{\"table\":\"docs\",\"key\":{\"Integer\":7}}}"
            .to_vec(),
    );
    for (right, union, intersection, difference) in [(same, 1, 1, 0), (bytes, 2, 0, 1)] {
        let params = Parameters::from([("$left".into(), record.clone()), ("$right".into(), right)]);
        for (op, count) in [
            ("UNION", union),
            ("INTERSECT", intersection),
            ("EXCEPT", difference),
        ] {
            let rows = c
                .execute(&format!("SELECT $left AS v {op} SELECT $right"), &params)
                .unwrap()
                .rows;
            assert_eq!(rows.len(), count, "{op}");
            assert!(rows
                .iter()
                .all(|row| matches!(row[0], Value::Record(_) | Value::Binary(_))));
        }
    }
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(v) VALUES ('A'),('a')");
    q(&c, "CREATE TABLE native(v TEXT COLLATE NOCASE)");
    q(&c, "INSERT INTO native VALUES ('A'),('a')");
    for (op, count) in [("UNION", 1), ("INTERSECT", 1), ("EXCEPT", 0)] {
        for (left, right) in [
            ("v COLLATE NOCASE FROM docs", "v FROM docs"),
            ("v FROM native", "v FROM docs"),
            ("v FROM docs", "v FROM native"),
        ] {
            let sql = format!("SELECT {left} {op} SELECT {right}");
            assert_eq!(q(&c, &sql).rows.len(), count, "{sql}");
        }
    }
}

#[test]
fn set_insert_sources_validate_and_restore_indexes_atomically() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE UNIQUE INDEX docs_n ON docs(n)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs(n) VALUES (9)");
    for op in ["UNION", "INTERSECT", "EXCEPT"] {
        let source = match op {
            "UNION" => "VALUES (1),(2) UNION VALUES (2),(9)",
            "INTERSECT" => "VALUES (1),(9) INTERSECT VALUES (1),(9)",
            _ => "VALUES (1),(9),(10) EXCEPT VALUES (10)",
        };
        assert!(c
            .execute(&format!("INSERT INTO docs(n) {source}"), &Parameters::new())
            .is_err());
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(
            q(&c, "SELECT n FROM docs").rows,
            vec![vec![Value::Integer(9)]]
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .documents,
            1
        );
    }
    assert_eq!(
        q(
            &c,
            "INSERT INTO docs(n) VALUES (1),(2) UNION VALUES (2),(3) RETURNING n"
        )
        .affected,
        3
    );
    q(&c, "CREATE TABLE native(n INTEGER UNIQUE)");
    assert_eq!(
        q(
            &c,
            "INSERT INTO native SELECT n FROM docs UNION SELECT n FROM docs"
        )
        .affected,
        4
    );
    assert_eq!(
        q(
            &c,
            "INSERT OR IGNORE INTO native SELECT n FROM docs INTERSECT SELECT n FROM docs"
        )
        .affected,
        0
    );
    let arrays = Parameters::from([("$array".into(), Value::Array(vec![Value::Integer(1)]))]);
    let report = c.execute_report(
        "INSERT INTO docs(n) SELECT $array UNION SELECT $array",
        &arrays,
    );
    assert!(report.result.is_err());
    assert_eq!(report.transaction_before, romedb::TransactionState::Active);
    // Failed source evaluation preserves the caller transaction and prior writes.
    assert_eq!(report.transaction_after, romedb::TransactionState::Active);
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .documents,
        4
    );
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT n FROM docs").rows.is_empty());
}

#[test]
fn set_operators_compare_whole_rows_and_keep_empty_derived_metadata() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for (table, native, rows) in [
        ("a", "na", vec!["(1,NULL)", "(1,2)", "(1,2)", "(NULL,2)"]),
        ("b", "nb", vec!["(1,NULL)", "(NULL,2)", "(3,3)"]),
    ] {
        q(&c, &format!("CREATE TABLE {table}"));
        q(&c, &format!("CREATE TABLE {native}(v,k)"));
        for row in rows {
            q(&c, &format!("INSERT INTO {table}(v,k) VALUES {row}"));
            q(&c, &format!("INSERT INTO {native} VALUES {row}"));
        }
    }
    for op in ["UNION", "INTERSECT", "EXCEPT"] {
        let sql = format!("SELECT v,k FROM a {op} SELECT v,k FROM b ORDER BY 1,2 LIMIT 3");
        let native = sql
            .replace("FROM a", "FROM na")
            .replace("FROM b", "FROM nb");
        assert_eq!(q(&c, &sql).rows, q(&c, &native).rows, "{sql}");
        let empty = q(
            &c,
            &format!(
                "SELECT d.* FROM (SELECT v,k FROM a WHERE 0 {op} SELECT v,k FROM b WHERE 0) d"
            ),
        );
        assert_eq!(empty.columns, vec!["v", "k"]);
        assert!(empty.rows.is_empty());
    }
}

#[test]
fn set_collation_precedence_and_representatives_match_native_keys() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for (table, native, values) in [
        ("a", "na", vec!["'A'", "'a'", "'B '", "'b'", "'x'", "NULL"]),
        ("b", "nb", vec!["'a'", "'A '", "'b'", "'B'", "'y'", "NULL"]),
    ] {
        q(&c, &format!("CREATE TABLE {table}"));
        q(&c, &format!("CREATE TABLE {native}(v)"));
        for value in values {
            q(&c, &format!("INSERT INTO {table}(v) VALUES ({value})"));
            q(&c, &format!("INSERT INTO {native} VALUES ({value})"));
        }
    }
    let keys = |rows: Vec<Vec<Value>>, collation: &str| {
        let mut keys = rows
            .into_iter()
            .map(|row| match row.as_slice() {
                [Value::Null] => "null:".to_owned(),
                [Value::String(value)] => format!(
                    "text:{}",
                    match collation {
                        "NOCASE" => value.to_ascii_lowercase(),
                        "RTRIM" => value.trim_end_matches(' ').to_owned(),
                        _ => value.clone(),
                    }
                ),
                _ => panic!("unexpected row: {row:?}"),
            })
            .collect::<Vec<_>>();
        keys.sort();
        keys
    };
    for left in ["", "BINARY", "NOCASE", "RTRIM"] {
        for right in ["", "BINARY", "NOCASE", "RTRIM"] {
            let l = if left.is_empty() {
                "v".to_owned()
            } else {
                format!("v COLLATE {left}")
            };
            let r = if right.is_empty() {
                "v".to_owned()
            } else {
                format!("v COLLATE {right}")
            };
            let collation = if left.is_empty() { right } else { left };
            for op in ["UNION", "INTERSECT", "EXCEPT"] {
                let sql = format!("SELECT {l} AS value FROM a {op} SELECT {r} FROM b");
                let native = sql
                    .replace("FROM a", "FROM na")
                    .replace("FROM b", "FROM nb");
                assert_eq!(
                    keys(q(&c, &sql).rows, collation),
                    keys(q(&c, &native).rows, collation),
                    "{sql}"
                );
            }
        }
    }
    for collation in ["", "BINARY", "NOCASE", "RTRIM"] {
        let value = if collation.is_empty() {
            "v".to_owned()
        } else {
            format!("v COLLATE {collation}")
        };
        for first in ["UNION ALL", "UNION", "INTERSECT", "EXCEPT"] {
            for second in ["UNION ALL", "UNION", "INTERSECT", "EXCEPT"] {
                let sql = format!("SELECT {value} AS value FROM a {first} SELECT {value} FROM b {second} SELECT {value} FROM a");
                let native = sql
                    .replace("FROM a", "FROM na")
                    .replace("FROM b", "FROM nb");
                assert_eq!(
                    keys(q(&c, &sql).rows, collation),
                    keys(q(&c, &native).rows, collation),
                    "{sql}"
                );
            }
        }
    }
}

#[test]
fn direct_union_pagination_preserves_typed_derived_and_insert_rows() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE sink");
    let source =
        "SELECT $left AS payload,1 AS marker UNION ALL SELECT $right,2 LIMIT $take OFFSET $skip";
    for value in [
        Value::Null,
        Value::Boolean(true),
        Value::Integer(i64::MAX),
        Value::String("text".into()),
        Value::Binary(b"FDB\x01{\"type\":\"Integer\",\"value\":7}".to_vec()),
        Value::Record(romedb::Record {
            table: "sink".into(),
            key: romedb::Key::Integer(7),
        }),
        Value::Array(vec![Value::Boolean(false), Value::Integer(9)]),
    ] {
        for (take, skip) in [(1, 0), (1, 1), (0, 0)] {
            let params = Parameters::from([
                ("$left".into(), Value::Boolean(false)),
                ("$right".into(), value.clone()),
                ("$take".into(), Value::Integer(take)),
                ("$skip".into(), Value::Integer(skip)),
            ]);
            let expected = if take == 0 {
                vec![]
            } else {
                vec![vec![
                    if skip == 0 {
                        Value::Boolean(false)
                    } else {
                        value.clone()
                    },
                    Value::Integer(skip + 1),
                ]]
            };
            for sql in [
                source.to_owned(),
                format!("SELECT u.payload,u.marker FROM ({source}) u"),
                format!("WITH u(payload,marker) AS ({source}) SELECT payload,marker FROM u"),
            ] {
                let result = c
                    .execute(&sql, &params)
                    .unwrap_or_else(|error| panic!("{sql}: {error}"));
                assert_eq!(result.columns, vec!["payload", "marker"]);
                assert_eq!(result.rows, expected, "{sql}");
            }
            let inserted = c
                .execute(
                    &format!("INSERT INTO sink(payload,marker) {source} RETURNING payload,marker"),
                    &params,
                )
                .unwrap();
            assert_eq!(inserted.rows, expected);
            assert_eq!(q(&c, "SELECT payload,marker FROM sink").rows, expected);
            q(&c, "DELETE FROM sink");
        }
    }
}

#[test]
fn pinned_correlated_unordered_union_pagination_is_not_materialization_equivalent() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE a(k INTEGER)");
    q(&c, "INSERT INTO a VALUES(1),(2)");
    let run = |rhs: &str| {
        q(&c, &format!("SELECT k,(SELECT 1 IN({rhs})) AS found,(SELECT 1 NOT IN({rhs})) AS absent FROM a ORDER BY k"))
    };
    let direct = run("SELECT k UNION SELECT NULL LIMIT 1 OFFSET 1");
    let materialized = run(
        "WITH q(v) AS MATERIALIZED(SELECT k UNION SELECT NULL) SELECT v FROM q LIMIT 1 OFFSET 1",
    );
    assert_eq!(direct.columns, materialized.columns);
    assert_eq!(
        direct.rows,
        vec![
            vec![Value::Integer(1), Value::Integer(1), Value::Integer(0)],
            vec![Value::Integer(2), Value::Null, Value::Null],
        ]
    );
    assert_eq!(
        materialized.rows,
        vec![
            vec![Value::Integer(1), Value::Integer(1), Value::Integer(0)],
            vec![Value::Integer(2), Value::Integer(0), Value::Integer(1)],
        ]
    );
}

#[test]
fn composite_membership_preserves_prior_work_and_allows_retry() {
    for composite in [
        Value::Array(vec![Value::Integer(1)]),
        Value::Object(std::collections::BTreeMap::from([(
            "n".into(),
            Value::Integer(1),
        )])),
    ] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE docs",
            "CREATE UNIQUE INDEX docs_n ON docs(n)",
            "CREATE TABLE probe(n INTEGER)",
            "INSERT INTO probe VALUES(1)",
            "CREATE TABLE sink",
            "CREATE UNIQUE INDEX sink_n ON sink(n)",
            "INSERT INTO docs(n,k) VALUES(1,1)",
        ] {
            q(&c, sql);
        }
        c.execute(
            "INSERT INTO docs(n,k) VALUES(2,$value)",
            &Parameters::from([("$value".into(), composite.clone())]),
        )
        .unwrap();
        q(&c, "BEGIN");
        q(&c, "INSERT INTO sink(n) VALUES(9)");
        let insert = "INSERT INTO sink(n) SELECT d.n FROM docs d WHERE (SELECT count(*) FROM probe WHERE d.k IN(SELECT d.k INTERSECT SELECT 1 LIMIT 1))=1 ORDER BY d.n RETURNING n";
        let error = c
            .write_with_result_limits(
                insert,
                &Parameters::new(),
                romedb::ResultLimits {
                    max_rows: 0,
                    max_payload_bytes: 1024,
                },
            )
            .unwrap_err();
        assert!(matches!(error, romedb::Error::Limit(_)), "{error}");
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(
            q(&c, "SELECT n FROM sink").rows,
            vec![vec![Value::Integer(9)]]
        );
        assert_eq!(q(&c, insert).rows, vec![vec![Value::Integer(1)]]);
        q(&c, "ROLLBACK");
        assert!(q(&c, "SELECT n FROM sink").rows.is_empty());
        assert_eq!(
            q(&c, "SELECT k FROM docs WHERE n=2").rows,
            vec![vec![composite.clone()]]
        );
        for table in ["docs", "sink"] {
            c.check_collection_integrity(table, Default::default())
                .unwrap();
        }
        q(&c, "BEGIN");
        q(&c, "UPDATE docs SET k=1 WHERE n=2");
        assert_eq!(
            q(&c, insert).rows,
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        );
        c.check_collection_integrity("sink", Default::default())
            .unwrap();
        q(&c, "ROLLBACK");
        assert!(q(&c, "SELECT n FROM sink").rows.is_empty());
        assert_eq!(
            q(&c, "SELECT k FROM docs WHERE n=2").rows,
            vec![vec![composite]]
        );
    }
}

#[test]
fn composite_sets_and_distinct_use_typed_identity_without_scalar_coercion_changes() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    let one = Value::Object(
        [(
            "values".into(),
            Value::Array(vec![
                Value::Integer(1),
                Value::Number(-0.0),
                Value::Record(romedb::Record {
                    table: "DOCS".into(),
                    key: romedb::Key::String("a".into()),
                }),
            ]),
        )]
        .into(),
    );
    let equivalent = Value::Object(
        [(
            "values".into(),
            Value::Array(vec![
                Value::Integer(1),
                Value::Number(0.0),
                Value::Record(romedb::Record {
                    table: "docs".into(),
                    key: romedb::Key::String("a".into()),
                }),
            ]),
        )]
        .into(),
    );
    let distinct = Value::Object(
        [(
            "values".into(),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(0.0),
                Value::Record(romedb::Record {
                    table: "docs".into(),
                    key: romedb::Key::String("a".into()),
                }),
            ]),
        )]
        .into(),
    );
    for (n, value) in [
        (1, one.clone()),
        (2, equivalent.clone()),
        (3, distinct.clone()),
    ] {
        c.execute(
            "INSERT INTO docs {n:$n,v:$v}",
            &Parameters::from([("$n".into(), Value::Integer(n)), ("$v".into(), value)]),
        )
        .unwrap();
    }
    let params = Parameters::from([
        ("$one".into(), one),
        ("$same".into(), equivalent),
        ("$different".into(), distinct),
    ]);
    let run = |sql: &str| {
        c.execute(
            sql,
            &params
                .iter()
                .filter(|(name, _)| sql.contains(name.as_str()))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        )
    };
    assert_eq!(
        run("SELECT $one AS v UNION SELECT $same")
            .unwrap()
            .rows
            .len(),
        1
    );
    assert_eq!(
        run("SELECT $one AS v UNION SELECT $different")
            .unwrap()
            .rows
            .len(),
        2
    );
    assert_eq!(
        run("SELECT $one AS v INTERSECT SELECT $same")
            .unwrap()
            .rows
            .len(),
        1
    );
    assert!(run("SELECT $one AS v EXCEPT SELECT $same")
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(q(&c, "SELECT DISTINCT v FROM docs").rows.len(), 2);
    assert_eq!(
        q(
            &c,
            "WITH keys AS (SELECT v FROM docs UNION SELECT v FROM docs) SELECT count(*) FROM keys"
        )
        .rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        q(&c, "SELECT v FROM docs UNION ALL SELECT v FROM docs")
            .rows
            .len(),
        6
    );
    assert_eq!(
        q(&c, "SELECT 1 UNION SELECT 1.0 UNION SELECT true")
            .rows
            .len(),
        1
    );
    assert_eq!(
        q(&c, "SELECT 'A' COLLATE NOCASE UNION SELECT 'a'")
            .rows
            .len(),
        1
    );
    for sql in [
        "SELECT v FROM docs ORDER BY v",
        "SELECT DISTINCT v FROM docs ORDER BY v",
        "SELECT v+1 FROM docs",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
}

#[test]
fn composite_membership_retains_sql_null_truth_and_binary_tag_separation() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    let array = Value::Array(vec![Value::Integer(1), Value::Null]);
    let object = Value::Object([("x".into(), Value::Boolean(true))].into());
    let vector = q(&c, "SELECT vector32('[1,0]')").rows[0][0].clone();
    for (n,value) in [(1,array.clone()),(2,object.clone()),(3,Value::Null),(4,Value::Binary(b"FDB\x01{\"type\":\"Array\",\"value\":[{\"type\":\"Integer\",\"value\":1},{\"type\":\"Null\"}]}".to_vec())),(5,vector.clone())] {c.execute("INSERT INTO docs {n:$n,v:$value}",&Parameters::from([("$n".into(),Value::Integer(n)),("$value".into(),value)])).unwrap();}
    let params = Parameters::from([
        ("$array".into(), array),
        ("$object".into(), object),
        ("$vector".into(), vector),
    ]);
    let run = |sql: &str| {
        c.execute(
            sql,
            &params
                .iter()
                .filter(|(name, _)| sql.contains(name.as_str()))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        )
    };
    assert_eq!(
        run("SELECT n FROM docs WHERE v IN($array,$object,$vector) ORDER BY n")
            .unwrap()
            .rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(5)]
        ]
    );
    assert_eq!(
        run("SELECT n FROM docs WHERE v NOT IN($array,NULL) ORDER BY n")
            .unwrap()
            .rows
            .len(),
        0
    );
    assert_eq!(
        run("SELECT v IN($array,NULL),v NOT IN($array,NULL) FROM docs ORDER BY n")
            .unwrap()
            .rows,
        vec![
            vec![Value::Integer(1), Value::Integer(0)],
            vec![Value::Null, Value::Null],
            vec![Value::Null, Value::Null],
            vec![Value::Null, Value::Null],
            vec![Value::Null, Value::Null]
        ]
    );
    assert_eq!(
        q(
            &c,
            "SELECT n FROM docs WHERE v IN(SELECT v FROM docs WHERE n=1) ORDER BY n"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(q(&c,"SELECT n FROM docs WHERE v IN(SELECT v FROM docs WHERE n=2 UNION SELECT v FROM docs WHERE n=5) ORDER BY n").rows,vec![vec![Value::Integer(2)],vec![Value::Integer(5)]]);
    q(&c, "CREATE INDEX docs_n ON docs(n)");
    assert!(run("SELECT n FROM docs WHERE n IN($array)")
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(
        run("SELECT n FROM docs WHERE n IN(1,$array) ORDER BY n")
            .unwrap()
            .rows,
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(
        run("SELECT n FROM docs NOT INDEXED WHERE n IN(1,$array) ORDER BY n")
            .unwrap()
            .rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn typed_row_membership_preserves_components_nulls_and_set_identity() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    for sql in [
        "INSERT INTO docs {id:docs:a,n:1,k:[1,2]}",
        "INSERT INTO docs {id:docs:b,n:2,k:{a:1}}",
        "INSERT INTO docs {id:docs:c,n:3,k:null}",
    ] {
        q(&c, sql);
    }
    for rhs in [
        "SELECT n,k FROM docs WHERE n<3",
        "SELECT n,k FROM docs WHERE n<3 UNION SELECT n,k FROM docs WHERE n=1",
        "WITH members AS (SELECT n,k FROM docs WHERE n<3) SELECT n,k FROM members",
    ] {
        assert_eq!(q(&c, &format!("SELECT n,(n,k) IN({rhs}) AS found,(n,k) NOT IN({rhs}) AS absent FROM docs ORDER BY n")).rows, vec![
            vec![Value::Integer(1),Value::Integer(1),Value::Integer(0)],
            vec![Value::Integer(2),Value::Integer(1),Value::Integer(0)],
            vec![Value::Integer(3),Value::Integer(0),Value::Integer(1)],
        ], "{rhs}");
    }
    assert_eq!(
        q(
            &c,
            "SELECT n,(n,k) IN(SELECT n,k FROM docs) FROM docs ORDER BY n"
        )
        .rows,
        vec![
            vec![Value::Integer(1), Value::Integer(1)],
            vec![Value::Integer(2), Value::Integer(1)],
            vec![Value::Integer(3), Value::Null],
        ]
    );
    assert_eq!(q(&c, "SELECT d.n FROM docs d WHERE (d.n,d.k) IN(SELECT p.n,p.k FROM docs p WHERE p.n=d.n) ORDER BY d.n").rows, vec![vec![Value::Integer(1)],vec![Value::Integer(2)]]);
    q(&c, "CREATE TABLE native(n INTEGER,k TEXT)");
    q(&c, "INSERT INTO native VALUES(1,'one'),(2,'two')");
    assert_eq!(
        q(
            &c,
            "SELECT n FROM native WHERE (n,k) IN(SELECT n,k FROM native WHERE n=1)"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    let params = Parameters::from([
        (
            "$a".into(),
            Value::Array(vec![Value::Integer(1), Value::Integer(2)]),
        ),
        (
            "$b".into(),
            Value::Object(std::collections::BTreeMap::from([(
                "a".into(),
                Value::Integer(1),
            )])),
        ),
    ]);
    assert_eq!(
        c.execute(
            "SELECT n FROM docs WHERE (n,k) IN((1,$a),(2,$b)) ORDER BY n",
            &params
        )
        .unwrap()
        .rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT n FROM docs WHERE (n,id) IN(SELECT n,id FROM docs WHERE n=1)"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT n FROM docs WHERE (n,id) IN((1,docs:a),(2,docs:a))"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT n FROM docs WHERE (n,k) IN(SELECT n,k FROM docs WHERE 0)"
        )
        .rows,
        Vec::<Vec<Value>>::new()
    );
}

#[test]
fn typed_row_membership_rejects_unsupported_affinity_and_widths() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {n:1,k:[1]}");
    q(&c, "CREATE TABLE native(n INTEGER,k BLOB)");
    q(&c, "INSERT INTO native VALUES(1,X'01')");
    for sql in [
        "SELECT (n,k) IN(SELECT n FROM docs) FROM docs",
        "SELECT (n,k) IN(SELECT n,k,id FROM docs) FROM docs",
        "SELECT (n,k) IN(SELECT CAST(n AS INTEGER),k FROM docs) FROM docs",
        "SELECT (native.n,native.k) IN(SELECT n,k FROM docs) FROM native JOIN docs",
    ] {
        assert_eq!(
            c.execute(sql, &Parameters::new()).unwrap_err().code(),
            "FDB_UNSUPPORTED",
            "{sql}"
        );
    }
}
