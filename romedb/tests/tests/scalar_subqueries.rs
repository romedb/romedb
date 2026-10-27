use romedb::{Database, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}

#[test]
fn collection_scalar_subqueries_preserve_values_and_empty_nulls() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(
        &c,
        "INSERT INTO docs {id:docs:a,n:1,link:docs:b,items:[1,true],meta:{ok:true}}",
    );
    q(&c, "INSERT INTO docs {id:docs:b,n:2}");
    for field in ["link", "items", "meta", "n"] {
        assert_eq!(
            q(
                &c,
                &format!("SELECT (SELECT {field} FROM docs WHERE id=docs:a) AS v")
            )
            .rows,
            q(
                &c,
                &format!("SELECT {field} AS v FROM docs WHERE id=docs:a")
            )
            .rows
        );
    }
    assert_eq!(
        q(&c, "SELECT (SELECT n FROM docs WHERE n=99) AS v").rows,
        vec![vec![Value::Null]]
    );
    assert_eq!(
        q(&c, "SELECT (SELECT n FROM docs ORDER BY n DESC) AS v").rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        q(&c, "SELECT n FROM docs WHERE n=(SELECT max(n) FROM docs)").rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        q(&c, "SELECT (SELECT n FROM docs ORDER BY n LIMIT 1)+10 AS v").rows,
        vec![vec![Value::Integer(11)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT (SELECT (SELECT n FROM docs ORDER BY n LIMIT 1) AS v FROM docs LIMIT 1) AS v"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    let binary = Value::Binary(vec![0x46, 0x44, 0x42, 0, 1, 2]);
    let projected = c
        .execute(
            "SELECT (SELECT $v FROM docs LIMIT 1) AS v",
            &Parameters::from([("$v".into(), binary.clone())]),
        )
        .unwrap();
    assert_eq!(projected.rows, vec![vec![binary]]);
    assert_eq!(
        q(
            &c,
            "SELECT d.n,(SELECT x.n FROM docs x WHERE x.n=d.n) AS v FROM docs d ORDER BY d.n"
        )
        .rows,
        vec![
            vec![Value::Integer(1), Value::Integer(1)],
            vec![Value::Integer(2), Value::Integer(2)]
        ]
    );
    assert!(c
        .execute("SELECT (SELECT n,link FROM docs) AS v", &Parameters::new())
        .is_err());
}

#[test]
fn scalar_subqueries_share_parameters_ctes_and_atomic_insert_sources() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {n:1}");
    q(&c, "INSERT INTO docs {n:2}");
    let result = c
        .execute(
            "SELECT (SELECT n FROM docs WHERE n=$n) AS v",
            &Parameters::from([("$n".into(), Value::Integer(2))]),
        )
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Integer(2)]]);
    assert_eq!(
        q(
            &c,
            "WITH chosen AS (SELECT n FROM docs WHERE n=2) SELECT (SELECT n FROM chosen) AS v"
        )
        .rows,
        result.rows
    );
    q(&c, "CREATE TABLE copies");
    q(&c, "DEFINE FIELD n ON copies TYPE integer REQUIRED");
    q(
        &c,
        "INSERT INTO copies(n) SELECT (SELECT max(n) FROM docs) AS v",
    );
    assert_eq!(q(&c, "SELECT n FROM copies").rows, result.rows);
    assert!(c
        .execute(
            "INSERT INTO copies(n) SELECT (SELECT n FROM docs WHERE n=99) AS v",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(
        c.check_collection_integrity("copies", Default::default())
            .unwrap()
            .documents,
        1
    );
    q(&c, "CREATE TABLE native(n INTEGER)");
    q(
        &c,
        "INSERT INTO native SELECT (SELECT max(n) FROM docs) AS v",
    );
    assert_eq!(q(&c, "SELECT n FROM native").rows, result.rows);
}

#[test]
fn collection_exists_matches_native_membership_and_insert_sources() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {n:1}");
    q(&c, "INSERT INTO docs {n:2}");
    q(&c, "CREATE TABLE native(n INTEGER)");
    q(&c, "INSERT INTO native VALUES (1),(2)");
    for inner in [
        "SELECT n FROM SOURCE",
        "SELECT n,n FROM SOURCE WHERE n=99",
        "SELECT n,n FROM SOURCE WHERE n=2",
        "SELECT count(*) FROM SOURCE WHERE n=99",
        "SELECT n FROM SOURCE LIMIT 0",
        "SELECT n FROM SOURCE LIMIT 1 OFFSET 2",
        "SELECT n FROM SOURCE GROUP BY n HAVING n>1",
    ] {
        for prefix in ["", "NOT "] {
            let logical = format!(
                "SELECT {prefix}EXISTS ({}) AS v",
                inner.replace("SOURCE", "docs")
            );
            let native = format!(
                "SELECT {prefix}EXISTS ({}) AS v",
                inner.replace("SOURCE", "native")
            );
            assert_eq!(q(&c, &logical).rows, q(&c, &native).rows, "{logical}");
        }
    }
    assert_eq!(
        q(
            &c,
            "SELECT n FROM docs WHERE EXISTS (SELECT n FROM docs WHERE n=2) ORDER BY n"
        )
        .rows,
        q(&c, "SELECT n FROM native ORDER BY n").rows
    );
    assert!(q(
        &c,
        "SELECT n FROM docs WHERE NOT EXISTS (SELECT n FROM docs)"
    )
    .rows
    .is_empty());
    assert_eq!(
        q(&c, "SELECT EXISTS (SELECT * FROM docs) AS v").rows,
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT EXISTS (SELECT n FROM docs UNION SELECT n FROM docs) AS v"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    let result = c
        .execute(
            "WITH v AS (SELECT n FROM docs WHERE n=$n) SELECT EXISTS (SELECT n FROM v) AS v",
            &Parameters::from([("$n".into(), Value::Integer(2))]),
        )
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Integer(1)]]);
    q(&c, "CREATE TABLE copies");
    q(
        &c,
        "INSERT INTO copies(n) SELECT EXISTS (SELECT n FROM docs) AS n",
    );
    assert_eq!(q(&c, "SELECT n FROM copies").rows, result.rows);
    q(
        &c,
        "INSERT INTO native SELECT EXISTS (SELECT n FROM docs WHERE n=99) AS n",
    );
    assert_eq!(
        q(&c, "SELECT n FROM native WHERE n=0").rows,
        vec![vec![Value::Integer(0)]]
    );
}

#[test]
fn collection_membership_subqueries_match_native_null_and_scalar_semantics() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {n:1}");
    q(&c, "INSERT INTO docs {n:2}");
    q(&c, "INSERT INTO docs {n:null}");
    q(&c, "CREATE TABLE native(n)");
    q(&c, "INSERT INTO native VALUES (1),(2),(NULL)");
    for lhs in ["1", "3", "NULL", "'1'", "1.0"] {
        for predicate in ["1", "n IS NOT NULL", "n=99"] {
            for negate in ["", "NOT "] {
                let sql =
                    format!("SELECT {lhs} {negate}IN (SELECT n FROM docs WHERE {predicate}) AS v");
                assert_eq!(
                    q(&c, &sql).rows,
                    q(&c, &sql.replace("FROM docs", "FROM native")).rows,
                    "{sql}"
                );
            }
        }
    }
    assert_eq!(
        q(
            &c,
            "SELECT n FROM docs WHERE n IN (SELECT n FROM docs WHERE n=2)"
        )
        .rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT (SELECT max(n) FROM docs) IN (SELECT n FROM docs) AS v"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    q(&c, "CREATE TABLE copies");
    q(
        &c,
        "INSERT INTO copies(n) SELECT n FROM docs WHERE n NOT IN (SELECT n FROM docs WHERE n=1)",
    );
    assert_eq!(
        q(&c, "SELECT n FROM copies").rows,
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn membership_subqueries_preserve_record_binary_keys_and_native_affinity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {id:docs:a,link:docs:a,n:'1'}");
    q(&c, "INSERT INTO docs {id:docs:b,link:docs:1,n:'2'}");
    assert_eq!(q(&c, "SELECT docs:a IN (SELECT link FROM docs) AS a,type::record('docs','1') IN (SELECT link FROM docs) AS b").rows, vec![vec![Value::Integer(1), Value::Integer(0)]]);
    let binary = Value::Binary(
        b"FDB\x01{\"type\":\"Record\",\"value\":{\"table\":\"docs\",\"key\":{\"String\":\"a\"}}}"
            .to_vec(),
    );
    let result = c
        .execute(
            "SELECT $v IN (SELECT link FROM docs) AS v",
            &Parameters::from([("$v".into(), binary.clone())]),
        )
        .unwrap();
    assert_eq!(result.rows, vec![vec![Value::Integer(0)]]);
    c.execute(
        "INSERT INTO docs(link) VALUES ($v)",
        &Parameters::from([("$v".into(), binary.clone())]),
    )
    .unwrap();
    assert_eq!(
        c.execute(
            "SELECT $v IN (SELECT link FROM docs) AS v",
            &Parameters::from([("$v".into(), binary.clone())])
        )
        .unwrap()
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    q(&c, "CREATE TABLE binary_lhs(v BLOB)");
    c.execute(
        "INSERT INTO binary_lhs VALUES ($v)",
        &Parameters::from([("$v".into(), binary)]),
    )
    .unwrap();
    assert_eq!(
        q(
            &c,
            "SELECT v IN (SELECT link FROM docs) AS v FROM binary_lhs"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    q(&c, "CREATE TABLE lhs(n INTEGER)");
    q(&c, "INSERT INTO lhs VALUES (1),(3)");
    q(&c, "CREATE TABLE rhs(n)");
    q(&c, "INSERT INTO rhs VALUES ('1'),('2'),(NULL)");
    assert_eq!(
        q(
            &c,
            "SELECT n,n IN (SELECT n FROM docs) AS present FROM lhs ORDER BY n"
        )
        .rows,
        q(
            &c,
            "SELECT n,n IN (SELECT n FROM rhs) AS present FROM lhs ORDER BY n"
        )
        .rows
    );
    assert_eq!(
        q(
            &c,
            "WITH v AS (SELECT link FROM docs) SELECT docs:a IN (SELECT link FROM v) AS v"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn membership_subquery_affinity_and_collation_match_native_sql() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(v) VALUES ('1'),('a'),('tail '),(2)");
    q(&c, "CREATE TABLE rhs(v)");
    q(&c, "INSERT INTO rhs VALUES ('1'),('a'),('tail '),(2)");
    let values = "(1),('1'),(2),('2'),('A'),('a'),('tail'),('tail '),(NULL),(X'31')";
    for (index, declaration) in [
        "",
        "INTEGER",
        "REAL",
        "NUMERIC",
        "TEXT",
        "BLOB",
        "TEXT COLLATE NOCASE",
    ]
    .iter()
    .enumerate()
    {
        let table = format!("left_{index}");
        q(&c, &format!("CREATE TABLE {table}(v {declaration})"));
        q(&c, &format!("INSERT INTO {table} VALUES {values}"));
        for left in [
            "l.v",
            "+l.v",
            "CAST(l.v AS TEXT)",
            "l.v COLLATE BINARY",
            "l.v COLLATE NOCASE",
            "l.v COLLATE RTRIM",
        ] {
            for right in [
                "v",
                "v COLLATE BINARY",
                "v COLLATE NOCASE",
                "v COLLATE RTRIM",
                "+v",
                "v || ''",
            ] {
                for negate in ["", "NOT "] {
                    let logical = format!("SELECT {left} {negate}IN (SELECT {right} FROM docs) AS matched FROM {table} l ORDER BY l.rowid");
                    let native = logical.replace("FROM docs", "FROM rhs");
                    assert_eq!(
                        q(&c, &logical).rows,
                        q(&c, &native).rows,
                        "{declaration}: {logical}"
                    );
                }
            }
        }
    }
}

#[test]
fn subquery_insert_failures_restore_documents_indexes_and_prior_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE source");
    q(&c, "INSERT INTO source(n) VALUES (1),(2),(9)");
    q(&c, "CREATE TABLE target");
    q(&c, "CREATE UNIQUE INDEX target_n ON target(n)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO target(n) VALUES (9)");
    let prior = q(&c, "SELECT id,n FROM target").rows;
    for predicate in [
        "n IN (SELECT n FROM source)",
        "n NOT IN (SELECT n FROM source WHERE n=99)",
        "EXISTS (SELECT n FROM source WHERE n=2)",
        "n <= (SELECT max(n) FROM source)",
    ] {
        let sql =
            format!("INSERT INTO target(n) SELECT n FROM source WHERE {predicate} ORDER BY n");
        assert!(c.execute(&sql, &Parameters::new()).is_err(), "{sql}");
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(q(&c, "SELECT id,n FROM target").rows, prior);
        assert_eq!(
            c.check_collection_integrity("target", Default::default())
                .unwrap()
                .documents,
            1
        );
        assert!(q(&c, "SELECT n FROM target WHERE n=1").rows.is_empty());
        q(&c, "INSERT INTO target(n) SELECT n FROM source WHERE n IN (SELECT n FROM source WHERE n<9)");
        assert_eq!(
            q(&c, "SELECT n FROM target ORDER BY n").rows,
            vec![
                vec![Value::Integer(1)],
                vec![Value::Integer(2)],
                vec![Value::Integer(9)]
            ]
        );
        q(&c, "DELETE FROM target WHERE n<9");
    }
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT n FROM target").rows.is_empty());
    assert_eq!(
        c.check_collection_integrity("target", Default::default())
            .unwrap()
            .documents,
        0
    );
    assert_eq!(
        q(&c, "SELECT n FROM source ORDER BY n").rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(9)]
        ]
    );
}

#[test]
fn source_free_subqueries_and_ctes_preserve_typed_parameters_and_helpers() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let record = q(&c, "SELECT type::record('docs','key') AS v").rows[0][0].clone();
    for value in [
        record,
        Value::Boolean(true),
        Value::Array(vec![Value::Integer(1)]),
        Value::Object(Default::default()),
        Value::Binary(b"FDB\x01payload".to_vec()),
        Value::vector32(&[1.0, 0.0]).unwrap(),
    ] {
        let params = Parameters::from([("$v".into(), value.clone())]);
        for sql in [
            "SELECT (SELECT $v AS v) AS v",
            "WITH chosen AS (SELECT $v AS v) SELECT v FROM chosen",
            "SELECT v FROM (SELECT $v AS v) chosen",
            "SELECT (SELECT (SELECT $v AS v) AS v) AS v",
        ] {
            assert_eq!(
                c.execute(sql, &params).unwrap().rows,
                vec![vec![value.clone()]],
                "{sql}"
            );
        }
        assert_eq!(
            c.execute("SELECT EXISTS (SELECT $v AS v) AS v", &params)
                .unwrap()
                .rows,
            vec![vec![Value::Integer(1)]]
        );
    }
    assert_eq!(
        q(&c, "SELECT (SELECT type::record('docs','key') AS v) AS v").rows,
        q(&c, "SELECT type::record('docs','key') AS v").rows
    );
    assert_eq!(
        q(
            &c,
            "SELECT docs:key IN (SELECT type::record('docs','key') AS v) AS v"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    q(&c, "CREATE TABLE docs");
    let numbered = Parameters::from([
        ("?1".into(), Value::Integer(3)),
        ("?2".into(), Value::Array(vec![Value::Boolean(true)])),
    ]);
    assert_eq!(
        c.execute("SELECT ?1 AS n,(SELECT ?2 AS v) AS v", &numbered)
            .unwrap()
            .rows,
        vec![vec![Value::Integer(3), numbered["?2"].clone()]]
    );
    q(&c, "CREATE TABLE native(v BLOB)");
    assert!(c
        .execute(
            "INSERT INTO native SELECT $v AS v",
            &Parameters::from([("$v".into(), Value::Array(vec![]))])
        )
        .is_err());
    assert!(q(&c, "SELECT * FROM native").rows.is_empty());
    let params = Parameters::from([("$v".into(), Value::Array(vec![Value::Integer(7)]))]);
    c.execute("INSERT INTO docs(v) SELECT (SELECT $v AS v) AS v", &params)
        .unwrap();
    assert_eq!(
        q(&c, "SELECT v FROM docs").rows,
        vec![vec![params["$v"].clone()]]
    );
}

#[test]
fn nested_anonymous_parameters_retain_statement_indices_and_fail_before_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let a = Value::Array(vec![Value::Integer(7)]);
    let b = Value::Binary(b"FDB\x01not-json".to_vec());
    let params = Parameters::from([
        ("?1".into(), Value::Integer(4)),
        ("?2".into(), a.clone()),
        ("?3".into(), b.clone()),
    ]);
    assert_eq!(
        c.execute(
            "SELECT ? AS n,(SELECT ? AS v) AS a,(SELECT ? AS v) AS b",
            &params
        )
        .unwrap()
        .rows,
        vec![vec![Value::Integer(4), a.clone(), b.clone()]]
    );
    let cte_params = Parameters::from([("?1".into(), a.clone()), ("?2".into(), b.clone())]);
    assert_eq!(
        c.execute(
            "WITH chosen AS (SELECT ? AS a) SELECT a,(SELECT ? AS v) AS b FROM chosen",
            &cte_params
        )
        .unwrap()
        .rows,
        vec![vec![a.clone(), b]]
    );
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE UNIQUE INDEX docs_n ON docs(n)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {n:9}");
    let prior = q(&c, "SELECT id,n FROM docs").rows;
    for invalid in [
        Parameters::from([("?1".into(), Value::Integer(1))]),
        Parameters::from([
            ("?1".into(), Value::Integer(1)),
            ("?2".into(), a.clone()),
            ("$unused".into(), Value::Boolean(true)),
        ]),
    ] {
        assert!(c
            .execute(
                "INSERT INTO docs(n,v) SELECT ? AS n,(SELECT ? AS v) AS v",
                &invalid
            )
            .is_err());
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(q(&c, "SELECT id,n FROM docs").rows, prior);
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .documents,
            1
        );
    }
    c.execute(
        "INSERT INTO docs(n,v) SELECT ? AS n,(SELECT ? AS v) AS v",
        &Parameters::from([("?1".into(), Value::Integer(1)), ("?2".into(), a.clone())]),
    )
    .unwrap();
    assert_eq!(q(&c, "SELECT v FROM docs WHERE n=1").rows, vec![vec![a]]);
    q(&c, "ROLLBACK");
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .documents,
        0
    );
}

#[test]
fn native_source_expression_subqueries_preserve_explicit_logical_values() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE native(n INTEGER,b BLOB)");
    q(&c, "INSERT INTO native VALUES (1,X'31'),(2,X'32')");
    assert_eq!(
        c.execute(
            "SELECT '1' IN (SELECT n FROM native WHERE b=$b) AS v",
            &Parameters::from([("$b".into(), Value::Binary(vec![49]))])
        )
        .unwrap()
        .rows,
        q(
            &c,
            "SELECT '1' IN (SELECT n FROM native WHERE b=X'31') AS v"
        )
        .rows
    );
    let record = q(&c, "SELECT type::record('docs','key') AS v").rows[0][0].clone();
    for value in [
        record,
        Value::Boolean(false),
        Value::Array(vec![Value::Integer(8)]),
        Value::Object(Default::default()),
        Value::Binary(b"FDB\x01not-json".to_vec()),
        Value::vector32(&[0.0, 1.0]).unwrap(),
    ] {
        let params = Parameters::from([("$v".into(), value.clone())]);
        for sql in [
            "WITH chosen AS (SELECT $v AS v FROM native WHERE n=2) SELECT v FROM chosen",
            "SELECT v FROM (SELECT $v AS v FROM native WHERE n=2) chosen",
        ] {
            assert_eq!(
                c.execute(sql, &params).unwrap().rows,
                vec![vec![value.clone()]],
                "{sql}"
            );
        }
        assert_eq!(
            c.execute(
                "SELECT (SELECT $v AS v FROM native WHERE n=2) AS v",
                &params
            )
            .unwrap()
            .rows,
            vec![vec![value]]
        );
        assert_eq!(
            c.execute(
                "SELECT (SELECT $v AS v FROM native WHERE n=99) AS v",
                &params
            )
            .unwrap()
            .rows,
            vec![vec![Value::Null]]
        );
        assert_eq!(
            c.execute(
                "SELECT EXISTS (SELECT $v AS v FROM native WHERE n=2) AS v",
                &params
            )
            .unwrap()
            .rows,
            vec![vec![Value::Integer(1)]]
        );
    }
    assert_eq!(
        q(
            &c,
            "SELECT docs:key IN (SELECT type::record('docs','key') AS v FROM native) AS v"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    let params = Parameters::from([
        ("$v".into(), Value::Array(vec![Value::Boolean(true)])),
        ("$b".into(), Value::Binary(vec![50])),
    ]);
    assert_eq!(
        c.execute(
            "SELECT (SELECT $v AS v FROM native WHERE b=$b) AS v",
            &params
        )
        .unwrap()
        .rows,
        vec![vec![params["$v"].clone()]]
    );
    q(&c, "CREATE TABLE docs");
    c.execute(
        "INSERT INTO docs(v) SELECT (SELECT $v AS v FROM native WHERE b=$b) AS v",
        &params,
    )
    .unwrap();
    assert_eq!(
        q(&c, "SELECT v FROM docs").rows,
        vec![vec![params["$v"].clone()]]
    );
}

#[test]
fn scalar_subquery_pagination_matches_native_and_retains_scope() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(n) VALUES (1),(2),(3),(3)");
    q(&c, "CREATE TABLE native(n INTEGER)");
    q(&c, "INSERT INTO native VALUES (1),(2),(3),(3)");
    for distinct in ["", "DISTINCT "] {
        for pagination in [
            "LIMIT (SELECT min(n) FROM SOURCE)",
            "LIMIT (SELECT max(n) FROM SOURCE) OFFSET (SELECT min(n) FROM SOURCE)",
            "LIMIT (SELECT n-n FROM SOURCE LIMIT 1)",
            "LIMIT (SELECT -n FROM SOURCE LIMIT 1)",
        ] {
            let logical = format!(
                "SELECT {distinct}n FROM docs ORDER BY n {}",
                pagination.replace("SOURCE", "docs")
            );
            let native = logical.replace("FROM docs", "FROM native");
            assert_eq!(q(&c, &logical).rows, q(&c, &native).rows, "{logical}");
        }
    }
    assert_eq!(
        q(
            &c,
            "SELECT n FROM docs ORDER BY n LIMIT (SELECT count(*) FROM native)"
        )
        .rows
        .len(),
        4
    );
    for distinct in ["", "DISTINCT "] {
        let sql = format!("WITH chosen AS (SELECT n FROM docs) SELECT {distinct}n FROM docs ORDER BY n LIMIT (SELECT min(n) FROM chosen)");
        // The pinned engine cannot resolve an outer CTE from LIMIT.
        assert!(c
            .execute(&sql.replace("FROM docs", "FROM native"), &Parameters::new())
            .is_err());
        assert!(c.execute(&sql, &Parameters::new()).is_err());
    }
    let params = Parameters::from([
        ("$limit".into(), Value::Integer(2)),
        ("$offset".into(), Value::Integer(1)),
    ]);
    assert_eq!(c.execute("SELECT n FROM docs ORDER BY n LIMIT (SELECT $limit FROM docs LIMIT 1) OFFSET (SELECT $offset FROM docs LIMIT 1)", &params).unwrap().rows, vec![vec![Value::Integer(2)],vec![Value::Integer(3)]]);
    q(&c, "CREATE TABLE target");
    assert!(c
        .execute(
            "INSERT INTO target(n) SELECT n FROM docs LIMIT (SELECT n FROM docs WHERE n=99)",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(
        c.check_collection_integrity("target", Default::default())
            .unwrap()
            .documents,
        0
    );
    q(
        &c,
        "INSERT INTO target(n) SELECT n FROM docs ORDER BY n LIMIT (SELECT min(n) FROM docs)",
    );
    assert_eq!(
        q(&c, "SELECT n FROM target").rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn compound_subquery_pagination_matches_native_and_is_atomic() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(n) VALUES (1),(2),(3),(3)");
    q(&c, "CREATE TABLE native(n INTEGER)");
    q(&c, "INSERT INTO native VALUES (1),(2),(3),(3)");
    for operator in ["UNION ALL", "UNION", "INTERSECT", "EXCEPT"] {
        for pagination in [
            "LIMIT (SELECT max(n) FROM docs) OFFSET (SELECT min(n) FROM docs)",
            "LIMIT (SELECT n-n FROM docs LIMIT 1)",
            "LIMIT (SELECT -n FROM docs LIMIT 1)",
        ] {
            let sql = format!("SELECT n FROM docs {operator} SELECT n FROM docs WHERE n=3 ORDER BY n {pagination}");
            let native = format!("SELECT n FROM (SELECT n FROM native {operator} SELECT n FROM native WHERE n=3) ORDER BY n {}", pagination.replace("FROM docs", "FROM native"));
            assert_eq!(q(&c, &sql).rows, q(&c, &native).rows, "{sql}");
        }
    }
    // The pinned direct native compound form rejects this pagination; wrapping
    // the compound in a native derived table supplies the successful oracle above.
    assert!(c.execute("SELECT n FROM native UNION ALL SELECT n FROM native WHERE n=3 ORDER BY n LIMIT (SELECT max(n) FROM native) OFFSET (SELECT min(n) FROM native)", &Parameters::new()).is_err());
    assert_eq!(q(&c, "SELECT n FROM native UNION SELECT n FROM native ORDER BY n LIMIT (SELECT min(n) FROM docs)").rows, vec![vec![Value::Integer(1)]]);
    let params = Parameters::from([
        ("$limit".into(), Value::Integer(2)),
        ("$offset".into(), Value::Integer(1)),
    ]);
    assert_eq!(c.execute("SELECT n FROM docs UNION SELECT n FROM docs ORDER BY n LIMIT (SELECT $limit FROM docs LIMIT 1) OFFSET (SELECT $offset FROM docs LIMIT 1)", &params).unwrap().rows, vec![vec![Value::Integer(2)], vec![Value::Integer(3)]]);
    q(&c, "CREATE TABLE target");
    assert!(c.execute("INSERT INTO target(n) SELECT n FROM docs UNION SELECT n FROM docs LIMIT (SELECT n FROM docs WHERE n=99)", &Parameters::new()).is_err());
    assert_eq!(
        c.check_collection_integrity("target", Default::default())
            .unwrap()
            .documents,
        0
    );
    q(&c, "INSERT INTO target(n) SELECT n FROM docs UNION SELECT n FROM docs ORDER BY n LIMIT (SELECT min(n) FROM docs)");
    assert_eq!(
        q(&c, "SELECT n FROM target").rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn pagination_errors_preserve_prior_transaction_work_and_scope() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(n) VALUES (1),(2)");
    q(&c, "CREATE TABLE target");
    q(&c, "CREATE UNIQUE INDEX target_n ON target(n)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO target {id:target:prior,n:9}");
    let prior = q(&c, "SELECT id,n FROM target").rows;
    for source in [
        "SELECT n FROM docs",
        "SELECT DISTINCT n FROM docs",
        "SELECT n FROM docs UNION SELECT n FROM docs",
    ] {
        for limit in [
            "(SELECT n FROM docs WHERE n=99)",
            "(SELECT 1.5 FROM docs)",
            "(SELECT 'invalid' FROM docs)",
            "n+(SELECT min(n) FROM docs)",
        ] {
            let sql = format!("INSERT INTO target(n) {source} LIMIT {limit}");
            assert!(c.execute(&sql, &Parameters::new()).is_err(), "{sql}");
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
            assert_eq!(q(&c, "SELECT id,n FROM target").rows, prior);
            assert_eq!(
                c.check_collection_integrity("target", Default::default())
                    .unwrap()
                    .documents,
                1
            );
            assert!(q(&c, "SELECT n FROM target WHERE n=1").rows.is_empty());
        }
    }
    q(&c, "INSERT INTO target(n) SELECT n FROM docs UNION SELECT n FROM docs ORDER BY n LIMIT (SELECT min(n) FROM docs)");
    assert_eq!(
        q(&c, "SELECT n FROM target ORDER BY n").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(9)]]
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
fn native_scalar_sources_project_into_collection_queries() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(n) VALUES (1),(2)");
    q(&c, "CREATE TABLE native(n INTEGER,b BLOB)");
    q(
        &c,
        "INSERT INTO native VALUES (3,x'464442000102'),(4,x'ff')",
    );
    assert_eq!(q(&c, "SELECT n,(SELECT max(n) FROM native) AS maximum,(SELECT b FROM native ORDER BY n LIMIT 1) AS bytes,(SELECT n FROM native WHERE n=99) AS missing FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(1),Value::Integer(4),Value::Binary(vec![70,68,66,0,1,2]),Value::Null],vec![Value::Integer(2),Value::Integer(4),Value::Binary(vec![70,68,66,0,1,2]),Value::Null]]);
    assert_eq!(
        q(
            &c,
            "SELECT n FROM docs WHERE n<(SELECT min(n) FROM native) ORDER BY n"
        )
        .rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    assert_eq!(
        c.execute(
            "SELECT (SELECT b FROM native WHERE n=$n) AS b FROM docs LIMIT 1",
            &Parameters::from([("$n".into(), Value::Integer(4))])
        )
        .unwrap()
        .rows,
        vec![vec![Value::Binary(vec![255])]]
    );
    assert!(c
        .execute(
            "SELECT (SELECT n,b FROM native) AS v FROM docs",
            &Parameters::new()
        )
        .is_err());
    q(&c, "CREATE TABLE target");
    q(&c, "INSERT INTO target(n,b) SELECT (SELECT max(n) FROM native),(SELECT b FROM native ORDER BY n LIMIT 1) FROM docs LIMIT 1");
    assert_eq!(
        q(&c, "SELECT n,b FROM target").rows,
        vec![vec![
            Value::Integer(4),
            Value::Binary(vec![70, 68, 66, 0, 1, 2])
        ]]
    );
}

#[test]
fn native_exists_sources_filter_collections_and_validate_bindings() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(n) VALUES (1),(2)");
    q(&c, "CREATE TABLE native(n INTEGER)");
    q(&c, "INSERT INTO native VALUES (1),(2)");
    for inner in [
        "SELECT * FROM native",
        "SELECT n,n FROM native WHERE n=99",
        "SELECT count(*) FROM native WHERE n=99",
        "SELECT n FROM native LIMIT 0",
        "SELECT n FROM native LIMIT 1 OFFSET 2",
    ] {
        let sql = format!("SELECT n,EXISTS ({inner}) AS present,NOT EXISTS ({inner}) AS absent FROM docs ORDER BY n");
        assert_eq!(
            q(&c, &sql).rows,
            q(&c, &sql.replace("FROM docs", "FROM native")).rows,
            "{sql}"
        );
    }
    let sql = "SELECT n FROM docs WHERE EXISTS (SELECT n FROM native WHERE n=$n) ORDER BY n";
    assert_eq!(
        c.execute(sql, &Parameters::from([("$n".into(), Value::Integer(2))]))
            .unwrap()
            .rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    q(&c, "CREATE TABLE target");
    q(&c, "CREATE UNIQUE INDEX target_n ON target(n)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO target {id:target:prior,n:9}");
    let prior = q(&c, "SELECT id,n FROM target").rows;
    assert!(c
        .execute(&format!("INSERT INTO target(n) {sql}"), &Parameters::new())
        .is_err());
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(q(&c, "SELECT id,n FROM target").rows, prior);
    assert_eq!(
        c.check_collection_integrity("target", Default::default())
            .unwrap()
            .documents,
        1
    );
    c.execute(
        &format!("INSERT INTO target(n) {sql}"),
        &Parameters::from([("$n".into(), Value::Integer(2))]),
    )
    .unwrap();
    assert_eq!(
        q(&c, "SELECT n FROM target ORDER BY n").rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(9)]
        ]
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
fn native_scalar_affinity_matches_native_document_scalar_storage() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(
        &c,
        "INSERT INTO docs(v) VALUES ('2'),(2),('A'),('a '),('A '),('a  '),('a\t'),(''),(NULL)",
    );
    q(&c, "CREATE TABLE lhs(v BLOB)");
    q(
        &c,
        "INSERT INTO lhs VALUES ('2'),(2),('A'),('a '),('A '),('a  '),('a\t'),(''),(NULL)",
    );
    assert_eq!(q(&c, "SELECT a.v,(SELECT b.v FROM lhs b WHERE b.rowid=a.rowid) AS nested FROM lhs a ORDER BY a.rowid").rows, q(&c, "SELECT v,v FROM lhs ORDER BY rowid").rows);
    for (name, declaration, value) in [
        ("numbers", "INTEGER", "2"),
        ("strings", "TEXT", "'2'"),
        ("letters", "TEXT COLLATE NOCASE", "'a'"),
        ("trimmed", "TEXT COLLATE RTRIM", "'a'"),
    ] {
        q(&c, &format!("CREATE TABLE {name}(v {declaration})"));
        q(&c, &format!("INSERT INTO {name} VALUES ({value})"));
        for projection in [
            "v",
            "v COLLATE BINARY",
            "v COLLATE NOCASE",
            "v COLLATE RTRIM",
            "+v",
            "CAST(v AS TEXT)",
            "CAST(v AS NUMERIC)",
        ] {
            for op in ["=", "!=", "IS", "IS NOT", "<", "<=", ">", ">="] {
                for operands in [
                    format!("v {op} (SELECT {projection} FROM {name})"),
                    format!("(SELECT {projection} FROM {name}) {op} v"),
                    format!("v {op} ((SELECT {projection} FROM {name}) COLLATE NOCASE)"),
                    format!("((SELECT {projection} FROM {name}) COLLATE NOCASE) {op} v"),
                    format!("(v COLLATE BINARY) {op} ((SELECT {projection} FROM {name}) COLLATE NOCASE)"),
                    format!("((SELECT {projection} FROM {name}) COLLATE NOCASE) {op} (v COLLATE BINARY)"),
                    format!("(v COLLATE NOCASE) {op} ((SELECT {projection} FROM {name}) COLLATE RTRIM)"),
                    format!("((SELECT {projection} FROM {name}) COLLATE RTRIM) {op} (v COLLATE NOCASE)"),
                ] {
                    let sql = format!("SELECT {operands} AS matched FROM docs");
                    assert_eq!(
                        q(&c, &sql).rows,
                        q(&c, &sql.replace("FROM docs", "FROM lhs")).rows,
                        "{sql}"
                    );
                }
            }
        }
    }
}

#[test]
fn native_scalar_comparisons_preserve_binary_identity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(
        &c,
        "INSERT INTO docs(v) VALUES (x'464442000102'),(docs:a),(NULL)",
    );
    q(&c, "CREATE TABLE native(v BLOB)");
    q(&c, "INSERT INTO native VALUES (x'464442000102')");
    for comparison in ["v=(SELECT v FROM native)", "(SELECT v FROM native)=v"] {
        assert_eq!(
            q(&c, &format!("SELECT {comparison} AS matched FROM docs")).rows,
            vec![
                vec![Value::Integer(1)],
                vec![Value::Integer(0)],
                vec![Value::Null]
            ]
        );
    }
}

#[test]
fn native_scalar_comparison_insert_failures_preserve_indexes_and_prior_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(n,v) VALUES (1,'2'),(2,'2'),(9,'2')");
    q(&c, "CREATE TABLE native(v INTEGER)");
    q(&c, "INSERT INTO native VALUES (2)");
    q(&c, "CREATE TABLE target");
    q(&c, "CREATE UNIQUE INDEX target_n ON target(n)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO target {id:target:prior,n:9}");
    let prior = q(&c, "SELECT id,n FROM target").rows;
    for predicate in [
        "v=(SELECT v FROM native)",
        "(SELECT v FROM native)=v",
        "v=((SELECT v FROM native) COLLATE NOCASE)",
        "((SELECT v FROM native) COLLATE NOCASE)=v",
    ] {
        let sql = format!("INSERT INTO target(n) SELECT n FROM docs WHERE {predicate} ORDER BY n");
        assert!(c.execute(&sql, &Parameters::new()).is_err(), "{sql}");
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(q(&c, "SELECT id,n FROM target").rows, prior);
        assert_eq!(
            c.check_collection_integrity("target", Default::default())
                .unwrap()
                .documents,
            1
        );
        assert!(q(&c, "SELECT n FROM target WHERE n=1").rows.is_empty());
        let retry = format!(
            "INSERT INTO target(n) SELECT n FROM docs WHERE ({predicate}) AND n<9 ORDER BY n"
        );
        q(&c, &retry);
        assert_eq!(
            q(&c, "SELECT n FROM target ORDER BY n").rows,
            vec![
                vec![Value::Integer(1)],
                vec![Value::Integer(2)],
                vec![Value::Integer(9)]
            ]
        );
        q(&c, "DELETE FROM target WHERE n<9");
    }
    q(&c, "ROLLBACK");
    assert_eq!(
        c.check_collection_integrity("target", Default::default())
            .unwrap()
            .documents,
        0
    );
}

#[test]
fn native_membership_sources_match_scalar_affinity_and_null_semantics() {
    for declaration in [
        "INTEGER",
        "TEXT",
        "TEXT COLLATE NOCASE",
        "TEXT COLLATE RTRIM",
        "BLOB",
    ] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(&c, "CREATE TABLE lhs(n BLOB)");
        q(
            &c,
            "INSERT INTO docs(n) VALUES (1),(2),('1'),('a'),('A'),('a '),('a  '),(NULL)",
        );
        q(
            &c,
            "INSERT INTO lhs VALUES (1),(2),('1'),('a'),('A'),('a '),('a  '),(NULL)",
        );
        q(&c, &format!("CREATE TABLE rhs(n {declaration})"));
        q(&c, "INSERT INTO rhs VALUES (1),('a'),(NULL)");
        for projection in ["n", "+n", "CAST(n AS TEXT)"] {
            for predicate in ["1", "n IS NOT NULL", "0"] {
                for (op, left) in ["IN", "NOT IN"].into_iter().flat_map(|op| {
                    [
                        "n",
                        "+n",
                        "(n)",
                        "n COLLATE BINARY",
                        "n COLLATE NOCASE",
                        "n COLLATE RTRIM",
                        "CAST(n AS TEXT)",
                    ]
                    .into_iter()
                    .map(move |left| (op, left))
                }) {
                    let suffix =
                        format!("{left} {op} (SELECT {projection} FROM rhs WHERE {predicate})");
                    let expected =
                        q(&c, &format!("SELECT n,{suffix} FROM lhs ORDER BY rowid")).rows;
                    let actual = q(&c, &format!("SELECT n,{suffix} FROM docs ORDER BY rowid")).rows;
                    assert_eq!(actual, expected, "{declaration}: {suffix}");
                }
            }
        }
    }
}

#[test]
fn native_membership_insert_parameters_and_binary_identity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE rhs(n BLOB)");
    q(&c, "CREATE TABLE target");
    q(&c, "CREATE UNIQUE INDEX target_n ON target(n)");
    let record = Value::Record(romedb::Record {
        table: "docs".into(),
        key: romedb::Key::String("a".into()),
    });
    let bytes = Value::Binary(
        b"FDB\x01{\"type\":\"Record\",\"value\":{\"table\":\"docs\",\"key\":{\"String\":\"a\"}}}"
            .to_vec(),
    );
    let params = Parameters::from([("$record".into(), record), ("$bytes".into(), bytes)]);
    c.execute("INSERT INTO docs(n) VALUES ($record),($bytes)", &params)
        .unwrap();
    c.execute(
        "INSERT INTO rhs VALUES ($bytes)",
        &Parameters::from([("$bytes".into(), params["$bytes"].clone())]),
    )
    .unwrap();
    assert_eq!(
        q(
            &c,
            "SELECT n IN (SELECT n FROM rhs) FROM docs ORDER BY rowid"
        )
        .rows,
        vec![vec![Value::Integer(0)], vec![Value::Integer(1)]]
    );
    q(&c, "DELETE FROM docs");
    q(&c, "DELETE FROM rhs");
    q(&c, "INSERT INTO docs(n) VALUES (1),(2)");
    q(&c, "INSERT INTO rhs VALUES (1),(2)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO target {id:target:prior,n:9}");
    let sql = "INSERT INTO target(n) SELECT n IN (SELECT n FROM rhs WHERE n>$min) FROM docs";
    assert_eq!(
        c.execute(sql, &Parameters::new()).unwrap_err().code(),
        "FDB_PARAMETER"
    );
    let result = c.execute(sql, &Parameters::from([("$min".into(), Value::Integer(0))]));
    assert!(result.is_err());
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
        c.execute(sql, &Parameters::from([("$min".into(), Value::Integer(1))]))
            .unwrap()
            .affected,
        2
    );
    assert_eq!(
        c.check_collection_integrity("target", Default::default())
            .unwrap()
            .documents,
        3
    );
    q(&c, "ROLLBACK");
}

#[test]
fn native_membership_compound_arms_keep_shared_sources_in_scope() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(n) VALUES (1),(2),(NULL)");
    q(&c, "CREATE TABLE lhs(n BLOB)");
    q(&c, "INSERT INTO lhs VALUES (1),(2),(NULL)");
    q(&c, "CREATE TABLE rhs(n INTEGER)");
    q(&c, "INSERT INTO rhs VALUES (1),(NULL)");
    for op in ["UNION ALL", "UNION", "INTERSECT", "EXCEPT"] {
        let query = |table| {
            format!("SELECT n IN (SELECT n FROM rhs) AS matched FROM {table} {op} SELECT n NOT IN (SELECT n FROM rhs) FROM {table}")
        };
        assert_eq!(
            q(&c, &query("docs")).rows,
            q(&c, &query("lhs")).rows,
            "{op}"
        );
    }
}

#[test]
fn native_membership_compound_arms_resolve_outer_cte_parameters() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(n) VALUES (1),(2),(NULL)");
    q(&c, "CREATE TABLE lhs(n BLOB)");
    q(&c, "INSERT INTO lhs VALUES (1),(2),(NULL)");
    q(&c, "CREATE TABLE rhs(n INTEGER)");
    q(&c, "INSERT INTO rhs VALUES (1),(2),(NULL)");
    let params = Parameters::from([("$min".into(), Value::Integer(1))]);
    for op in ["UNION ALL", "UNION", "INTERSECT", "EXCEPT"] {
        let query = |table| {
            format!("WITH r AS (SELECT n FROM rhs WHERE n>$min) SELECT n IN (SELECT n FROM r) AS v FROM {table} {op} SELECT n NOT IN (SELECT n FROM r) FROM {table}")
        };
        let expected = c.execute(&query("lhs"), &params).unwrap().rows;
        let actual = c.execute(&query("docs"), &params).unwrap().rows;
        assert_eq!(actual, expected, "{op}");
        assert!(c.execute(&query("docs"), &Parameters::new()).is_err());
    }
}

#[test]
fn cte_membership_compound_inserts_preserve_prior_work_and_retry() {
    for outer in [false, true] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(&c, "INSERT INTO docs(n) VALUES (1),(2)");
        q(&c, "CREATE TABLE rhs(n INTEGER)");
        q(&c, "INSERT INTO rhs VALUES (1),(2)");
        q(&c, "CREATE TABLE target");
        q(&c, "CREATE UNIQUE INDEX target_n ON target(n)");
        if outer {
            q(&c, "BEGIN");
        }
        q(&c, "INSERT INTO target {id:target:prior,n:9}");
        let state = c.transaction_state();
        let script="WITH r AS (SELECT n FROM rhs WHERE n>$min) INSERT INTO target(n) SELECT n IN (SELECT n FROM r) FROM docs UNION ALL SELECT 8";
        let missing = c.execute(script, &Parameters::new()).unwrap_err();
        assert_eq!(missing.code(), "FDB_PARAMETER");
        let conflict = c.execute(
            script,
            &Parameters::from([("$min".into(), Value::Integer(0))]),
        );
        assert!(conflict.is_err());
        assert_eq!(c.transaction_state(), state);
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
        let retry = c
            .execute(
                script,
                &Parameters::from([("$min".into(), Value::Integer(1))]),
            )
            .unwrap();
        assert_eq!(retry.affected, 3);
        assert_eq!(
            q(&c, "SELECT n FROM target ORDER BY n").rows,
            vec![
                vec![Value::Integer(0)],
                vec![Value::Integer(1)],
                vec![Value::Integer(8)],
                vec![Value::Integer(9)]
            ]
        );
        assert_eq!(
            c.check_collection_integrity("target", Default::default())
                .unwrap()
                .index_entries,
            4
        );
        if outer {
            q(&c, "ROLLBACK");
            assert_eq!(
                c.check_collection_integrity("target", Default::default())
                    .unwrap()
                    .documents,
                0
            );
        }
    }
}

#[test]
fn nested_native_membership_resolves_preceding_ctes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(n) VALUES (1),(2),(NULL)");
    q(&c, "CREATE TABLE rhs(n INTEGER)");
    q(&c, "INSERT INTO rhs VALUES (1),(NULL)");
    for sql in [
        "WITH r AS (SELECT n FROM rhs) SELECT v FROM (SELECT n IN (SELECT n FROM r) AS v FROM docs) d",
        "WITH r AS (SELECT n FROM rhs), d AS (SELECT n IN (SELECT n FROM r) AS v FROM docs) SELECT v FROM d",
        "WITH r AS (SELECT n FROM rhs), d AS (SELECT n IN (SELECT n FROM r) AS v FROM docs), e AS (SELECT v FROM d) SELECT v FROM e",
    ] {
        assert_eq!(q(&c,sql).rows,vec![vec![Value::Integer(1)],vec![Value::Null],vec![Value::Null]]);
    }
}

#[test]
fn native_membership_local_cte_names_match_pinned_scope_resolution() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(n) VALUES (1),(2)");
    q(&c, "CREATE TABLE lhs(n BLOB)");
    q(&c, "INSERT INTO lhs VALUES (1),(2)");
    for derived in [false, true] {
        let query = |table| {
            if derived {
                format!("WITH r AS (SELECT 1 AS n) SELECT v FROM (WITH r AS (SELECT 2 AS n) SELECT n IN (SELECT n FROM r) AS v FROM {table}) d")
            } else {
                format!("WITH r AS (SELECT 1 AS n), d AS (WITH r AS (SELECT 2 AS n) SELECT n IN (SELECT n FROM r) AS v FROM {table}) SELECT v FROM d")
            }
        };
        let raw_db = turso_core::Database::open_file(
            turso_core::Database::io_for_path(":memory:").unwrap(),
            ":memory:",
            std::sync::Arc::new(turso_core::SqliteDialect),
        )
        .unwrap();
        let raw = raw_db.connect().unwrap();
        raw.prepare("CREATE TABLE lhs(n BLOB)")
            .unwrap()
            .run_collect_rows()
            .unwrap();
        raw.prepare("INSERT INTO lhs VALUES (1),(2)")
            .unwrap()
            .run_collect_rows()
            .unwrap();
        let expected = raw
            .prepare(query("lhs"))
            .unwrap()
            .run_collect_rows()
            .unwrap()
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|value| match value {
                        turso_core::Value::Numeric(turso_core::Numeric::Integer(value)) => {
                            Value::Integer(value)
                        }
                        other => panic!("unexpected membership result: {other:?}"),
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(q(&c, &query("lhs")).rows, expected);
        assert_eq!(q(&c, &query("docs")).rows, expected);
    }
}

#[test]
fn native_subquery_predicates_correlate_with_outer_collection_rows() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(n) VALUES(1),(2),(3)");
    q(&c, "CREATE TABLE native(n INTEGER)");
    q(&c, "INSERT INTO native VALUES(1),(2),(3)");
    for projection in [
        "(SELECT max(n) FROM native WHERE n<d.n)",
        "(SELECT n FROM native WHERE n=d.n+10)",
        "EXISTS(SELECT n FROM native WHERE n<d.n)",
        "(SELECT max(d.n) FROM native AS d WHERE d.n<3)",
        "((SELECT 'A' WHERE d.n>0) COLLATE NOCASE)='a'",
        "((SELECT 'A' WHERE d.n>0) COLLATE NOCASE)=(SELECT 'a' WHERE d.n>0)",
        "(SELECT max(x.n) FROM native AS x JOIN native AS y ON x.n=y.n AND y.n<d.n)",
    ] {
        let expected = q(
            &c,
            &format!("SELECT d.n,{projection} AS prior FROM native AS d ORDER BY d.n"),
        );
        let sql = format!("SELECT d.n,{projection} AS prior FROM docs AS d ORDER BY d.n");
        let actual = q(&c, &sql);
        assert_eq!(actual.rows, expected.rows, "{sql}");
        assert_eq!(
            c.profile_select(&sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected.rows
        );
    }
}

#[test]
fn correlated_native_scalars_keep_comparisons_parameters_and_atomic_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "DEFINE FIELD n ON docs TYPE integer");
    q(&c, "CREATE UNIQUE INDEX docs_n ON docs(n)");
    q(&c, "INSERT INTO docs(n) VALUES(1),(2),(3)");
    q(&c, "CREATE TABLE native(n INTEGER UNIQUE)");
    q(&c, "INSERT INTO native VALUES(1),(2),(3)");
    q(&c, "CREATE TABLE lookup(n INTEGER)");
    q(&c, "INSERT INTO lookup VALUES(1),(2),(3)");
    let params = Parameters::from([("$delta".into(), Value::Integer(1))]);
    for predicate in [
        "(SELECT max(n) FROM lookup WHERE n<d.n+$delta)=d.n",
        "d.n=(SELECT max(n) FROM lookup WHERE n<d.n+$delta)",
        "(SELECT max(n) FROM lookup WHERE n<d.n+$delta)=2",
        "(SELECT max(n) FROM lookup WHERE n<d.n+$delta)=(SELECT min(n) FROM lookup WHERE n>=d.n)",
    ] {
        let sql = format!("SELECT d.n FROM docs AS d WHERE {predicate} ORDER BY d.n");
        let oracle = sql.replace("FROM docs AS d", "FROM native AS d");
        assert_eq!(
            c.execute(&sql, &params).unwrap().rows,
            c.execute(&oracle, &params).unwrap().rows,
            "{sql}"
        );
        assert!(c.execute(&sql, &Parameters::new()).is_err());
    }
    q(&c, "BEGIN");
    let sql = "UPDATE docs AS d SET n=(SELECT max(n) FROM lookup WHERE n<=d.n)+10 RETURNING n";
    assert_eq!(
        q(&c, sql).rows,
        q(&c, &sql.replace("UPDATE docs", "UPDATE native")).rows
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs(n) VALUES(9)");
    let error = c
        .execute(
            "UPDATE docs AS d SET n=(SELECT n FROM lookup WHERE n=d.n+100)",
            &Parameters::new(),
        )
        .unwrap_err();
    assert_eq!(error.code(), "FDB_VALIDATION");
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .documents,
        4
    );
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    q(&c, "ROLLBACK");
}

#[test]
fn correlated_native_predicates_preserve_binary_keys_and_collation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE native(n INTEGER,v BLOB,t TEXT)");
    q(&c, "CREATE TABLE lookup(v BLOB,t TEXT COLLATE NOCASE)");
    q(
        &c,
        "INSERT INTO lookup VALUES(x'0102','Alpha'),(x'03','Beta')",
    );
    for (n, bytes, text) in [
        (1, vec![1, 2], "alpha"),
        (2, vec![3], "BETA"),
        (3, vec![4], "absent"),
    ] {
        let params = Parameters::from([
            ("$n".into(), Value::Integer(n)),
            ("$v".into(), Value::Binary(bytes)),
            ("$t".into(), Value::String(text.into())),
        ]);
        c.execute("INSERT INTO docs(n,v,t) VALUES($n,$v,$t)", &params)
            .unwrap();
        c.execute("INSERT INTO native VALUES($n,$v,$t)", &params)
            .unwrap();
    }
    for projection in [
        "(SELECT count(*) FROM lookup WHERE v=d.v)",
        "EXISTS(SELECT 1 FROM lookup WHERE v=d.v)",
        "(SELECT count(*) FROM lookup WHERE t=d.t)",
        "(SELECT t FROM lookup WHERE v=d.v)=d.t",
    ] {
        let sql = format!("SELECT d.n,{projection} FROM docs AS d ORDER BY d.n");
        let oracle = sql.replace("FROM docs AS d", "FROM native AS d");
        assert_eq!(q(&c, &sql).rows, q(&c, &oracle).rows, "{sql}");
    }
}

#[test]
fn correlated_scalar_affinity_and_collation_match_native_matrix() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(
        &c,
        "INSERT INTO docs(n,v) VALUES(1,'2'),(2,2),(3,'A'),(4,'a '),(5,NULL)",
    );
    q(&c, "CREATE TABLE lhs(n INTEGER,v BLOB)");
    q(
        &c,
        "INSERT INTO lhs VALUES(1,'2'),(2,2),(3,'A'),(4,'a '),(5,NULL)",
    );
    for (name, declaration, value) in [
        ("numbers", "INTEGER", "2"),
        ("letters", "TEXT COLLATE NOCASE", "'a'"),
        ("trimmed", "TEXT COLLATE RTRIM", "'a'"),
    ] {
        q(&c, &format!("CREATE TABLE {name}(v {declaration})"));
        q(&c, &format!("INSERT INTO {name} VALUES({value})"));
        for projection in [
            "v",
            "v COLLATE NOCASE",
            "v COLLATE RTRIM",
            "+v",
            "CAST(v AS TEXT)",
            "CAST(v AS NUMERIC)",
        ] {
            for op in ["=", "!=", "IS", "IS NOT", "<", "<=", ">", ">="] {
                let rhs = format!("(SELECT {projection} FROM {name} WHERE d.n<5)");
                for expr in [
                    format!("d.v {op} {rhs}"),
                    format!("{rhs} {op} d.v"),
                    format!("({rhs} COLLATE NOCASE) {op} d.v"),
                    format!("d.v {op} ({rhs} COLLATE RTRIM)"),
                ] {
                    let sql = format!("SELECT {expr} FROM docs AS d ORDER BY d.n");
                    assert_eq!(
                        q(&c, &sql).rows,
                        q(&c, &sql.replace("FROM docs AS d", "FROM lhs AS d")).rows,
                        "{sql}"
                    );
                }
            }
        }
    }
}

#[test]
fn native_having_predicates_correlate_with_collection_candidates() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "DEFINE FIELD n ON docs TYPE integer");
    q(&c, "CREATE UNIQUE INDEX docs_n ON docs(n)");
    q(&c, "INSERT INTO docs(n) VALUES(1),(2),(3)");
    q(&c, "CREATE TABLE native(n INTEGER UNIQUE)");
    q(&c, "INSERT INTO native VALUES(1),(2),(3)");
    q(&c, "CREATE TABLE lookup(n INTEGER)");
    q(&c, "INSERT INTO lookup VALUES(1),(2),(3)");
    for projection in [
        "(SELECT max(n) FROM lookup HAVING max(n)>d.n)",
        "(SELECT max(n) AS maximum FROM lookup HAVING maximum>d.n)",
        "(SELECT n FROM lookup GROUP BY n HAVING n<d.n ORDER BY n DESC)",
        "EXISTS(SELECT n FROM lookup GROUP BY n HAVING n<d.n)",
        "(SELECT max(n) FROM lookup WHERE n>=d.n HAVING count(*)>1)",
        "(SELECT max(d.n) FROM lookup AS d HAVING max(d.n)>1)",
    ] {
        let sql = format!("SELECT d.n,{projection} FROM docs AS d ORDER BY d.n");
        let expected = q(&c, &sql.replace("FROM docs AS d", "FROM native AS d")).rows;
        assert_eq!(q(&c, &sql).rows, expected, "{sql}");
        assert_eq!(
            c.profile_select(&sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected
        );
    }
    let sql="SELECT d.n,(SELECT max(n) FROM lookup HAVING max(n)>d.n+$delta) FROM docs AS d ORDER BY d.n";
    let params = Parameters::from([("$delta".into(), Value::Integer(1))]);
    assert_eq!(
        c.execute(sql, &params).unwrap().rows,
        c.execute(&sql.replace("FROM docs AS d", "FROM native AS d"), &params)
            .unwrap()
            .rows
    );
    assert!(c.execute(sql, &Parameters::new()).is_err());
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs(n) VALUES(9)");
    let error = c
        .execute(
            "UPDATE docs AS d SET n=(SELECT max(n) FROM lookup HAVING max(n)>d.n)",
            &Parameters::new(),
        )
        .unwrap_err();
    assert!(matches!(error.code(), "FDB_CONSTRAINT" | "FDB_VALIDATION"));
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(3)],
            vec![Value::Integer(9)]
        ]
    );
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .documents,
        4
    );
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    let retry=q(&c,"UPDATE docs AS d SET n=n+(SELECT count(*) FROM lookup HAVING max(n)>=d.n)+10 WHERE n<9 RETURNING n");
    assert_eq!(
        retry.rows,
        vec![
            vec![Value::Integer(14)],
            vec![Value::Integer(15)],
            vec![Value::Integer(16)]
        ]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        q(&c, "SELECT n FROM native ORDER BY n").rows
    );
}

#[test]
fn correlated_native_membership_matches_null_and_empty_sets() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(n) VALUES(1),(2),(3),(NULL)");
    q(&c, "CREATE TABLE native(n BLOB)");
    q(&c, "INSERT INTO native VALUES(1),(2),(3),(NULL)");
    q(&c, "CREATE TABLE rhs(n INTEGER)");
    q(&c, "INSERT INTO rhs VALUES(1),(2),(NULL)");
    for predicate in ["n<d.n", "n=d.n", "n<d.n OR n IS NULL", "n>d.n+100"] {
        for negate in ["", "NOT "] {
            for lhs in ["d.n", "+d.n", "CAST(d.n AS TEXT)", "2"] {
                let sql=format!("SELECT d.n,{lhs} {negate}IN(SELECT n FROM rhs WHERE {predicate}) FROM docs AS d ORDER BY d.n");
                let expected = q(&c, &sql.replace("FROM docs AS d", "FROM native AS d")).rows;
                assert_eq!(q(&c, &sql).rows, expected, "{sql}");
                assert_eq!(
                    c.profile_select(&sql, &Parameters::new())
                        .unwrap()
                        .result
                        .rows,
                    expected,
                    "profile: {sql}"
                );
            }
        }
    }
}

#[test]
fn correlated_membership_preserves_native_affinity_and_atomic_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(
        &c,
        "INSERT INTO docs(n,v) VALUES(1,'2'),(2,2),(3,'A'),(4,'a '),(5,NULL)",
    );
    q(&c, "CREATE TABLE lhs(n INTEGER,v BLOB)");
    q(
        &c,
        "INSERT INTO lhs VALUES(1,'2'),(2,2),(3,'A'),(4,'a '),(5,NULL)",
    );
    for (name, declaration, value) in [
        ("numbers", "INTEGER", "2"),
        ("letters", "TEXT COLLATE NOCASE", "'a'"),
        ("trimmed", "TEXT COLLATE RTRIM", "'a'"),
    ] {
        q(&c, &format!("CREATE TABLE {name}(v {declaration})"));
        q(&c, &format!("INSERT INTO {name} VALUES({value}),(NULL)"));
        for projection in ["v", "+v", "CAST(v AS TEXT)", "v COLLATE BINARY"] {
            for left in [
                "d.v",
                "+d.v",
                "CAST(d.v AS TEXT)",
                "d.v COLLATE NOCASE",
                "'A'",
            ] {
                for negate in ["", "NOT "] {
                    let sql=format!("SELECT {left} {negate}IN(SELECT {projection} FROM {name} WHERE d.n<5) FROM docs AS d ORDER BY d.n");
                    assert_eq!(
                        q(&c, &sql).rows,
                        q(&c, &sql.replace("FROM docs AS d", "FROM lhs AS d")).rows,
                        "{sql}"
                    );
                }
            }
        }
    }
    q(&c, "CREATE TABLE target");
    q(&c, "CREATE UNIQUE INDEX target_n ON target(n)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO target(n) VALUES(2)");
    let sql="INSERT INTO target(n) SELECT d.n FROM docs AS d WHERE d.n IN(SELECT n FROM lhs WHERE n<=d.n+$delta)";
    assert!(c.execute(sql, &Parameters::new()).is_err());
    let params = Parameters::from([("$delta".into(), Value::Integer(0))]);
    assert_eq!(
        c.execute(sql, &params).unwrap_err().code(),
        "FDB_CONSTRAINT"
    );
    assert_eq!(
        q(&c, "SELECT n FROM target").rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        c.check_collection_integrity("target", Default::default())
            .unwrap()
            .documents,
        1
    );
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    let retry = format!("{sql} AND d.n<>2");
    assert_eq!(c.execute(&retry, &params).unwrap().affected, 4);
    c.check_collection_integrity("target", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT n FROM target").rows.is_empty());
}

#[test]
fn correlated_membership_keeps_native_binary_and_record_identities_distinct() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(
        &c,
        "INSERT INTO docs(n,v) VALUES(1,x'464442000102'),(2,docs:a),(3,NULL)",
    );
    q(&c, "CREATE TABLE rhs(v BLOB)");
    q(&c, "INSERT INTO rhs VALUES(x'464442000102')");
    for negate in ["", "NOT "] {
        let rows = q(
            &c,
            &format!(
                "SELECT d.v {negate}IN(SELECT v FROM rhs WHERE d.n>0) FROM docs AS d ORDER BY d.n"
            ),
        )
        .rows;
        assert_eq!(
            rows,
            vec![
                vec![Value::Integer(i64::from(negate.is_empty()))],
                vec![Value::Integer(i64::from(!negate.is_empty()))],
                vec![Value::Null]
            ]
        );
    }
}

#[test]
fn correlated_predicates_resolve_nested_document_paths() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {n:1,meta:{n:1,deep:{n:1}}}");
    q(&c, "INSERT INTO docs {n:2,meta:{n:2,deep:{n:2}}}");
    q(&c, "INSERT INTO docs {n:3,meta:{n:null,deep:{n:null}}}");
    q(&c, "INSERT INTO docs {n:4,meta:{}}");
    q(&c, "INSERT INTO docs {n:5,meta:7}");
    q(&c, "CREATE TABLE native(n INTEGER,v INTEGER)");
    q(
        &c,
        "INSERT INTO native VALUES(1,1),(2,2),(3,NULL),(4,NULL),(5,NULL)",
    );
    q(&c, "CREATE TABLE rhs(n INTEGER)");
    q(&c, "INSERT INTO rhs VALUES(1),(2),(NULL)");
    for field in ["d.meta.n", "d.meta.deep.n"] {
        for projection in [
            format!("(SELECT max(n) FROM rhs WHERE n<={field})"),
            format!("(SELECT 1 WHERE {field}=1)"),
            format!("EXISTS(SELECT 1 WHERE {field}=1)"),
            format!("1 IN(SELECT 1 WHERE {field}=1)"),
            format!("EXISTS(SELECT n FROM rhs WHERE n={field})"),
            format!("d.n IN(SELECT n FROM rhs WHERE n<={field})"),
            format!("d.n NOT IN(SELECT n FROM rhs GROUP BY n HAVING n<={field})"),
            format!("(SELECT max(x.n) FROM rhs x JOIN rhs y ON x.n=y.n AND y.n<={field})"),
        ] {
            let sql = format!("SELECT d.n,{projection} FROM docs AS d ORDER BY d.n");
            let oracle = sql
                .replace("FROM docs AS d", "FROM native AS d")
                .replace(field, "d.v");
            let expected = q(&c, &oracle).rows;
            assert_eq!(q(&c, &sql).rows, expected, "{sql}");
            let derived = sql.replace("FROM docs AS d", "FROM (SELECT n,meta FROM docs) AS d");
            assert_eq!(q(&c, &derived).rows, expected, "{derived}");
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected,
                "{sql}"
            );
        }
    }
    assert_eq!(
        q(
            &c,
            "SELECT d.n,d.meta.n AS shallow,d.meta.deep.n AS deep FROM (SELECT n,meta FROM docs) AS d ORDER BY d.n"
        )
        .rows,
        q(&c, "SELECT n,v,v FROM native ORDER BY n").rows
    );
    for field in ["d.meta.n", "d.meta.deep.n"] {
        let sql = format!("SELECT (SELECT n FROM rhs AS d WHERE n={field}) FROM docs AS d");
        assert!(
            c.execute(&sql, &Parameters::new()).is_err(),
            "local alias must shadow outer: {sql}"
        );
    }
    q(&c, "CREATE UNIQUE INDEX docs_n ON docs(n)");
    q(&c, "BEGIN");
    let result=q(&c,"UPDATE docs AS d SET n=n+10 WHERE d.n IN(SELECT n FROM rhs WHERE n=d.meta.deep.n) RETURNING n");
    assert_eq!(result.affected, 2);
    assert_eq!(
        result.rows,
        vec![vec![Value::Integer(11)], vec![Value::Integer(12)]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        q(&c, "SELECT n FROM native ORDER BY n").rows
    );
}

#[test]
fn correlated_native_projections_preserve_outer_values() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE native(n INTEGER)");
    q(&c, "INSERT INTO native VALUES(1),(2),(NULL)");
    q(&c, "INSERT INTO docs(n) VALUES(1),(2),(NULL)");
    for expr in [
        "(SELECT d.n)",
        "(SELECT d.n+1)",
        "(SELECT n+d.n FROM native WHERE n=1)",
        "(SELECT max(n)+d.n FROM native)",
        "EXISTS(SELECT d.n FROM native WHERE n=1)",
        "d.n IN (SELECT d.n FROM native WHERE n=1)",
    ] {
        let expected = q(
            &c,
            &format!("SELECT d.n,{expr} AS v FROM native d ORDER BY d.n"),
        );
        let sql = format!("SELECT d.n,{expr} AS v FROM docs d ORDER BY d.n");
        assert_eq!(q(&c, &sql).rows, expected.rows, "{expr}");
        assert_eq!(
            c.profile_select(&sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected.rows
        );
    }
    q(&c, "CREATE TABLE typed");
    q(
        &c,
        "INSERT INTO typed {flag:true,link:typed:a,meta:{ok:true},items:[1,false]}",
    );
    for field in ["flag", "link", "meta", "items"] {
        assert_eq!(
            q(&c, &format!("SELECT (SELECT d.{field}) AS v FROM typed d")).rows,
            q(&c, &format!("SELECT {field} AS v FROM typed")).rows
        );
    }
}

#[test]
fn correlated_projection_membership_empty_results_and_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {id:docs:a,n:1,flag:true,link:docs:a}");
    q(
        &c,
        "INSERT INTO docs {id:docs:b,n:2,flag:false,link:docs:b}",
    );
    q(&c, "CREATE TABLE native(n INTEGER)");
    q(&c, "INSERT INTO native VALUES(1),(2)");
    for field in ["flag", "link", "n"] {
        assert_eq!(q(&c, &format!("SELECT d.{field} IN (SELECT d.{field}), d.{field} NOT IN (SELECT d.{field} WHERE 0), (SELECT d.{field} WHERE 0) FROM docs d ORDER BY d.n")).rows,
            vec![vec![Value::Integer(1), Value::Integer(1), Value::Null]; 2]);
    }
    let params = Parameters::from([("$add".into(), Value::Integer(3))]);
    assert_eq!(
        c.execute("SELECT (SELECT d.n+$add) FROM docs d ORDER BY d.n", &params)
            .unwrap()
            .rows,
        vec![vec![Value::Integer(4)], vec![Value::Integer(5)]]
    );
    assert!(c
        .execute(
            "SELECT (SELECT d.n+$missing) FROM docs d",
            &Parameters::new()
        )
        .is_err());
    q(&c, "CREATE UNIQUE INDEX docs_n ON docs(n)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO native VALUES(9)");
    assert!(c
        .execute(
            "UPDATE docs SET n=(SELECT docs.n-docs.n+7)",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    assert_eq!(
        q(&c, "SELECT count(*) FROM native").rows,
        vec![vec![Value::Integer(3)]]
    );
    assert_eq!(
        q(&c, "UPDATE docs SET n=(SELECT docs.n+10) RETURNING n").affected,
        2
    );
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}

#[test]
fn correlated_projection_comparisons_and_shadowing_match_typeless_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE native(n)");
    q(&c, "INSERT INTO docs(n) VALUES(1),(2),('1'),('A'),(NULL)");
    q(&c, "INSERT INTO native VALUES(1),(2),('1'),('A'),(NULL)");
    for projected in [
        "d.n",
        "+d.n",
        "CAST(d.n AS TEXT)",
        "d.n COLLATE NOCASE",
        "(CAST(d.n AS TEXT))",
        "CAST(d.n AS TEXT) COLLATE NOCASE",
        "((CAST(d.n AS INTEGER)) COLLATE BINARY)",
    ] {
        for op in ["=", "!=", "<", ">", "IS"] {
            for rhs in ["1", "'1'", "'a'", "NULL"] {
                let expression = format!("(SELECT {projected}) {op} {rhs}");
                let expected = q(
                    &c,
                    &format!("SELECT {expression} AS v FROM native d ORDER BY d.n"),
                )
                .rows;
                let sql = format!("SELECT {expression} AS v FROM docs d ORDER BY d.n");
                assert_eq!(q(&c, &sql).rows, expected, "{expression}");
                assert_eq!(
                    c.profile_select(&sql, &Parameters::new())
                        .unwrap()
                        .result
                        .rows,
                    expected,
                    "profile {expression}"
                );
            }
        }
    }
    assert_eq!(
        q(
            &c,
            "SELECT (SELECT d.n FROM native d WHERE d.n=2) FROM docs d"
        )
        .rows,
        vec![vec![Value::Integer(2)]; 5]
    );
}

#[test]
fn correlated_native_ordering_matches_native_rows_and_nulls() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE native(n)");
    q(&c, "INSERT INTO docs(n) VALUES(1),(2),(NULL)");
    q(&c, "INSERT INTO native VALUES(1),(2),(NULL)");
    for ordering in [
        "n+d.n",
        "n*d.n DESC",
        "n+d.n DESC NULLS LAST",
        "abs(n-d.n),n DESC",
    ] {
        for expr in [
            format!("(SELECT n FROM native ORDER BY {ordering} LIMIT 1)"),
            format!("d.n IN (SELECT n FROM native ORDER BY {ordering} LIMIT 1)"),
            format!("EXISTS(SELECT n FROM native ORDER BY {ordering} LIMIT 1)"),
        ] {
            let expected = q(
                &c,
                &format!("SELECT {expr} AS v FROM native d ORDER BY d.n"),
            )
            .rows;
            let sql = format!("SELECT {expr} AS v FROM docs d ORDER BY d.n");
            assert_eq!(q(&c, &sql).rows, expected, "{expr}");
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected
            );
        }
    }
}

#[test]
fn correlated_projection_sort_aliases_use_logical_values() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE native(n)",
        "CREATE TABLE lookup(n)",
        "INSERT INTO docs(n) VALUES(0),(1)",
        "INSERT INTO native VALUES(0),(1)",
        "INSERT INTO lookup VALUES(2),(10),(-1)",
    ] {
        q(&c, sql);
    }
    for order in [
        "x",
        "x DESC",
        "1",
        "1 DESC",
        "(x) DESC",
        "x COLLATE BINARY DESC",
        "(1) DESC",
        "x DESC,n",
        "x+0 DESC",
        "abs(x) DESC",
        "n,x DESC",
    ] {
        for projection in [
            "n+d.n",
            "CASE WHEN d.n>=0 THEN n ELSE d.n END",
            "coalesce(NULL,n,d.n)",
        ] {
            for limit in ["0", "1", "1 OFFSET 1", "1 OFFSET 2"] {
                let expr = format!(
                    "(SELECT {projection} AS x FROM lookup ORDER BY {order} LIMIT {limit})"
                );
                assert_eq!(
                    q(&c, &format!("SELECT {expr} FROM docs d ORDER BY d.n")).rows,
                    q(&c, &format!("SELECT {expr} FROM native d ORDER BY d.n")).rows,
                    "{projection}: {order} LIMIT {limit}"
                );
            }
        }
    }
}

#[test]
fn mixed_distinct_correlated_typed_ordering_matches_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE native(n)",
        "CREATE TABLE lookup(n)",
        "INSERT INTO docs(n) VALUES(1),(2),(NULL)",
        "INSERT INTO native VALUES(1),(2),(NULL)",
        "INSERT INTO lookup VALUES(1),(2),(10),(2),(NULL),(NULL)",
    ] {
        q(&c, sql);
    }
    for projection in [
        "CASE WHEN d.n>0 THEN 1 ELSE d.n END",
        "CASE WHEN n>1 THEN 10 ELSE d.n END",
    ] {
        for order in ["x,n", "x DESC,n DESC", "n DESC,x", "x,abs(x) DESC"] {
            for limit in ["0", "1", "1 OFFSET 1", "1 OFFSET 2"] {
                let source = format!(
                    "SELECT DISTINCT {projection} AS x FROM lookup ORDER BY {order} LIMIT {limit}"
                );
                for expr in [
                    format!("({source})"),
                    format!("d.n IN ({source})"),
                    format!("d.n NOT IN ({source})"),
                    format!("EXISTS({source})"),
                ] {
                    let expected = q(&c, &format!("SELECT {expr} FROM native d ORDER BY d.n")).rows;
                    let sql = format!("SELECT {expr} FROM docs d ORDER BY d.n");
                    assert_eq!(q(&c, &sql).rows, expected, "{sql}");
                    assert_eq!(
                        c.profile_select(&sql, &Parameters::new())
                            .unwrap()
                            .result
                            .rows,
                        expected,
                        "profile {sql}"
                    );
                }
            }
        }
    }
}

#[test]
fn correlated_typed_sort_membership_matches_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE native(n)",
        "CREATE TABLE lookup(n)",
        "INSERT INTO docs(n) VALUES(-1),(2),(10),(NULL),('a'),('B'),('c')",
        "INSERT INTO native VALUES(-1),(2),(10),(NULL),('a'),('B'),('c')",
        "INSERT INTO lookup VALUES(-1),(2),(10),(NULL),('a'),('B'),('c')",
    ] {
        q(&c, sql);
    }
    for order in [
        "x DESC NULLS LAST",
        "x,n DESC",
        "1 DESC",
        "x COLLATE NOCASE DESC",
    ] {
        for limit in ["0", "1", "2 OFFSET 1"] {
            for projection in [
                "CASE WHEN d.n IS NULL THEN NULL ELSE n END",
                "coalesce(n,d.n)",
            ] {
                let source =
                    format!("SELECT {projection} AS x FROM lookup ORDER BY {order} LIMIT {limit}");
                for expr in [
                    format!("d.n IN ({source})"),
                    format!("d.n NOT IN ({source})"),
                    format!("EXISTS({source})"),
                ] {
                    let expected = q(&c, &format!("SELECT {expr} FROM native d ORDER BY d.n")).rows;
                    let sql = format!("SELECT {expr} FROM docs d ORDER BY d.n");
                    assert_eq!(q(&c, &sql).rows, expected, "{sql}");
                    assert_eq!(
                        c.profile_select(&sql, &Parameters::new())
                            .unwrap()
                            .result
                            .rows,
                        expected,
                        "profile {sql}"
                    );
                }
            }
        }
    }
}

#[test]
fn correlated_sorted_projection_preserves_record_and_boolean_values() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {id:docs:a,flag:true}");
    q(&c, "CREATE TABLE lookup(n)");
    q(&c, "INSERT INTO lookup VALUES(2),(1)");
    for field in ["id", "flag"] {
        for order in ["x", "x DESC,n"] {
            let projection = format!("SELECT d.{field} AS x FROM lookup ORDER BY {order} LIMIT 1");
            let sql = format!("SELECT ({projection}),d.{field} IN ({projection}) FROM docs d");
            let value = q(&c, &format!("SELECT {field} FROM docs")).rows[0][0].clone();
            let expected = vec![vec![value, Value::Integer(1)]];
            assert_eq!(q(&c, &sql).rows, expected, "{sql}");
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected,
                "profile {sql}"
            );
            let projection = format!(
                "SELECT DISTINCT d.{field} AS x FROM lookup ORDER BY {order} LIMIT 1 OFFSET 1"
            );
            let sql = format!("SELECT ({projection}),d.{field} IN ({projection}) FROM docs d");
            let expected = vec![vec![Value::Null, Value::Integer(0)]];
            assert_eq!(q(&c, &sql).rows, expected, "{sql}");
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected,
                "profile {sql}"
            );
        }
    }
}

#[test]
fn correlated_sort_expression_aliases_match_native_name_precedence() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE native(n)",
        "CREATE TABLE lookup(n,x)",
        "INSERT INTO docs(n) VALUES(0)",
        "INSERT INTO native VALUES(0)",
        "INSERT INTO lookup VALUES(2,10),(10,2)",
        "CREATE VIEW lookup_view AS SELECT * FROM lookup",
    ] {
        q(&c, sql);
    }
    for source in ["lookup", "lookup_view", "lookup_cte"] {
        for order in ["x+0 DESC", "abs(x) DESC", "x,x+0 DESC"] {
            let expr=format!("(SELECT CASE WHEN d.n=0 THEN n ELSE d.n END AS x FROM {source} ORDER BY {order} LIMIT 1)");
            let expected = q(
                &c,
                &format!("WITH lookup_cte AS (SELECT * FROM lookup) SELECT {expr} FROM native d"),
            )
            .rows;
            let sql =
                format!("WITH lookup_cte AS (SELECT * FROM lookup) SELECT {expr} FROM docs d");
            assert_eq!(q(&c, &sql).rows, expected, "{sql}");
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected,
                "profile {sql}"
            );
        }
    }
}

#[test]
fn correlated_sort_alias_writes_preserve_atomic_indexes_and_retry() {
    for outer in [false, true] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE docs",
            "INSERT INTO docs(n) VALUES(1),(2)",
            "CREATE UNIQUE INDEX docs_n ON docs(n)",
            "CREATE TABLE lookup(n)",
            "INSERT INTO lookup VALUES(2),(10),(-1)",
            "CREATE TABLE prior(n)",
        ] {
            q(&c, sql);
        }
        if outer {
            q(&c, "BEGIN");
        }
        q(&c, "INSERT INTO prior VALUES(9)");
        let sql = "UPDATE docs AS d SET n=(SELECT CASE WHEN d.n>0 THEN n+d.n*$factor ELSE d.n END AS x FROM lookup ORDER BY abs(x) DESC LIMIT 1) RETURNING n";
        let params = Parameters::from([("$factor".into(), Value::Integer(0))]);
        let state = if outer {
            romedb::TransactionState::Active
        } else {
            romedb::TransactionState::Autocommit
        };
        let report = c.execute_report(sql, &params);
        assert!(report.result.is_err());
        assert_eq!(report.transaction_before, state);
        assert_eq!(report.transaction_after, state);
        assert_eq!(
            q(&c, "SELECT n FROM docs ORDER BY n").rows,
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        );
        assert_eq!(
            q(&c, "SELECT n FROM prior").rows,
            vec![vec![Value::Integer(9)]]
        );
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
        let params = Parameters::from([("$factor".into(), Value::Integer(1))]);
        let report = c.execute_report(sql, &params);
        assert_eq!(report.transaction_before, state);
        assert_eq!(report.transaction_after, state);
        let result = report.result.unwrap();
        assert_eq!(result.affected, 2);
        let mut values = result
            .rows
            .into_iter()
            .map(|row| match row[0] {
                Value::Integer(n) => n,
                _ => panic!("integer RETURNING value"),
            })
            .collect::<Vec<_>>();
        values.sort();
        assert_eq!(values, vec![11, 12]);
        assert_eq!(
            q(&c, "SELECT n FROM docs WHERE n=12").rows,
            vec![vec![Value::Integer(12)]]
        );
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
        if outer {
            q(&c, "ROLLBACK");
            assert!(q(&c, "SELECT n FROM prior").rows.is_empty());
            assert_eq!(
                q(&c, "SELECT n FROM docs ORDER BY n").rows,
                vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
            );
            c.check_collection_integrity("docs", Default::default())
                .unwrap();
        }
    }
}

#[test]
fn correlated_distinct_numeric_equality_matches_native() {
    for inputs in [
        "(1),(1.0),(2)",
        "(1.0),(1),(2)",
        "(NULL),(NULL),(1),(1.0),(2)",
    ] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE docs",
            "CREATE TABLE native(n)",
            "CREATE TABLE lookup(n)",
            "INSERT INTO docs(n) VALUES(1)",
            "INSERT INTO native VALUES(1)",
        ] {
            q(&c, sql);
        }
        q(&c, &format!("INSERT INTO lookup VALUES{inputs}"));
        for order in ["x", "x,n", "x DESC,n", "n", "n DESC"] {
            for offset in 0..4 {
                let source=format!("SELECT DISTINCT CASE WHEN d.n>0 THEN n ELSE d.n END AS x FROM lookup ORDER BY {order} LIMIT 1 OFFSET {offset}");
                for expr in [format!("({source})"), format!("d.n IN ({source})")] {
                    let expected = q(&c, &format!("SELECT {expr} FROM native d")).rows;
                    let sql = format!("SELECT {expr} FROM docs d");
                    assert_eq!(q(&c, &sql).rows, expected, "{inputs}: {sql}");
                    assert_eq!(
                        c.profile_select(&sql, &Parameters::new())
                            .unwrap()
                            .result
                            .rows,
                        expected,
                        "profile {inputs}: {sql}"
                    );
                }
            }
        }
    }
}

#[test]
fn correlated_distinct_projection_collation_matches_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE native(n)",
        "CREATE TABLE lookup(n)",
        "INSERT INTO docs(n) VALUES(1)",
        "INSERT INTO native VALUES(1)",
        "INSERT INTO lookup VALUES('a'),('A'),('b')",
    ] {
        q(&c, sql);
    }
    for collation in ["BINARY", "NOCASE"] {
        for projection in [
            format!("(CASE WHEN d.n>0 THEN n ELSE d.n END) COLLATE {collation}"),
            format!("CASE WHEN d.n>0 THEN n COLLATE {collation} ELSE d.n END"),
        ] {
            for order in [
                "x",
                "x COLLATE BINARY DESC",
                "x,n DESC",
                "n",
                "n COLLATE NOCASE DESC,n COLLATE BINARY",
            ] {
                for offset in 0..3 {
                    let source=format!("SELECT DISTINCT {projection} AS x FROM lookup ORDER BY {order} LIMIT 1 OFFSET {offset}");
                    for expr in [
                        format!("({source})"),
                        format!("'A' IN ({source})"),
                        format!("'a' COLLATE NOCASE IN ({source})"),
                        format!("EXISTS ({source})"),
                    ] {
                        let expected = q(&c, &format!("SELECT {expr} FROM native d")).rows;
                        let sql = format!("SELECT {expr} FROM docs d");
                        assert_eq!(q(&c, &sql).rows, expected, "{sql}");
                        assert_eq!(
                            c.profile_select(&sql, &Parameters::new())
                                .unwrap()
                                .result
                                .rows,
                            expected,
                            "profile {sql}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn correlated_distinct_bound_pagination_matches_literal_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE native(n)",
        "CREATE TABLE lookup(n)",
        "INSERT INTO docs(n) VALUES(1)",
        "INSERT INTO native VALUES(1)",
        "INSERT INTO lookup VALUES(1),(1.0),(2)",
    ] {
        q(&c, sql);
    }
    for limit in [0, 1, 2, -1] {
        for offset in [0, 1, 4] {
            let params = Parameters::from([
                ("$limit".into(), Value::Integer(limit)),
                ("$offset".into(), Value::Integer(offset)),
            ]);
            let source="SELECT DISTINCT CASE WHEN d.n>0 THEN n ELSE d.n END AS x FROM lookup ORDER BY x,n LIMIT $limit OFFSET $offset";
            for expr in [
                format!("({source})"),
                format!("d.n IN ({source})"),
                format!("EXISTS({source})"),
            ] {
                // The pinned native scalar compiler discards a bound LIMIT.
                // Literal native pagination is the reference for bound values.
                let literal = expr
                    .replace("$limit", &limit.to_string())
                    .replace("$offset", &offset.to_string());
                let expected = q(&c, &format!("SELECT {literal} FROM native d")).rows;
                let sql = format!("SELECT {expr} FROM docs d");
                assert_eq!(
                    c.execute(&sql, &params).unwrap().rows,
                    expected,
                    "{sql}: {params:?}"
                );
                assert_eq!(
                    c.profile_select(&sql, &params).unwrap().result.rows,
                    expected,
                    "profile {sql}: {params:?}"
                );
            }
        }
    }
}

#[test]
fn correlated_bound_pagination_errors_preserve_writes_and_retry() {
    for outer in [false, true] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE docs",
            "INSERT INTO docs(n) VALUES(1),(2)",
            "CREATE UNIQUE INDEX docs_n ON docs(n)",
            "CREATE TABLE lookup(n)",
            "INSERT INTO lookup VALUES(10),(10.0),(20)",
            "CREATE TABLE prior(n)",
        ] {
            q(&c, sql);
        }
        if outer {
            q(&c, "BEGIN");
        }
        q(&c, "INSERT INTO prior VALUES(9)");
        let state = c.transaction_state();
        let before = q(&c, "SELECT id,n FROM docs ORDER BY n").rows;
        let sql="UPDATE docs AS d SET n=(SELECT DISTINCT CASE WHEN d.n>0 THEN n+d.n ELSE d.n END AS x FROM lookup ORDER BY x,n LIMIT $limit OFFSET $offset) RETURNING n";
        let invalid = [
            Value::Null,
            Value::Number(1.5),
            Value::String("invalid".into()),
            Value::Array(vec![]),
        ];
        for name in ["$limit", "$offset"] {
            for value in &invalid {
                let mut params = Parameters::from([
                    ("$limit".into(), Value::Integer(1)),
                    ("$offset".into(), Value::Integer(0)),
                ]);
                params.insert(name.into(), value.clone());
                let report = c.execute_report(sql, &params);
                assert!(report.result.is_err(), "{name}: {value:?}");
                assert_eq!(report.transaction_before, state);
                assert_eq!(report.transaction_after, state);
                assert_eq!(q(&c, "SELECT id,n FROM docs ORDER BY n").rows, before);
                assert_eq!(
                    q(&c, "SELECT n FROM prior").rows,
                    vec![vec![Value::Integer(9)]]
                );
                c.check_collection_integrity("docs", Default::default())
                    .unwrap();
            }
        }
        assert_eq!(
            c.execute(sql, &Parameters::new()).unwrap_err().code(),
            "FDB_PARAMETER"
        );
        assert_eq!(c.transaction_state(), state);
        let result = c
            .execute(
                sql,
                &Parameters::from([
                    ("$limit".into(), Value::Integer(1)),
                    ("$offset".into(), Value::Integer(0)),
                ]),
            )
            .unwrap();
        assert_eq!(result.affected, 2);
        assert_eq!(
            q(&c, "SELECT n FROM docs ORDER BY n").rows,
            vec![vec![Value::Integer(11)], vec![Value::Integer(12)]]
        );
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
        if outer {
            q(&c, "ROLLBACK");
            assert_eq!(q(&c, "SELECT id,n FROM docs ORDER BY n").rows, before);
            assert!(q(&c, "SELECT n FROM prior").rows.is_empty());
            c.check_collection_integrity("docs", Default::default())
                .unwrap();
        }
    }
}

#[test]
fn correlated_native_bound_pagination_matches_literal_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE native(n)",
        "CREATE TABLE lookup(n)",
        "INSERT INTO docs(n) VALUES(1),(2)",
        "INSERT INTO native VALUES(1),(2)",
        "INSERT INTO lookup VALUES(1),(2),(10)",
    ] {
        q(&c, sql);
    }
    for projection in [
        "n",
        "n+d.n",
        "CAST(n+d.n AS TEXT)",
        "CASE WHEN d.n>0 THEN n ELSE d.n END",
    ] {
        for limit in [0, 1, 2, -1] {
            for offset in [-2, 0, 1, 2, 4] {
                let params = Parameters::from([
                    ("$limit".into(), Value::Integer(limit)),
                    ("$offset".into(), Value::Integer(offset)),
                ]);
                let source=format!("SELECT {projection} FROM lookup WHERE n>=d.n ORDER BY n LIMIT $limit OFFSET $offset");
                for expr in [
                    format!("({source})"),
                    format!("d.n IN ({source})"),
                    format!("EXISTS({source})"),
                ] {
                    let literal = expr
                        .replace("$limit", &limit.to_string())
                        .replace("$offset", &offset.to_string());
                    let expected =
                        q(&c, &format!("SELECT {literal} FROM native d ORDER BY d.n")).rows;
                    let sql = format!("SELECT {expr} FROM docs d ORDER BY d.n");
                    assert_eq!(
                        c.execute(&sql, &params).unwrap().rows,
                        expected,
                        "{sql}: {params:?}"
                    );
                    assert_eq!(
                        c.profile_select(&sql, &params).unwrap().result.rows,
                        expected,
                        "profile {sql}: {params:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn correlated_pagination_reused_bindings_preserve_predicates() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE native(n)",
        "CREATE TABLE lookup(n)",
        "INSERT INTO docs(n) VALUES(1),(2)",
        "INSERT INTO native VALUES(1),(2)",
        "INSERT INTO lookup VALUES(1),(2),(3),(10)",
    ] {
        q(&c, sql);
    }
    for parameter in ["$count", "?1"] {
        for count in [0, 1, 2, 3] {
            let params = Parameters::from([(parameter.into(), Value::Integer(count))]);
            for pagination in [
                format!("LIMIT {parameter}"),
                format!("LIMIT 1 OFFSET {parameter}"),
            ] {
                let source = format!(
                    "SELECT n FROM lookup WHERE n>=d.n AND n>={parameter} ORDER BY n {pagination}"
                );
                for expr in [
                    format!("({source})"),
                    format!("d.n IN ({source})"),
                    format!("EXISTS({source})"),
                ] {
                    let literal = expr.replace(parameter, &count.to_string());
                    let expected =
                        q(&c, &format!("SELECT {literal} FROM native d ORDER BY d.n")).rows;
                    let sql = format!("SELECT {expr} FROM docs d ORDER BY d.n");
                    assert_eq!(
                        c.execute(&sql, &params).unwrap().rows,
                        expected,
                        "{sql}, count={count}"
                    );
                    assert_eq!(
                        c.profile_select(&sql, &params).unwrap().result.rows,
                        expected,
                        "profile {sql}, count={count}"
                    );
                    assert_eq!(
                        c.execute(&sql, &Parameters::new()).unwrap_err().code(),
                        "FDB_PARAMETER"
                    );
                }
            }
        }
    }
}

#[test]
fn correlated_float_pagination_matches_integer_values() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE lookup(n)",
        "INSERT INTO docs(n) VALUES(1),(2)",
        "INSERT INTO lookup VALUES(1),(2),(3)",
    ] {
        q(&c, sql);
    }
    for limit in [0, 1, 2, -1, i64::MIN + 1024, 9_223_372_036_854_774_784] {
        for offset in [-2, 0, 1, 2] {
            let floats = Parameters::from([
                ("$limit".into(), Value::Number(limit as f64)),
                ("$offset".into(), Value::Number(offset as f64)),
            ]);
            let integers = Parameters::from([
                ("$limit".into(), Value::Integer(limit)),
                ("$offset".into(), Value::Integer(offset)),
            ]);
            let source = "SELECT n FROM lookup WHERE n>=d.n ORDER BY n LIMIT $limit OFFSET $offset";
            for expr in [
                format!("({source})"),
                format!("d.n IN ({source})"),
                format!("EXISTS({source})"),
            ] {
                let sql = format!("SELECT {expr} FROM docs d ORDER BY d.n");
                let expected = c.execute(&sql, &integers).unwrap().rows;
                assert_eq!(
                    c.execute(&sql, &floats).unwrap().rows,
                    expected,
                    "{sql}: {floats:?}"
                );
                assert_eq!(
                    c.profile_select(&sql, &floats).unwrap().result.rows,
                    expected,
                    "profile {sql}: {floats:?}"
                );
            }
        }
    }
    for value in [i64::MIN as f64, i64::MAX as f64] {
        let params = Parameters::from([("$limit".into(), Value::Number(value))]);
        for expr in [
            "(SELECT n FROM lookup WHERE n>=d.n ORDER BY n LIMIT $limit)",
            "d.n IN (SELECT n FROM lookup WHERE n>=d.n ORDER BY n LIMIT $limit)",
        ] {
            let sql = format!("SELECT {expr} FROM docs d");
            assert!(
                c.execute(&sql, &params).is_err(),
                "int64 real endpoint: {sql}, {value}"
            );
            assert!(
                c.profile_select(&sql, &params).is_err(),
                "profile int64 real endpoint: {sql}, {value}"
            );
        }
    }
}

#[test]
fn correlated_float_pagination_preserves_reused_parameter_type() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE lookup(n)",
        "INSERT INTO docs(n) VALUES(1),(2)",
        "INSERT INTO lookup VALUES(1),(2),(3)",
    ] {
        q(&c, sql);
    }
    for parameter in ["$count", "?1"] {
        for count in [0.0, 1.0, 2.0] {
            let params = Parameters::from([(parameter.into(), Value::Number(count))]);
            let source = format!(
                "SELECT n FROM lookup WHERE n>=d.n AND n>={parameter} ORDER BY n LIMIT {parameter}"
            );
            let sql = format!(
                "SELECT {parameter},typeof({parameter}),d.n IN ({source}) FROM docs d ORDER BY d.n"
            );
            let expected_members = if count == 0.0 {
                [0, 0]
            } else if count == 1.0 {
                [1, 1]
            } else {
                [0, 1]
            };
            let expected = expected_members
                .into_iter()
                .map(|member| {
                    vec![
                        Value::Number(count),
                        Value::String("real".into()),
                        Value::Integer(member),
                    ]
                })
                .collect::<Vec<_>>();
            assert_eq!(
                c.execute(&sql, &params).unwrap().rows,
                expected,
                "{sql}, count={count}"
            );
            assert_eq!(
                c.profile_select(&sql, &params).unwrap().result.rows,
                expected,
                "profile {sql}, count={count}"
            );
        }
    }
}

#[test]
fn unsorted_correlated_distinct_exhausts_logical_values() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE native(n)",
        "CREATE TABLE lookup(n)",
        "INSERT INTO docs(n) VALUES(1),(2)",
        "INSERT INTO native VALUES(1),(2)",
        "INSERT INTO lookup VALUES(1),(1.0),(2),(2.0),(NULL),(NULL)",
    ] {
        q(&c, sql);
    }
    // No ordering is promised; after three logical values every page is empty.
    for offset in [3, 4, 6] {
        let source = format!("SELECT DISTINCT CASE WHEN d.n>0 THEN n ELSE d.n END FROM lookup LIMIT 1 OFFSET {offset}");
        for expr in [
            format!("({source})"),
            format!("d.n IN ({source})"),
            format!("EXISTS ({source})"),
        ] {
            let expected = q(&c, &format!("SELECT {expr} FROM native d ORDER BY d.n")).rows;
            let sql = format!("SELECT {expr} FROM docs d ORDER BY d.n");
            assert_eq!(q(&c, &sql).rows, expected, "{sql}");
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected,
                "profile {sql}"
            );
        }
    }
}

#[test]
fn source_sorted_distinct_bound_pages_and_writes_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE native(n)",
        "CREATE TABLE lookup(n)",
        "INSERT INTO docs(n) VALUES(1),(2)",
        "INSERT INTO native VALUES(1),(2)",
        "INSERT INTO lookup VALUES(10),(10.0),(20),(20.0)",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
    ] {
        q(&c, sql);
    }
    let source = "SELECT DISTINCT CASE WHEN d.n>0 THEN n+d.n ELSE d.n END AS x FROM lookup ORDER BY n LIMIT $limit OFFSET $offset";
    for limit in [0, 1, -1] {
        for offset in [0, 1, 2, 4] {
            for real in [false, true] {
                let value = |n| {
                    if real {
                        Value::Number(n as f64)
                    } else {
                        Value::Integer(n)
                    }
                };
                let params = Parameters::from([
                    ("$limit".into(), value(limit)),
                    ("$offset".into(), value(offset)),
                ]);
                for expr in [
                    format!("({source})"),
                    format!("21 IN ({source})"),
                    format!("EXISTS ({source})"),
                ] {
                    let native = format!("SELECT {expr} FROM native d ORDER BY d.n")
                        .replace("$limit", &limit.to_string())
                        .replace("$offset", &offset.to_string());
                    let expected = q(&c, &native).rows;
                    let sql = format!("SELECT {expr} FROM docs d ORDER BY d.n");
                    assert_eq!(
                        c.execute(&sql, &params).unwrap().rows,
                        expected,
                        "{sql}: {params:?}"
                    );
                    assert_eq!(
                        c.profile_select(&sql, &params).unwrap().result.rows,
                        expected,
                        "profile {sql}: {params:?}"
                    );
                }
            }
        }
    }
    let before = q(&c, "SELECT id,n FROM docs ORDER BY n").rows;
    q(&c, "BEGIN");
    let result = c
        .execute(
            &format!("UPDATE docs AS d SET n=({source}) RETURNING n"),
            &Parameters::from([
                ("$limit".into(), Value::Integer(1)),
                ("$offset".into(), Value::Integer(1)),
            ]),
        )
        .unwrap();
    assert_eq!(result.affected, 2);
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(21)], vec![Value::Integer(22)]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(q(&c, "SELECT id,n FROM docs ORDER BY n").rows, before);
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn correlated_composite_counts_match_native_presence_and_empty_sources() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE baseline(n,v)",
        "CREATE TABLE lookup(k)",
        "INSERT INTO lookup VALUES(1),(2)",
        "INSERT INTO docs {n:1,v:[]}",
        "INSERT INTO docs {n:2,v:null}",
        "INSERT INTO docs {n:3,v:{a:1}}",
        "INSERT INTO baseline VALUES(1,x'01'),(2,NULL),(3,x'02')",
    ] {
        q(&c, sql);
    }
    for argument in ["d.v", "CASE WHEN k>0 THEN d.v END", "coalesce(d.v,NULL)"] {
        for predicate in ["1", "k>=d.n", "0"] {
            let inner = format!("SELECT count({argument}) FROM lookup WHERE {predicate}");
            let sql = |table| format!("SELECT n,({inner}) FROM {table} d ORDER BY n");
            let expected = q(&c, &sql("baseline")).rows;
            assert_eq!(
                q(&c, &sql("docs")).rows,
                expected,
                "{argument}: {predicate}"
            );
            assert_eq!(
                c.profile_select(&sql("docs"), &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected,
                "profile {argument}: {predicate}"
            );
        }
    }
    q(&c, "CREATE UNIQUE INDEX docs_n ON docs(n)");
    let before = q(&c, "SELECT id,n FROM docs ORDER BY n").rows;
    q(&c, "BEGIN");
    assert_eq!(
        q(
            &c,
            "UPDATE docs AS d SET n=n*10+(SELECT count(d.v) FROM lookup)"
        )
        .affected,
        3
    );
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![
            vec![Value::Integer(12)],
            vec![Value::Integer(20)],
            vec![Value::Integer(32)]
        ]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(q(&c, "SELECT id,n FROM docs ORDER BY n").rows, before);
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn inner_collection_correlation_preserves_outer_record_identity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE users",
        "CREATE TABLE posts",
        "INSERT INTO users {id:users:a,n:1}",
        "INSERT INTO users {id:users:b,n:2}",
        "INSERT INTO posts {owner:users:a,n:3}",
    ] {
        q(&c, sql);
    }
    for sql in [
        "SELECT n FROM users u WHERE EXISTS(SELECT 1 FROM posts p WHERE p.owner=u.id)",
        "SELECT n FROM users u WHERE u.id IN(SELECT p.owner FROM posts p WHERE p.n>u.n)",
    ] {
        let expected = vec![vec![Value::Integer(1)]];
        assert_eq!(q(&c, sql).rows, expected, "{sql}");
        assert_eq!(
            c.profile_select(sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected,
            "profile {sql}"
        );
    }
    assert_eq!(
        q(
            &c,
            "SELECT n,(SELECT count(*) FROM posts p WHERE p.owner=u.id) FROM users u ORDER BY n"
        )
        .rows,
        vec![
            vec![Value::Integer(1), Value::Integer(1)],
            vec![Value::Integer(2), Value::Integer(0)]
        ]
    );
}

#[test]
fn inner_collection_correlation_preserves_types_shadowing_and_write_rollback() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "INSERT INTO docs {id:docs:a,n:1,v:[1],meta:{key:3}}",
        "INSERT INTO docs {id:docs:b,n:2,v:null,meta:{key:0}}",
        "INSERT INTO links {owner:docs:a,n:3}",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
    ] {
        q(&c, sql);
    }
    for field in ["id", "v", "meta", "meta.key"] {
        let sql = format!("SELECT (SELECT d.{field} FROM links l WHERE l.owner=d.id) FROM docs d WHERE d.id=docs:a");
        let expected = q(&c, &format!("SELECT d.{field} FROM docs d WHERE id=docs:a")).rows;
        assert_eq!(q(&c, &sql).rows, expected, "{field}");
        assert_eq!(
            c.profile_select(&sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected
        );
    }
    assert_eq!(
        q(
            &c,
            "SELECT n FROM docs d WHERE EXISTS(SELECT 1 FROM links l WHERE l.n=d.meta.key)"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT n,(SELECT count(*) FROM links d WHERE d.n=3) FROM docs d ORDER BY n"
        )
        .rows,
        vec![
            vec![Value::Integer(1), Value::Integer(1)],
            vec![Value::Integer(2), Value::Integer(1)]
        ]
    );
    let before = q(&c, "SELECT id,n FROM docs ORDER BY n").rows;
    q(&c, "BEGIN");
    q(
        &c,
        "UPDATE docs AS d SET n=n*10+(SELECT count(*) FROM links l WHERE l.owner=d.id)",
    );
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(11)], vec![Value::Integer(20)]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(q(&c, "SELECT id,n FROM docs ORDER BY n").rows, before);
}

#[test]
fn inner_collection_correlation_accepts_typed_derived_and_cte_outer_fields() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "INSERT INTO docs {id:docs:a,n:1,v:[1],meta:{key:3}}",
        "INSERT INTO docs {id:docs:b,n:2,v:null,meta:{key:0}}",
        "INSERT INTO links {owner:docs:a,n:3}",
    ] {
        q(&c, sql);
    }
    for (prefix, source) in [
        ("", "(SELECT id,n,v,meta FROM docs) d"),
        (
            "WITH outer_docs AS (SELECT id,n,v,meta FROM docs) ",
            "outer_docs d",
        ),
    ] {
        for predicate in ["l.owner=d.id", "l.n=d.meta.key"] {
            let sql = format!("{prefix}SELECT d.n,(SELECT count(*) FROM links l WHERE {predicate}) FROM {source} ORDER BY d.n");
            let expected = vec![
                vec![Value::Integer(1), Value::Integer(1)],
                vec![Value::Integer(2), Value::Integer(0)],
            ];
            assert_eq!(q(&c, &sql).rows, expected, "{sql}");
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected
            );
        }
        for predicate in [
            "EXISTS(SELECT d.id FROM links l WHERE l.owner=d.id)",
            "d.id IN(SELECT l.owner FROM links l WHERE l.n>d.n)",
            "(SELECT l.n FROM links l WHERE l.owner=d.id ORDER BY d.n LIMIT 1)=3",
        ] {
            let sql = format!("{prefix}SELECT d.n FROM {source} WHERE {predicate}");
            assert_eq!(q(&c, &sql).rows, vec![vec![Value::Integer(1)]], "{sql}");
        }
        for field in ["id", "v", "meta", "meta.key"] {
            let sql = format!("{prefix}SELECT (SELECT d.{field} FROM links l WHERE l.owner=d.id) FROM {source} WHERE d.n=1");
            assert_eq!(
                q(&c, &sql).rows,
                q(&c, &format!("SELECT d.{field} FROM docs d WHERE n=1")).rows,
                "{sql}"
            );
        }
    }
}

#[test]
fn correlated_derived_parameters_preserve_binary_and_atomic_insert_validation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "CREATE TABLE output",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO links {owner:docs:a}",
        "DEFINE FIELD v ON output TYPE integer",
    ] {
        q(&c, sql);
    }
    let value = Value::Binary(b"FDB\x01payload".to_vec());
    let params = Parameters::from([("$value".into(), value.clone())]);
    let sql = "SELECT (SELECT d.payload FROM links l WHERE l.owner=d.id) FROM (SELECT id,$value AS payload FROM docs) d";
    assert_eq!(
        c.execute(sql, &params).unwrap().rows,
        vec![vec![value.clone()]]
    );
    assert_eq!(
        c.profile_select(sql, &params).unwrap().result.rows,
        vec![vec![value]]
    );
    q(&c, "BEGIN");
    q(&c, "INSERT INTO output {v:7}");
    assert!(c
        .execute(&format!("INSERT INTO output(v) {sql}"), &params)
        .is_err());
    assert_eq!(
        q(&c, "SELECT v FROM output").rows,
        vec![vec![Value::Integer(7)]]
    );
    let params = Parameters::from([("$value".into(), Value::Integer(8))]);
    assert_eq!(
        c.execute(&format!("INSERT INTO output(v) {sql}"), &params)
            .unwrap()
            .affected,
        1
    );
    c.check_collection_integrity("output", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT v FROM output").rows.is_empty());
}

#[test]
fn correlated_collection_having_retains_unprojected_record_keys() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "INSERT INTO links {owner:docs:a,n:3}",
        "INSERT INTO links {owner:docs:a,n:4}",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
    ] {
        q(&c, sql);
    }
    for source in ["docs d", "(SELECT id,n FROM docs) d"] {
        let sql = format!("SELECT n,(SELECT count(*) FROM links l GROUP BY l.owner HAVING l.owner=d.id) FROM {source} ORDER BY n");
        let expected = vec![
            vec![Value::Integer(1), Value::Integer(2)],
            vec![Value::Integer(2), Value::Null],
        ];
        assert_eq!(q(&c, &sql).rows, expected);
        assert_eq!(
            c.profile_select(&sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected
        );
        assert_eq!(q(&c, &format!("SELECT n FROM {source} WHERE EXISTS(SELECT count(*) FROM links l GROUP BY l.owner HAVING l.owner=d.id)")).rows, vec![vec![Value::Integer(1)]]);
    }
    let before = q(&c, "SELECT id,n FROM docs ORDER BY n").rows;
    q(&c, "BEGIN");
    q(&c, "UPDATE docs AS d SET n=n*10+coalesce((SELECT count(*) FROM links l GROUP BY l.owner HAVING l.owner=d.id),0)");
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(12)], vec![Value::Integer(20)]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(q(&c, "SELECT id,n FROM docs ORDER BY n").rows, before);
}

#[test]
fn inner_derived_collection_sources_correlate_outer_typed_fields() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "INSERT INTO docs {id:docs:a,n:1,v:[1]}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "INSERT INTO links {owner:docs:a,n:3}",
    ] {
        q(&c, sql);
    }
    for outer in ["docs d", "(SELECT id,n,v FROM docs) d"] {
        for inner in [
            "(SELECT owner,n FROM links) l",
            "(SELECT owner,n FROM links WHERE n>0 LIMIT 2) l",
        ] {
            let sql = format!("SELECT n,(SELECT count(*) FROM {inner} WHERE l.owner=d.id) FROM {outer} ORDER BY n");
            let expected = vec![
                vec![Value::Integer(1), Value::Integer(1)],
                vec![Value::Integer(2), Value::Integer(0)],
            ];
            assert_eq!(q(&c, &sql).rows, expected);
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected
            );
            for predicate in [
                format!("EXISTS(SELECT 1 FROM {inner} WHERE l.owner=d.id)"),
                format!("d.id IN(SELECT l.owner FROM {inner} WHERE l.n>d.n)"),
            ] {
                assert_eq!(
                    q(&c, &format!("SELECT n FROM {outer} WHERE {predicate}")).rows,
                    vec![vec![Value::Integer(1)]]
                );
            }
            assert_eq!(q(&c,&format!("SELECT (SELECT d.v FROM {inner} WHERE l.owner=d.id) FROM {outer} WHERE d.n=1")).rows,q(&c,"SELECT v FROM docs WHERE n=1").rows);
        }
    }
}

#[test]
fn inner_derived_correlated_updates_preserve_validation_and_rollback() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "DEFINE FIELD n ON docs TYPE integer CHECK(n<10)",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "INSERT INTO links {owner:docs:a,n:3}",
        "INSERT INTO links {owner:docs:b,n:20}",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
    ] {
        q(&c, sql);
    }
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {id:docs:prior,n:5}");
    let before = q(&c, "SELECT id,n FROM docs ORDER BY n").rows;
    let sql = "UPDATE docs AS d SET n=(SELECT l.n FROM (SELECT owner,n FROM links) l WHERE l.owner=d.id) WHERE EXISTS(SELECT 1 FROM (SELECT owner FROM links) l WHERE l.owner=d.id)";
    assert!(c.execute(sql, &Parameters::new()).is_err());
    assert_eq!(q(&c, "SELECT id,n FROM docs ORDER BY n").rows, before);
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "UPDATE links SET n=4 WHERE n=20");
    assert_eq!(q(&c, sql).affected, 2);
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![
            vec![Value::Integer(3)],
            vec![Value::Integer(4)],
            vec![Value::Integer(5)]
        ]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    assert_eq!(
        q(&c, "SELECT n FROM links ORDER BY n").rows,
        vec![vec![Value::Integer(3)], vec![Value::Integer(20)]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn pinned_outer_group_key_rejection_preserves_transaction_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "CREATE TABLE baseline(n)",
        "CREATE TABLE lookup(n)",
        "INSERT INTO docs {n:1}",
        "INSERT INTO docs {n:2}",
        "INSERT INTO links {n:3}",
        "INSERT INTO baseline VALUES(1),(2)",
        "INSERT INTO lookup VALUES(3)",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
    ] {
        q(&c, sql);
    }
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {n:4}");
    q(&c, "INSERT INTO baseline VALUES(4)");
    for (outer, inner) in [("docs", "links"), ("baseline", "lookup")] {
        let before = q(&c, &format!("SELECT n FROM {outer} ORDER BY n")).rows;
        for source in [format!("{outer} d"), format!("(SELECT n FROM {outer}) d")] {
            let sql =
                format!("SELECT n,(SELECT count(*) FROM {inner} l GROUP BY d.n) FROM {source}");
            let error = c.execute(&sql, &Parameters::new()).unwrap_err();
            assert_eq!(error.code(), "FDB_ENGINE", "{sql}: {error}");
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap_err()
                    .code(),
                "FDB_ENGINE"
            );
        }
        let sql =
            format!("UPDATE {outer} AS d SET n=(SELECT count(*) FROM {inner} l GROUP BY d.n)");
        assert_eq!(
            c.execute(&sql, &Parameters::new()).unwrap_err().code(),
            "FDB_ENGINE"
        );
        assert_eq!(
            q(&c, &format!("SELECT n FROM {outer} ORDER BY n")).rows,
            before
        );
        assert_eq!(q(&c,&format!("SELECT n,(SELECT count(*) FROM {inner} l WHERE l.n>d.n) FROM {outer} d ORDER BY n")).rows,
            vec![vec![Value::Integer(1),Value::Integer(1)],vec![Value::Integer(2),Value::Integer(1)],vec![Value::Integer(4),Value::Integer(0)]]);
    }
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    for table in ["docs", "baseline"] {
        assert_eq!(
            q(&c, &format!("SELECT n FROM {table} ORDER BY n")).rows,
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        );
    }
}

#[test]
fn local_collection_cte_consumers_correlate_outer_records() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "INSERT INTO docs {id:docs:a,n:1,v:[1]}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "INSERT INTO links {owner:docs:a,n:3}",
    ] {
        q(&c, sql);
    }
    for outer in ["docs d", "(SELECT id,n,v FROM docs) d"] {
        for definitions in [
            "x AS (SELECT owner,n FROM links)",
            "seed AS (SELECT owner,n FROM links),x AS (SELECT owner,n FROM seed)",
            "docs AS (SELECT owner,n FROM links),x AS (SELECT owner,n FROM docs)",
        ] {
            let inner = format!("WITH {definitions} SELECT count(*) FROM x WHERE x.owner=d.id");
            let sql = format!("SELECT n,({inner}) FROM {outer} ORDER BY n");
            let expected = vec![
                vec![Value::Integer(1), Value::Integer(1)],
                vec![Value::Integer(2), Value::Integer(0)],
            ];
            assert_eq!(q(&c, &sql).rows, expected);
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected
            );
            for predicate in [
                format!("EXISTS(WITH {definitions} SELECT 1 FROM x WHERE x.owner=d.id)"),
                format!("d.id IN(WITH {definitions} SELECT x.owner FROM x WHERE x.n>d.n)"),
            ] {
                assert_eq!(
                    q(&c, &format!("SELECT n FROM {outer} WHERE {predicate}")).rows,
                    vec![vec![Value::Integer(1)]]
                );
            }
        }
    }
}

#[test]
fn local_cte_correlated_updates_bind_values_and_preserve_atomicity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "DEFINE FIELD n ON docs TYPE integer CHECK(n<10)",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "INSERT INTO links {owner:docs:a,n:3}",
        "INSERT INTO links {owner:docs:b,n:4}",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
    ] {
        q(&c, sql);
    }
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {id:docs:prior,n:5}");
    let before = q(&c, "SELECT id,n FROM docs ORDER BY n").rows;
    let sql = "UPDATE docs AS d SET n=(WITH x AS (SELECT owner,n FROM links) SELECT x.n+$extra FROM x WHERE x.owner=d.id) WHERE EXISTS(WITH x AS (SELECT owner FROM links) SELECT 1 FROM x WHERE x.owner=d.id)";
    let invalid = Parameters::from([("$extra".into(), Value::Integer(7))]);
    assert!(c.execute(sql, &invalid).is_err());
    assert_eq!(q(&c, "SELECT id,n FROM docs ORDER BY n").rows, before);
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    let valid = Parameters::from([("$extra".into(), Value::Integer(0))]);
    assert_eq!(c.execute(sql, &valid).unwrap().affected, 2);
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![
            vec![Value::Integer(3)],
            vec![Value::Integer(4)],
            vec![Value::Integer(5)]
        ]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn collection_cte_definitions_correlate_outer_fields() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "INSERT INTO links {owner:docs:a,n:3}",
    ] {
        q(&c, sql);
    }
    for outer in ["docs d", "(SELECT id,n FROM docs) d"] {
        for predicate in ["l.owner=d.id", "l.n>d.n+1"] {
            for definitions in [
                format!("x AS (SELECT n FROM links l WHERE {predicate})"),
                format!(
                    "seed AS (SELECT n FROM links l WHERE {predicate}),x AS (SELECT n FROM seed)"
                ),
            ] {
                let sql = format!(
                    "SELECT n,(WITH {definitions} SELECT count(*) FROM x) FROM {outer} ORDER BY n"
                );
                let expected = vec![
                    vec![Value::Integer(1), Value::Integer(1)],
                    vec![Value::Integer(2), Value::Integer(0)],
                ];
                assert_eq!(q(&c, &sql).rows, expected, "{sql}");
                assert_eq!(
                    c.profile_select(&sql, &Parameters::new())
                        .unwrap()
                        .result
                        .rows,
                    expected
                );
            }
        }
    }
}

#[test]
fn correlated_cte_definitions_preserve_local_aliases_and_write_rollback() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "DEFINE FIELD n ON docs TYPE integer CHECK(n<10)",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "INSERT INTO links {owner:docs:a,n:3}",
        "INSERT INTO links {owner:docs:b,n:20}",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
    ] {
        q(&c, sql);
    }
    let shadow = "SELECT n,(WITH x AS (SELECT n FROM links d WHERE d.n=3) SELECT count(*) FROM x) FROM docs d ORDER BY n";
    assert_eq!(
        q(&c, shadow).rows,
        vec![
            vec![Value::Integer(1), Value::Integer(1)],
            vec![Value::Integer(2), Value::Integer(1)]
        ]
    );
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {id:docs:prior,n:5}");
    let before = q(&c, "SELECT id,n FROM docs ORDER BY n").rows;
    let sql = "UPDATE docs AS d SET n=(WITH x AS (SELECT n FROM links l WHERE l.owner=d.id) SELECT n FROM x) WHERE EXISTS(WITH x AS (SELECT n FROM links l WHERE l.owner=d.id) SELECT 1 FROM x)";
    assert!(c.execute(sql, &Parameters::new()).is_err());
    assert_eq!(q(&c, "SELECT id,n FROM docs ORDER BY n").rows, before);
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "UPDATE links SET n=4 WHERE n=20");
    assert_eq!(q(&c, sql).affected, 2);
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![
            vec![Value::Integer(3)],
            vec![Value::Integer(4)],
            vec![Value::Integer(5)]
        ]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    assert_eq!(
        q(&c, "SELECT n FROM links ORDER BY n").rows,
        vec![vec![Value::Integer(3)], vec![Value::Integer(20)]]
    );
}

#[test]
fn correlated_cte_exists_pages_and_empty_aggregates_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "CREATE TABLE baseline(n)",
        "CREATE TABLE lookup(n)",
        "INSERT INTO docs {n:1}",
        "INSERT INTO docs {n:4}",
        "INSERT INTO links {n:2}",
        "INSERT INTO links {n:3}",
        "INSERT INTO baseline VALUES(1),(4)",
        "INSERT INTO lookup VALUES(2),(3)",
    ] {
        q(&c, sql);
    }
    for projection in ["n", "count(*)"] {
        for page in [
            "",
            " LIMIT 0",
            " LIMIT 1",
            " LIMIT 1 OFFSET 1",
            " LIMIT -1 OFFSET 2",
        ] {
            for negate in ["", "NOT "] {
                // Keep the native reference as a scalar SELECT too: this avoids
                // the pinned direct-EXISTS correlated-CTE preparation defect.
                let sql = |outer, inner| {
                    let predicate = format!("{negate}EXISTS(WITH x AS (SELECT n FROM {inner} l WHERE l.n>d.n) SELECT {projection} FROM x{page})");
                    let predicate = if outer == "baseline" {
                        format!("(SELECT {predicate})")
                    } else {
                        predicate
                    };
                    format!("SELECT n,{predicate} FROM {outer} d ORDER BY n")
                };
                let expected = q(&c, &sql("baseline", "lookup")).rows;
                assert_eq!(
                    q(&c, &sql("docs", "links")).rows,
                    expected,
                    "{projection}: {page}: {negate}"
                );
                assert_eq!(
                    c.profile_select(&sql("docs", "links"), &Parameters::new())
                        .unwrap()
                        .result
                        .rows,
                    expected
                );
            }
        }
    }
}

#[test]
fn source_free_scalar_wrappers_retain_collection_correlation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "INSERT INTO links {owner:docs:a}",
    ] {
        q(&c, sql);
    }
    for outer in ["docs d", "(SELECT id,n FROM docs) d"] {
        for body in [
            "EXISTS(WITH x AS (SELECT owner FROM links l WHERE l.owner=d.id) SELECT 1 FROM x)",
            "(SELECT count(*) FROM links l WHERE l.owner=d.id)",
        ] {
            for wrapped in [
                format!("(SELECT {body})"),
                format!("(SELECT (SELECT {body}))"),
            ] {
                let sql = format!("SELECT n,{wrapped} FROM {outer} ORDER BY n");
                let expected = vec![
                    vec![Value::Integer(1), Value::Integer(1)],
                    vec![Value::Integer(2), Value::Integer(0)],
                ];
                assert_eq!(q(&c, &sql).rows, expected, "{sql}");
                assert_eq!(
                    c.profile_select(&sql, &Parameters::new())
                        .unwrap()
                        .result
                        .rows,
                    expected
                );
            }
        }
    }
}

#[test]
fn scalar_wrapper_correlated_updates_preserve_atomicity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "DEFINE FIELD n ON docs TYPE integer CHECK(n<10)",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "INSERT INTO links {owner:docs:a,n:3}",
        "INSERT INTO links {owner:docs:b,n:4}",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
    ] {
        q(&c, sql);
    }
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {id:docs:prior,n:5}");
    let before = q(&c, "SELECT id,n FROM docs ORDER BY n").rows;
    let sql = "UPDATE docs AS d SET n=(SELECT (SELECT l.n+$extra FROM links l WHERE l.owner=d.id)) WHERE (SELECT EXISTS(WITH x AS (SELECT owner FROM links l WHERE l.owner=d.id) SELECT 1 FROM x))";
    let invalid = Parameters::from([("$extra".into(), Value::Integer(7))]);
    assert!(c.execute(sql, &invalid).is_err());
    assert_eq!(q(&c, "SELECT id,n FROM docs ORDER BY n").rows, before);
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    let valid = Parameters::from([("$extra".into(), Value::Integer(0))]);
    assert_eq!(c.execute(sql, &valid).unwrap().affected, 2);
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![
            vec![Value::Integer(3)],
            vec![Value::Integer(4)],
            vec![Value::Integer(5)]
        ]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}

#[test]
fn source_free_where_subqueries_retain_outer_record_correlation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "INSERT INTO links {owner:docs:a}",
    ] {
        q(&c, sql);
    }
    for outer in ["docs d", "(SELECT id,n FROM docs) d"] {
        for predicate in [
            "EXISTS(SELECT 1 FROM links l WHERE l.owner=d.id)",
            "(SELECT count(*) FROM links l WHERE l.owner=d.id)>0",
            "EXISTS(WITH x AS (SELECT owner FROM links l WHERE l.owner=d.id) SELECT 1 FROM x)",
        ] {
            let sql = format!("SELECT n,(SELECT 1 WHERE {predicate}) FROM {outer} ORDER BY n");
            let expected = vec![
                vec![Value::Integer(1), Value::Integer(1)],
                vec![Value::Integer(2), Value::Null],
            ];
            assert_eq!(q(&c, &sql).rows, expected, "{sql}");
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected
            );
        }
    }
}

#[test]
fn source_free_correlated_filters_skip_invalid_projections() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "INSERT INTO docs {id:docs:a,n:1,v:[]}",
        "INSERT INTO docs {id:docs:b,n:2,v:1}",
        "INSERT INTO links {owner:docs:a}",
    ] {
        q(&c, sql);
    }
    for source in ["docs d", "(SELECT id,n,v FROM docs) d"] {
        let sql = format!("SELECT n,(SELECT array::append(d.v,2) WHERE EXISTS(SELECT 1 FROM links l WHERE l.owner=d.id)) FROM {source} ORDER BY n");
        let expected = vec![
            vec![Value::Integer(1), Value::Array(vec![Value::Integer(2)])],
            vec![Value::Integer(2), Value::Null],
        ];
        assert_eq!(q(&c, &sql).rows, expected, "{sql}");
        assert_eq!(
            c.profile_select(&sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected,
            "{sql}"
        );
        q(&c, "INSERT INTO links {owner:docs:b}");
        assert!(c.execute(&sql, &Parameters::new()).is_err(), "{sql}");
        assert!(c.profile_select(&sql, &Parameters::new()).is_err(), "{sql}");
        q(&c, "DELETE FROM links WHERE owner=docs:b");
        assert_eq!(q(&c, &sql).rows, expected, "{sql}");
    }
}

#[test]
fn source_free_typed_parameters_preserve_correlated_records() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {id:docs:a,n:1}");
    q(&c, "INSERT INTO docs {id:docs:b,n:2}");
    let records = q(&c, "SELECT id FROM docs ORDER BY n").rows;
    for source in ["docs d", "(SELECT id,n FROM docs) d"] {
        for admitted in [false, true] {
            let params = Parameters::from([("$admit".into(), Value::Boolean(admitted))]);
            let sql = format!("SELECT (SELECT d.id WHERE $admit) FROM {source} ORDER BY n");
            let expected = if admitted {
                records.clone()
            } else {
                vec![vec![Value::Null], vec![Value::Null]]
            };
            assert_eq!(c.execute(&sql, &params).unwrap().rows, expected, "{sql}");
            assert_eq!(
                c.profile_select(&sql, &params).unwrap().result.rows,
                expected,
                "{sql}"
            );
        }
        for selected in 0..2 {
            let params = Parameters::from([("$id".into(), records[selected][0].clone())]);
            let sql = format!("SELECT (SELECT d.id WHERE d.id=$id) FROM {source} ORDER BY n");
            let mut expected = vec![vec![Value::Null], vec![Value::Null]];
            expected[selected] = records[selected].clone();
            assert_eq!(c.execute(&sql, &params).unwrap().rows, expected, "{sql}");
            assert_eq!(
                c.profile_select(&sql, &params).unwrap().result.rows,
                expected,
                "{sql}"
            );
        }
    }
}

#[test]
fn source_free_membership_binds_outer_left_operand() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "INSERT INTO docs {id:docs:a,n:1,v:[]}",
        "INSERT INTO docs {id:docs:b,n:2,v:[]}",
        "INSERT INTO links {owner:docs:a}",
    ] {
        q(&c, sql);
    }
    for null_member in [false, true] {
        if null_member {
            q(&c, "INSERT INTO links {owner:null}");
        }
        for source in ["docs d", "(SELECT id,n,v FROM docs) d"] {
            for lhs in [
                "d.id",
                "coalesce(d.id,docs:a)",
                "(SELECT d.id WHERE record::id(d.id) IS NOT NULL)",
                "(SELECT d.id WHERE true)",
            ] {
                for negated in [false, true] {
                    let op = if negated { "NOT IN" } else { "IN" };
                    let sql = format!("SELECT n,(SELECT array::append(d.v,2) WHERE {lhs} {op} (SELECT owner FROM links)) FROM {source} ORDER BY n");
                    let expected = (0..2)
                        .map(|row| {
                            let admitted = if negated {
                                row == 1 && !null_member
                            } else {
                                row == 0
                            };
                            vec![
                                Value::Integer(row + 1),
                                if admitted {
                                    Value::Array(vec![Value::Integer(2)])
                                } else {
                                    Value::Null
                                },
                            ]
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(q(&c, &sql).rows, expected, "{sql}");
                    assert_eq!(
                        c.profile_select(&sql, &Parameters::new())
                            .unwrap()
                            .result
                            .rows,
                        expected,
                        "{sql}"
                    );
                }
            }
        }
    }
}

#[test]
fn scalar_membership_assignments_preserve_atomicity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "DEFINE FIELD n ON docs TYPE integer CHECK(n<10)",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "INSERT INTO links {owner:docs:a,n:3}",
        "INSERT INTO links {owner:docs:b,n:4}",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
    ] {
        q(&c, sql);
    }
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {id:docs:prior,n:5}");
    let before = q(&c, "SELECT id,n FROM docs ORDER BY n").rows;
    let sql = "UPDATE docs AS d SET n=(SELECT d.n+$extra WHERE d.id IN (SELECT owner FROM links) AND $admit) WHERE n<5";
    let invalid = Parameters::from([
        ("$extra".into(), Value::Integer(8)),
        ("$admit".into(), Value::Boolean(true)),
    ]);
    assert!(c.execute(sql, &invalid).is_err());
    assert_eq!(q(&c, "SELECT id,n FROM docs ORDER BY n").rows, before);
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    let valid = Parameters::from([
        ("$extra".into(), Value::Integer(2)),
        ("$admit".into(), Value::Boolean(true)),
    ]);
    assert_eq!(c.execute(sql, &valid).unwrap().affected, 2);
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![
            vec![Value::Integer(3)],
            vec![Value::Integer(4)],
            vec![Value::Integer(5)]
        ]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}

#[test]
fn collection_scalar_membership_binds_outer_left_operand() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "CREATE TABLE marker",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "INSERT INTO links {owner:docs:a}",
        "INSERT INTO marker {n:7}",
    ] {
        q(&c, sql);
    }
    for source in ["docs d", "(SELECT id,n FROM docs) d"] {
        for negated in [false, true] {
            let op = if negated { "NOT IN" } else { "IN" };
            for lhs in ["d.id", "coalesce(d.id,docs:a)"] {
                let sql = format!("SELECT n,(SELECT m.n FROM marker m WHERE {lhs} {op} (SELECT owner FROM links)) FROM {source} ORDER BY n");
                let expected = (0..2)
                    .map(|row| {
                        vec![
                            Value::Integer(row + 1),
                            if (row == 0) != negated {
                                Value::Integer(7)
                            } else {
                                Value::Null
                            },
                        ]
                    })
                    .collect::<Vec<_>>();
                assert_eq!(q(&c, &sql).rows, expected, "{sql}");
                assert_eq!(
                    c.profile_select(&sql, &Parameters::new())
                        .unwrap()
                        .result
                        .rows,
                    expected,
                    "{sql}"
                );
            }
        }
        let shadowed = format!("SELECT n,(SELECT d.n FROM marker d WHERE d.n IN (SELECT n FROM docs)) FROM {source} ORDER BY n");
        let expected = vec![
            vec![Value::Integer(1), Value::Null],
            vec![Value::Integer(2), Value::Null],
        ];
        assert_eq!(q(&c, &shadowed).rows, expected, "{shadowed}");
        assert_eq!(
            c.profile_select(&shadowed, &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected,
            "{shadowed}"
        );
    }
}

#[test]
fn collection_scalar_membership_nulls_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE links",
        "CREATE TABLE marker",
        "CREATE TABLE baseline(n INTEGER,k INTEGER)",
        "CREATE TABLE lookup(n INTEGER)",
        "CREATE TABLE native_marker(n INTEGER)",
        "INSERT INTO docs {n:1,k:1}",
        "INSERT INTO docs {n:2,k:2}",
        "INSERT INTO docs {n:3,k:null}",
        "INSERT INTO baseline VALUES(1,1),(2,2),(3,NULL)",
        "INSERT INTO marker {n:7}",
        "INSERT INTO native_marker VALUES(7)",
    ] {
        q(&c, sql);
    }
    for setup in [
        vec!["INSERT INTO links {n:1}", "INSERT INTO lookup VALUES(1)"],
        vec![
            "INSERT INTO links {n:null}",
            "INSERT INTO lookup VALUES(NULL)",
        ],
        vec!["DELETE FROM links", "DELETE FROM lookup"],
    ] {
        for sql in setup {
            q(&c, sql);
        }
        for negated in [false, true] {
            let op = if negated { "NOT IN" } else { "IN" };
            let native = format!("SELECT n,(SELECT m.n FROM native_marker m WHERE d.k {op} (SELECT n FROM lookup)) FROM baseline d ORDER BY n");
            let expected = q(&c, &native).rows;
            for source in ["docs d", "(SELECT n,k FROM docs) d"] {
                let sql = format!("SELECT n,(SELECT m.n FROM marker m WHERE d.k {op} (SELECT n FROM links)) FROM {source} ORDER BY n");
                assert_eq!(q(&c, &sql).rows, expected, "{sql}");
                assert_eq!(
                    c.profile_select(&sql, &Parameters::new())
                        .unwrap()
                        .result
                        .rows,
                    expected,
                    "{sql}"
                );
            }
        }
    }
}

#[test]
fn inherited_scalar_binding_preserves_outer_value_types() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    let record = q(&c, "SELECT type::record('docs','key')").rows[0][0].clone();
    for value in [
        Value::Null,
        Value::Integer(42),
        Value::Number(1.5),
        Value::String("hello".into()),
        Value::Boolean(true),
        record,
        Value::Array(vec![Value::Null, Value::Integer(1)]),
        Value::Object(Default::default()),
        Value::Binary(b"FDB\x01payload".to_vec()),
        Value::vector32(&[1.0, 0.0]).unwrap(),
    ] {
        q(&c, "DELETE FROM docs");
        c.execute(
            "INSERT INTO docs {v:$v}",
            &Parameters::from([("$v".into(), value.clone())]),
        )
        .unwrap();
        for source in ["docs d", "(SELECT v FROM docs) d"] {
            let sql =
                format!("SELECT (SELECT array::append(array::new(), (SELECT d.v WHERE true))) FROM {source}");
            let expected = vec![vec![Value::Array(vec![value.clone()])]];
            assert_eq!(q(&c, &sql).rows, expected, "{sql}");
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected,
                "{sql}"
            );
        }
    }
}

#[test]
fn source_free_scalar_ordering_binds_outer_fields() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE baseline(n INTEGER)",
        "INSERT INTO docs {n:1}",
        "INSERT INTO docs {n:2}",
        "INSERT INTO baseline VALUES(1),(2)",
    ] {
        q(&c, sql);
    }
    let native = q(
        &c,
        "SELECT n,(SELECT d.n ORDER BY d.n DESC) FROM baseline d ORDER BY n",
    )
    .rows;
    assert_eq!(
        native,
        vec![
            vec![Value::Integer(1), Value::Integer(1)],
            vec![Value::Integer(2), Value::Integer(2)]
        ]
    );
    for source in ["docs d", "(SELECT n FROM docs) d"] {
        let sql =
            format!("SELECT n,(SELECT array::new(d.n) ORDER BY d.n DESC) FROM {source} ORDER BY n");
        let expected = vec![
            vec![Value::Integer(1), Value::Array(vec![Value::Integer(1)])],
            vec![Value::Integer(2), Value::Array(vec![Value::Integer(2)])],
        ];
        assert_eq!(q(&c, &sql).rows, expected, "{sql}");
        assert_eq!(
            c.profile_select(&sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected,
            "{sql}"
        );
    }
    for (limit, offset) in [(0, 0), (1, 0), (1, 1), (-1, 0), (-1, 2)] {
        for numeric in [false, true] {
            let bound = |v| {
                if numeric {
                    Value::Number(v as f64)
                } else {
                    Value::Integer(v)
                }
            };
            let params = Parameters::from([
                ("$limit".into(), bound(limit)),
                ("$offset".into(), bound(offset)),
            ]);
            let native = q(&c, &format!("SELECT n,(SELECT d.n ORDER BY d.n DESC LIMIT {limit} OFFSET {offset}) FROM baseline d ORDER BY n")).rows;
            let expected = native
                .into_iter()
                .map(|mut row| {
                    if row[1] != Value::Null {
                        row[1] = Value::Array(vec![row[1].clone()]);
                    }
                    row
                })
                .collect::<Vec<_>>();
            for source in ["docs d", "(SELECT n FROM docs) d"] {
                for (limit_expr, offset_expr) in [
                    ("$limit", "$offset"),
                    ("$limit+0", "$offset+0"),
                    ("coalesce($limit,0)", "coalesce($offset,0)"),
                    ("(SELECT $limit)", "(SELECT $offset)"),
                ] {
                    let sql = format!("SELECT n,(SELECT array::new(d.n) ORDER BY d.n DESC LIMIT {limit_expr} OFFSET {offset_expr}) FROM {source} ORDER BY n");
                    assert_eq!(
                        c.execute(&sql, &params).unwrap().rows,
                        expected,
                        "{sql}: {params:?}"
                    );
                    assert_eq!(
                        c.profile_select(&sql, &params).unwrap().result.rows,
                        expected,
                        "{sql}: {params:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn scalar_pagination_preserves_parameter_accounting() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {n:1}");
    q(&c, "INSERT INTO docs {n:2}");
    let sql =
        "SELECT (SELECT array::new(d.n,$limit) LIMIT $limit OFFSET $offset) FROM docs d ORDER BY n";
    let valid = Parameters::from([
        ("$limit".into(), Value::Integer(1)),
        ("$offset".into(), Value::Integer(0)),
    ]);
    let expected = vec![
        vec![Value::Array(vec![Value::Integer(1), Value::Integer(1)])],
        vec![Value::Array(vec![Value::Integer(2), Value::Integer(1)])],
    ];
    assert_eq!(c.execute(sql, &valid).unwrap().rows, expected);
    for invalid in [
        Parameters::from([("$limit".into(), Value::Integer(1))]),
        Parameters::from([("$offset".into(), Value::Integer(0))]),
        Parameters::from([
            ("$limit".into(), Value::Integer(1)),
            ("$offset".into(), Value::Integer(0)),
            ("$unused".into(), Value::Integer(0)),
        ]),
    ] {
        let result = c.execute(sql, &invalid);
        assert!(
            matches!(result, Err(romedb::Error::Parameter(_))),
            "{invalid:?}: {result:?}"
        );
        assert!(matches!(
            c.profile_select(sql, &invalid),
            Err(romedb::Error::Parameter(_))
        ));
        assert_eq!(c.execute(sql, &valid).unwrap().rows, expected);
        assert_eq!(c.profile_select(sql, &valid).unwrap().result.rows, expected);
    }
}

#[test]
fn scalar_pagination_retains_statement_parameter_positions() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {n:1}");
    q(&c, "INSERT INTO docs {n:2}");
    let binary = Value::Binary(b"FDB\x01parameter".to_vec());
    for offset in [0, 1] {
        for (sql, params) in [
            (
                "SELECT ?1,(SELECT array::new(d.n,?2) LIMIT ?2 OFFSET ?3) FROM docs d ORDER BY n",
                Parameters::from([
                    ("?1".into(), binary.clone()),
                    ("?2".into(), Value::Integer(1)),
                    ("?3".into(), Value::Integer(offset)),
                ]),
            ),
            (
                "SELECT ?,(SELECT array::new(d.n,?) LIMIT ? OFFSET ?) FROM docs d ORDER BY n",
                Parameters::from([
                    ("?1".into(), binary.clone()),
                    ("?2".into(), Value::Integer(1)),
                    ("?3".into(), Value::Integer(1)),
                    ("?4".into(), Value::Integer(offset)),
                ]),
            ),
        ] {
            let expected = (1..=2)
                .map(|n| {
                    vec![
                        binary.clone(),
                        if offset == 0 {
                            Value::Array(vec![Value::Integer(n), Value::Integer(1)])
                        } else {
                            Value::Null
                        },
                    ]
                })
                .collect::<Vec<_>>();
            assert_eq!(c.execute(sql, &params).unwrap().rows, expected, "{sql}");
            assert_eq!(
                c.profile_select(sql, &params).unwrap().result.rows,
                expected,
                "{sql}"
            );
        }
    }
}

#[test]
fn scalar_pagination_rejects_nonintegral_numbers_and_retries() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {n:1}");
    q(&c, "INSERT INTO docs {n:2}");
    for source in ["docs d", "(SELECT n FROM docs) d"] {
        let sql = format!(
            "SELECT (SELECT array::new(d.n) LIMIT $limit OFFSET $offset) FROM {source} ORDER BY n"
        );
        let valid = Parameters::from([
            ("$limit".into(), Value::Number(1.0)),
            ("$offset".into(), Value::Number(0.0)),
        ]);
        let expected = vec![
            vec![Value::Array(vec![Value::Integer(1)])],
            vec![Value::Array(vec![Value::Integer(2)])],
        ];
        for name in ["$limit", "$offset"] {
            for value in [1.5, -1.5, i64::MIN as f64, i64::MAX as f64] {
                let mut params = valid.clone();
                params.insert(name.into(), Value::Number(value));
                assert!(c.execute(&sql, &params).is_err(), "{sql}: {params:?}");
                assert!(
                    c.profile_select(&sql, &params).is_err(),
                    "{sql}: {params:?}"
                );
                assert_eq!(c.execute(&sql, &valid).unwrap().rows, expected, "{sql}");
                assert_eq!(
                    c.profile_select(&sql, &valid).unwrap().result.rows,
                    expected,
                    "{sql}"
                );
            }
        }
    }
}

#[test]
fn scalar_paginated_assignments_preserve_atomicity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD n ON docs TYPE integer CHECK(n<10)",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
    ] {
        q(&c, sql);
    }
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {id:docs:prior,n:5}");
    let before = q(&c, "SELECT id,n FROM docs ORDER BY n").rows;
    let sql = "UPDATE docs AS d SET n=(SELECT d.n+$extra WHERE $admit LIMIT $limit OFFSET $offset) WHERE n<5";
    let invalid = Parameters::from([
        ("$extra".into(), Value::Integer(8)),
        ("$admit".into(), Value::Boolean(true)),
        ("$limit".into(), Value::Number(1.0)),
        ("$offset".into(), Value::Number(0.0)),
    ]);
    assert!(c.execute(sql, &invalid).is_err());
    assert_eq!(q(&c, "SELECT id,n FROM docs ORDER BY n").rows, before);
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    let valid = Parameters::from([
        ("$extra".into(), Value::Integer(2)),
        ("$admit".into(), Value::Boolean(true)),
        ("$limit".into(), Value::Number(1.0)),
        ("$offset".into(), Value::Number(0.0)),
    ]);
    let mut bad_pagination = valid.clone();
    bad_pagination.insert("$offset".into(), Value::Number(1.5));
    assert!(c.execute(sql, &bad_pagination).is_err());
    assert_eq!(q(&c, "SELECT id,n FROM docs ORDER BY n").rows, before);
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    assert_eq!(c.execute(sql, &valid).unwrap().affected, 2);
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![
            vec![Value::Integer(3)],
            vec![Value::Integer(4)],
            vec![Value::Integer(5)]
        ]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}

#[test]
fn scalar_pagination_rejects_nonnumeric_values_and_retries() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {n:1}");
    q(&c, "INSERT INTO docs {n:2}");
    for source in ["docs d", "(SELECT n FROM docs) d"] {
        let sql = format!(
            "SELECT (SELECT array::new(d.n) LIMIT $limit OFFSET $offset) FROM {source} ORDER BY n"
        );
        let valid = Parameters::from([
            ("$limit".into(), Value::Integer(1)),
            ("$offset".into(), Value::Integer(0)),
        ]);
        let expected = vec![
            vec![Value::Array(vec![Value::Integer(1)])],
            vec![Value::Array(vec![Value::Integer(2)])],
        ];
        for name in ["$limit", "$offset"] {
            for value in [
                Value::Null,
                Value::String("invalid".into()),
                Value::Binary(b"FDB\x01payload".to_vec()),
            ] {
                let mut params = valid.clone();
                params.insert(name.into(), value);
                assert!(c.execute(&sql, &params).is_err(), "{sql}: {params:?}");
                assert!(
                    c.profile_select(&sql, &params).is_err(),
                    "{sql}: {params:?}"
                );
                assert_eq!(c.execute(&sql, &valid).unwrap().rows, expected);
                assert_eq!(
                    c.profile_select(&sql, &valid).unwrap().result.rows,
                    expected
                );
            }
        }
    }
}

#[test]
fn collection_scalar_computed_offsets_reset_per_outer_row() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE items",
        "INSERT INTO docs {n:1}",
        "INSERT INTO docs {n:2}",
        "INSERT INTO items {n:1}",
        "INSERT INTO items {n:2}",
        "INSERT INTO items {n:3}",
    ] {
        q(&c, sql);
    }
    for source in ["docs d", "(SELECT n FROM docs) d"] {
        for offset in ["1", "1+0", "coalesce(1,0)"] {
            let sql = format!("SELECT n,(SELECT i.n FROM items i WHERE i.n>=d.n ORDER BY i.n LIMIT 1 OFFSET {offset}) FROM {source} ORDER BY n");
            let expected = vec![
                vec![Value::Integer(1), Value::Integer(2)],
                vec![Value::Integer(2), Value::Integer(3)],
            ];
            assert_eq!(q(&c, &sql).rows, expected, "{sql}");
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected,
                "{sql}"
            );
        }
    }
}

#[test]
fn correlated_predicate_pagination_matches_literal_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE items",
        "CREATE TABLE baseline(n INTEGER)",
        "CREATE TABLE lookup(n INTEGER)",
        "INSERT INTO docs {n:1}",
        "INSERT INTO docs {n:2}",
        "INSERT INTO docs {n:3}",
        "INSERT INTO items {n:1}",
        "INSERT INTO items {n:2}",
        "INSERT INTO items {n:3}",
        "INSERT INTO baseline VALUES(1),(2),(3)",
        "INSERT INTO lookup VALUES(1),(2),(3)",
    ] {
        q(&c, sql);
    }
    for (limit, offset) in [(0, 0), (1, 0), (1, 1), (1, 2)] {
        for predicate in ["EXISTS", "NOT EXISTS", "(d.n+1) IN", "(d.n+1) NOT IN"] {
            for (projection, filter, ordering) in
                [("i.n", "i.n>=d.n", "i.n"), ("count(*)", "i.n>d.n", "1")]
            {
                let native = format!("SELECT n,{predicate}(SELECT {projection} FROM lookup i WHERE {filter} ORDER BY {ordering} LIMIT {limit} OFFSET {offset}) FROM baseline d ORDER BY n");
                let expected = q(&c, &native).rows;
                for source in ["docs d", "(SELECT n FROM docs) d"] {
                    let sql = format!("SELECT n,{predicate}(SELECT {projection} FROM items i WHERE {filter} ORDER BY {ordering} LIMIT {limit}+0 OFFSET {offset}+0) FROM {source} ORDER BY n");
                    assert_eq!(q(&c, &sql).rows, expected, "{sql}");
                    assert_eq!(
                        c.profile_select(&sql, &Parameters::new())
                            .unwrap()
                            .result
                            .rows,
                        expected,
                        "{sql}"
                    );
                }
            }
        }
    }
}

#[test]
fn scalar_pagination_preserves_numeric_expression_types() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {n:1}");
    q(&c, "INSERT INTO docs {n:2}");
    for source in ["docs d", "(SELECT n FROM docs) d"] {
        for numeric in [false, true] {
            let params = Parameters::from([(
                "$limit".into(),
                if numeric {
                    Value::Number(1.0)
                } else {
                    Value::Integer(1)
                },
            )]);
            for pagination in [
                "LIMIT CASE WHEN typeof($limit)='real' THEN 1 ELSE 0 END",
                "LIMIT 1 OFFSET CASE WHEN typeof($limit)='real' THEN 0 ELSE 1 END",
            ] {
                let sql = format!(
                    "SELECT (SELECT array::new(d.n) {pagination}) FROM {source} ORDER BY n"
                );
                let expected = (1..=2)
                    .map(|n| {
                        vec![if numeric {
                            Value::Array(vec![Value::Integer(n)])
                        } else {
                            Value::Null
                        }]
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    c.execute(&sql, &params).unwrap().rows,
                    expected,
                    "{sql}: {params:?}"
                );
                assert_eq!(
                    c.profile_select(&sql, &params).unwrap().result.rows,
                    expected,
                    "{sql}: {params:?}"
                );
            }
        }
    }
}

#[test]
fn native_inner_pagination_preserves_numeric_expression_types() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE lookup(n INTEGER)",
        "INSERT INTO docs {n:1}",
        "INSERT INTO docs {n:2}",
        "INSERT INTO lookup VALUES(1),(2),(3)",
    ] {
        q(&c, sql);
    }
    for source in ["docs d", "(SELECT n FROM docs) d"] {
        for numeric in [false, true] {
            let params = Parameters::from([(
                "$v".into(),
                if numeric {
                    Value::Number(1.0)
                } else {
                    Value::Integer(1)
                },
            )]);
            for pagination in [
                "LIMIT CASE WHEN typeof($v)='real' THEN 1 ELSE 0 END",
                "LIMIT 1 OFFSET CASE WHEN typeof($v)='real' THEN 0 ELSE 99 END",
            ] {
                let sql = format!("SELECT (SELECT i.n FROM lookup i WHERE i.n>=d.n ORDER BY i.n {pagination}) FROM {source} ORDER BY n");
                let expected = (1..=2)
                    .map(|n| {
                        vec![if numeric {
                            Value::Integer(n)
                        } else {
                            Value::Null
                        }]
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    c.execute(&sql, &params).unwrap().rows,
                    expected,
                    "{sql}: {params:?}"
                );
                assert_eq!(
                    c.profile_select(&sql, &params).unwrap().result.rows,
                    expected,
                    "{sql}: {params:?}"
                );
            }
        }
    }
}

#[test]
fn collection_distinct_scalar_pagination_matches_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE items",
        "CREATE TABLE baseline(n INTEGER)",
        "CREATE TABLE lookup(n INTEGER)",
        "INSERT INTO docs {n:1}",
        "INSERT INTO docs {n:2}",
        "INSERT INTO items {n:1}",
        "INSERT INTO items {n:2}",
        "INSERT INTO items {n:2}",
        "INSERT INTO items {n:3}",
        "INSERT INTO baseline VALUES(1),(2)",
        "INSERT INTO lookup VALUES(1),(2),(2),(3)",
    ] {
        q(&c, sql);
    }
    for offset in [0, 1, 2, 3] {
        for (distinct, group) in [("DISTINCT ", ""), ("", " GROUP BY i.n HAVING count(*)>1")] {
            let native = format!("SELECT n,(SELECT {distinct}i.n FROM lookup i WHERE i.n>=d.n{group} ORDER BY i.n LIMIT 1 OFFSET {offset}) FROM baseline d ORDER BY n");
            let expected = q(&c, &native).rows;
            for source in ["docs d", "(SELECT n FROM docs) d"] {
                let sql = format!("SELECT n,(SELECT {distinct}i.n FROM items i WHERE i.n>=d.n{group} ORDER BY i.n LIMIT 1 OFFSET {offset}+0) FROM {source} ORDER BY n");
                assert_eq!(q(&c, &sql).rows, expected, "{sql}");
                assert_eq!(
                    c.profile_select(&sql, &Parameters::new())
                        .unwrap()
                        .result
                        .rows,
                    expected,
                    "{sql}"
                );
            }
        }
    }
}

#[test]
fn collection_windowed_scalar_pagination_matches_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE items",
        "CREATE TABLE baseline(n INTEGER)",
        "CREATE TABLE lookup(n INTEGER)",
        "INSERT INTO docs {n:1}",
        "INSERT INTO docs {n:2}",
        "INSERT INTO docs {n:3}",
        "INSERT INTO items {n:1}",
        "INSERT INTO items {n:2}",
        "INSERT INTO items {n:3}",
        "INSERT INTO baseline VALUES(1),(2),(3)",
        "INSERT INTO lookup VALUES(1),(2),(3)",
    ] {
        q(&c, sql);
    }
    for offset in [0, 1, 2, 3] {
        for specification in [
            "ORDER BY i.n",
            "PARTITION BY d.n ORDER BY i.n",
            "PARTITION BY d.n+1 ORDER BY i.n",
            "PARTITION BY i.n % d.n ORDER BY i.n",
            "PARTITION BY i.n % d.n ORDER BY i.n * -d.n",
        ] {
            for window in ["row_number()", "sum(i.n)", "count(*)"] {
                let native = format!("SELECT n,(SELECT {window} OVER ({specification}) FROM lookup i WHERE i.n>=d.n ORDER BY i.n LIMIT 1 OFFSET {offset}) FROM baseline d ORDER BY n");
                let expected = q(&c, &native).rows;
                for source in ["docs d", "(SELECT n FROM docs) d"] {
                    let sql = format!("SELECT n,(SELECT {window} OVER ({specification}) FROM items i WHERE i.n>=d.n ORDER BY i.n LIMIT 1 OFFSET {offset}+0) FROM {source} ORDER BY n");
                    assert_eq!(q(&c, &sql).rows, expected, "{sql}");
                    assert_eq!(
                        c.profile_select(&sql, &Parameters::new())
                            .unwrap()
                            .result
                            .rows,
                        expected,
                        "{sql}"
                    );
                }
            }
        }
    }
}

#[test]
fn source_free_named_windows_bind_outer_fields() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE baseline(n INTEGER)",
        "INSERT INTO docs {n:1}",
        "INSERT INTO docs {n:2}",
        "INSERT INTO baseline VALUES(1),(2)",
    ] {
        q(&c, sql);
    }
    for function in ["row_number()", "sum(d.n)"] {
        for key in ["d.n", "d.n+1", "CAST(d.n AS TEXT)"] {
            let native = format!("SELECT n,(SELECT {function} OVER w WINDOW w AS (PARTITION BY {key} ORDER BY {key})) FROM baseline d ORDER BY n");
            let expected = q(&c, &native)
                .rows
                .into_iter()
                .map(|mut row| {
                    row[1] = Value::Array(vec![row[1].clone()]);
                    row
                })
                .collect::<Vec<_>>();
            for source in ["docs d", "(SELECT n FROM docs) d"] {
                let sql = format!("SELECT n,(SELECT array::new({function} OVER w) WINDOW w AS (PARTITION BY {key} ORDER BY {key})) FROM {source} ORDER BY n");
                assert_eq!(q(&c, &sql).rows, expected, "{sql}");
                assert_eq!(
                    c.profile_select(&sql, &Parameters::new())
                        .unwrap()
                        .result
                        .rows,
                    expected,
                    "{sql}"
                );
            }
        }
    }
}

#[test]
fn relational_outer_columns_keep_plain_names_beside_collection_subqueries() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(
        &c,
        "CREATE TABLE organizations (id TEXT PRIMARY KEY, plan TEXT, status TEXT)",
    );
    q(
        &c,
        "INSERT INTO organizations VALUES ('o1','trial','active')",
    );
    q(&c, "CREATE TABLE settings");
    q(
        &c,
        "INSERT INTO settings {id:settings:s1,startedAt:1,endsAt:50}",
    );
    let mut parameters = Parameters::new();
    parameters.insert("$org".into(), Value::String("o1".into()));
    parameters.insert("$key".into(), Value::String("s1".into()));
    parameters.insert("$now".into(), Value::Integer(10));
    for (sql, columns) in [
        (
            "SELECT plan FROM organizations WHERE id=$org AND EXISTS (SELECT 1 FROM settings WHERE id=type::record('settings',$key) AND startedAt<=$now AND endsAt>$now)",
            vec!["plan"],
        ),
        (
            "SELECT plan FROM organizations WHERE id=$org AND EXISTS (SELECT 1 FROM organizations o WHERE o.id=$org AND EXISTS (SELECT 1 FROM settings WHERE id=type::record('settings',$key) AND endsAt>$now))",
            vec!["plan"],
        ),
        (
            "SELECT plan, status FROM organizations WHERE (SELECT endsAt FROM settings WHERE id=type::record('settings',$key)) > $now",
            vec!["plan", "status"],
        ),
        (
            "SELECT plan FROM organizations WHERE id IN (SELECT $org FROM settings)",
            vec!["plan"],
        ),
    ] {
        let used: Parameters = parameters
            .iter()
            .filter(|(name, _)| sql.contains(name.as_str()))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let result = c.execute(sql, &used).expect(sql);
        assert_eq!(result.columns, columns, "{sql}");
        assert_eq!(result.rows.len(), 1, "{sql}");
    }
}
