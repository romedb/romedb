use romedb::{Database, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
#[test]
fn grouped_aggregates_match_relational_engine() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE sales");
    q(&c, "CREATE TABLE baseline (region TEXT, amount INTEGER)");
    for (id, region, amount) in [(1, "east", 3), (2, "east", 7), (3, "west", 4)] {
        q(
            &c,
            &format!("INSERT INTO sales {{id:sales:{id},region:'{region}',amount:{amount}}}"),
        );
        q(
            &c,
            &format!("INSERT INTO baseline VALUES ('{region}',{amount})"),
        );
    }
    for tail in [
        "GROUP BY region ORDER BY region",
        "WHERE amount > 3 GROUP BY region HAVING sum(amount) >= 7 ORDER BY region",
        "GROUP BY 1 ORDER BY 1 LIMIT 1",
    ] {
        let projection = "region, count(*) AS n, sum(amount) AS total, avg(amount) AS mean";
        assert_eq!(
            q(&c, &format!("SELECT {projection} FROM sales {tail}")).rows,
            q(&c, &format!("SELECT {projection} FROM baseline {tail}")).rows
        );
    }
    assert_eq!(
        q(&c, "SELECT 7 AS key, count(*) AS n FROM sales GROUP BY 1").rows,
        vec![vec![Value::Integer(7), Value::Integer(3)]]
    );
    q(&c, "INSERT INTO sales {id:sales:4,amount:2}");
    q(&c, "INSERT INTO sales {id:sales:5,region:null,amount:1}");
    assert_eq!(
        q(
            &c,
            "SELECT region, count(*) AS n FROM sales GROUP BY region HAVING region IS NULL"
        )
        .rows,
        vec![vec![Value::Null, Value::Integer(2)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT region AS place, count(*) AS n FROM sales GROUP BY place ORDER BY place"
        )
        .rows,
        q(
            &c,
            "SELECT region AS place, count(*) AS n FROM sales GROUP BY region ORDER BY place"
        )
        .rows
    );
}
#[test]
fn grouped_insert_select_validates_atomically() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE sales");
    q(&c, "INSERT INTO sales {region:'a',amount:2}");
    q(&c, "INSERT INTO sales {region:'b',amount:8}");
    q(&c, "CREATE TABLE totals");
    q(
        &c,
        "DEFINE FIELD total ON totals TYPE integer CHECK (total < 5)",
    );
    assert!(c.execute("INSERT INTO totals (region,total) SELECT region,sum(amount) FROM sales GROUP BY region ORDER BY region",&Parameters::new()).is_err());
    assert!(q(&c, "SELECT * FROM totals").rows.is_empty());
    q(&c,"INSERT INTO totals (region,total) SELECT region,sum(amount) FROM sales GROUP BY region HAVING sum(amount)<5");
    assert_eq!(
        q(&c, "SELECT total FROM totals").rows,
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn having_aliases_match_native_precedence_and_aggregate_expressions() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE sales");
    q(&c, "CREATE TABLE baseline(region,amount,total)");
    for (region, amount, total) in [("a", 2, 99), ("a", 3, 99), ("b", 8, 0)] {
        q(
            &c,
            &format!("INSERT INTO sales {{region:'{region}',amount:{amount},total:{total}}}"),
        );
        q(
            &c,
            &format!("INSERT INTO baseline VALUES ('{region}',{amount},{total})"),
        );
    }
    for having in [
        "total>6",
        "abs(total)>6 AND n=1",
        "place='a'",
        "total>6 AND s.total=0",
    ] {
        for distinct in ["", "DISTINCT "] {
            let tail = format!("GROUP BY s.region HAVING {having} ORDER BY total");
            let projection =
                format!("{distinct}s.region AS place,sum(s.amount) AS total,count(*) AS n");
            assert_eq!(
                q(&c, &format!("SELECT {projection} FROM sales s {tail}")).rows,
                q(&c, &format!("SELECT {projection} FROM baseline s {tail}")).rows,
                "{having}"
            );
        }
    }
    assert_eq!(
        q(&c, "SELECT count(*) AS n FROM sales HAVING N=3").rows,
        vec![vec![Value::Integer(3)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT s.region AS amount,count(*) AS n FROM sales s GROUP BY s.amount ORDER BY n"
        )
        .rows,
        q(
            &c,
            "SELECT s.region AS amount,count(*) AS n FROM baseline s GROUP BY s.amount ORDER BY n"
        )
        .rows
    );
    q(&c, "CREATE TABLE totals");
    q(&c,"INSERT INTO totals (region,total) SELECT region,sum(amount) AS total FROM sales GROUP BY region HAVING total>6");
    assert_eq!(
        q(&c, "SELECT total FROM totals").rows,
        vec![vec![Value::Integer(8)]]
    );
}

#[test]
fn having_aliases_preserve_typed_helper_inputs_and_reject_fetched_values() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {id:docs:2,active:true}");
    q(&c, "INSERT INTO docs {id:docs:10,active:false}");
    assert_eq!(
        q(
            &c,
            "SELECT active AS flag,count(*) AS n FROM docs GROUP BY active HAVING flag=true"
        )
        .rows,
        vec![vec![Value::Boolean(true), Value::Integer(1)]]
    );
    let result = q(
        &c,
        "SELECT id AS ref,count(*) AS n FROM docs GROUP BY id HAVING record::id(ref)=10",
    );
    assert!(matches!(
        &result.rows[0][0],
        Value::Record(romedb::Record {
            key: romedb::Key::Integer(10),
            ..
        })
    ));
    assert!(c.execute("SELECT record::fetch(id) AS target,count(*) AS n FROM docs GROUP BY id HAVING target IS NOT NULL",&Parameters::new()).is_err());
}

#[test]
fn grouping_ordinals_follow_native_wrappers_and_scalar_equality() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE baseline(v)");
    for value in ["1", "1.0", "true", "2", "null"] {
        q(&c, &format!("INSERT INTO docs {{v:{value}}}"));
        q(&c, &format!("INSERT INTO baseline VALUES ({value})"));
    }
    for group in [
        "2",
        "(2)",
        "+2",
        "((+2))",
        "2 COLLATE BINARY",
        "(2 COLLATE BINARY)",
    ] {
        let counts = |table: &str| {
            q(
                &c,
                &format!("SELECT count(*) AS n,v FROM {table} GROUP BY {group} ORDER BY n"),
            )
            .rows
            .into_iter()
            .map(|row| row[0].clone())
            .collect::<Vec<_>>()
        };
        assert_eq!(counts("docs"), counts("baseline"), "{group}");
        assert_eq!(
            counts("docs"),
            vec![Value::Integer(1), Value::Integer(1), Value::Integer(3)],
            "{group}"
        );
    }
    // The pinned engine only recognizes a single sign directly on a numeric
    // literal. More complex expressions remain constant grouping expressions.
    for group in ["+(2)", "-(-2)", "2.0", "2+0"] {
        assert_eq!(
            q(
                &c,
                &format!("SELECT count(*) AS n,v FROM docs GROUP BY {group}")
            )
            .rows[0][0],
            Value::Integer(5),
            "{group}"
        );
    }
    for group in ["0", "(0)", "+0", "-1", "(3)", "+3", "3 COLLATE BINARY"] {
        assert!(
            c.execute(
                &format!("SELECT v,count(*) FROM docs GROUP BY {group}"),
                &Parameters::new()
            )
            .is_err(),
            "{group}"
        );
    }
    assert_eq!(
        q(&c, "SELECT 7 AS key,count(*) AS n FROM docs GROUP BY (+1)").rows,
        vec![vec![Value::Integer(7), Value::Integer(5)]]
    );
}

#[test]
fn collated_grouping_ordinals_preserve_text_and_insert_select() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE baseline(v)");
    for value in ["a", "A", "b"] {
        q(&c, &format!("INSERT INTO docs {{v:'{value}'}}"));
        q(&c, &format!("INSERT INTO baseline VALUES ('{value}')"));
    }
    for group in ["1 COLLATE NOCASE", "((+1) COLLATE NOCASE)"] {
        let tail = format!("GROUP BY {group} ORDER BY n,v");
        assert_eq!(
            q(&c, &format!("SELECT v,count(*) AS n FROM docs {tail}")).rows,
            q(&c, &format!("SELECT v,count(*) AS n FROM baseline {tail}")).rows
        );
    }
    for group in ["1", "(1)", "+1", "1 COLLATE BINARY"] {
        let query = |table: &str| {
            format!("SELECT v COLLATE NOCASE AS value,count(*) AS n FROM {table} GROUP BY {group} ORDER BY n,value")
        };
        assert_eq!(
            q(&c, &query("docs")).rows,
            q(&c, &query("baseline")).rows,
            "projected collation: {group}"
        );
    }
    q(&c, "CREATE TABLE totals");
    q(
        &c,
        "INSERT INTO totals (v,n) SELECT v,count(*) FROM docs GROUP BY (1 COLLATE NOCASE)",
    );
    assert_eq!(
        q(&c, "SELECT n FROM totals ORDER BY n").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    // Ordinal substitution must not subsequently reinterpret a source name as
    // another projection's alias.
    assert_eq!(
        q(
            &c,
            "SELECT v AS key,count(*) AS v FROM docs GROUP BY (1) ORDER BY key"
        )
        .rows,
        q(
            &c,
            "SELECT v AS key,count(*) AS v FROM baseline GROUP BY (1) ORDER BY key"
        )
        .rows
    );
}

#[test]
fn group_aliases_resolve_expressions_constants_and_explicit_field_collisions() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(
        &c,
        "CREATE TABLE baseline(city TEXT,amount INTEGER,bucket TEXT)",
    );
    for (city, amount) in [("A", 1), ("a", 2), ("B", 3)] {
        q(
            &c,
            &format!("INSERT INTO docs {{city:'{city}',amount:{amount},bucket:'same'}}"),
        );
        q(
            &c,
            &format!("INSERT INTO baseline VALUES ('{city}',{amount},'same')"),
        );
    }
    for tail in [
        "GROUP BY place ORDER BY place",
        "GROUP BY (place COLLATE NOCASE) ORDER BY place",
        "GROUP BY lower(place) ORDER BY place",
    ] {
        let sql = |table| format!("SELECT city AS place,count(*) AS total FROM {table} {tail}");
        assert_eq!(
            q(&c, &sql("docs")).rows,
            q(&c, &sql("baseline")).rows,
            "{tail}"
        );
    }
    assert_eq!(
        q(
            &c,
            "SELECT 7 AS key,count(*) AS total FROM docs GROUP BY key"
        )
        .rows,
        vec![vec![Value::Integer(7), Value::Integer(3)]]
    );
    assert_eq!(q(&c, "SELECT lower(city) AS bucket,count(*) AS total FROM docs GROUP BY bucket ORDER BY bucket").rows,
               q(&c, "SELECT lower(city) AS bucket,count(*) AS total FROM baseline GROUP BY lower(city) ORDER BY bucket").rows);
    assert_eq!(
        q(
            &c,
            "SELECT lower(city) AS bucket,count(*) AS total FROM docs d GROUP BY d.bucket"
        )
        .rows
        .len(),
        1
    );
    assert!(c
        .execute(
            "SELECT sum(amount) AS total FROM docs GROUP BY total",
            &Parameters::new()
        )
        .is_err());
    q(&c, "CREATE TABLE typed");
    q(
        &c,
        "INSERT INTO typed (value) VALUES (X'31'),(X'31'),(docs:1),(docs:1)",
    );
    assert_eq!(
        q(
            &c,
            "SELECT value AS key,count(*) AS total FROM typed GROUP BY key ORDER BY key"
        )
        .rows,
        q(
            &c,
            "SELECT value AS key,count(*) AS total FROM typed GROUP BY value ORDER BY key"
        )
        .rows
    );
}

#[test]
fn filtered_distinct_aggregates_match_native_and_validate_grouped_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE baseline(region,v)");
    for values in [
        "('a',1)",
        "('a',1.0)",
        "('a',2)",
        "('a',NULL)",
        "('b',NULL)",
    ] {
        q(&c, &format!("INSERT INTO docs(region,v) VALUES{values}"));
        q(&c, &format!("INSERT INTO baseline VALUES{values}"));
    }
    for filter in ["v>1", "v IS NULL", "v<0", "v IS NOT NULL"] {
        for having in ["", "HAVING n>0"] {
            let sql = |table| {
                format!("SELECT region,count(DISTINCT v) FILTER (WHERE {filter}) AS n,sum(DISTINCT v) FILTER (WHERE {filter}) AS total,avg(DISTINCT v) FILTER (WHERE {filter}) AS mean FROM {table} GROUP BY region {having} ORDER BY region")
            };
            let expected = q(&c, &sql("baseline")).rows;
            assert_eq!(q(&c, &sql("docs")).rows, expected, "{filter} {having}");
            assert_eq!(
                c.profile_select(&sql("docs"), &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected,
                "profile {filter} {having}"
            );
        }
    }
    q(&c, "CREATE TABLE totals");
    q(
        &c,
        "DEFINE FIELD total ON totals TYPE integer CHECK (total<2)",
    );
    q(&c, "CREATE UNIQUE INDEX totals_region ON totals(region)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO totals {region:'prior',total:0}");
    let before = q(&c, "SELECT id,region,total FROM totals").rows;
    let insert="INSERT INTO totals(region,total) SELECT region,sum(DISTINCT v) FILTER (WHERE v IS NOT NULL) AS total FROM docs GROUP BY region HAVING total IS NOT NULL";
    assert!(c.execute(insert, &Parameters::new()).is_err());
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(q(&c, "SELECT id,region,total FROM totals").rows, before);
    c.check_collection_integrity("totals", Default::default())
        .unwrap();
    assert_eq!(q(&c, &insert.replace("v IS NOT NULL", "v<=1")).affected, 1);
    assert_eq!(
        q(&c, "SELECT total FROM totals WHERE region='a'").rows,
        vec![vec![Value::Integer(1)]]
    );
    c.check_collection_integrity("totals", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT * FROM totals").rows.is_empty());
    c.check_collection_integrity("totals", Default::default())
        .unwrap();
}

#[test]
fn aggregate_filters_preserve_record_arguments_and_skip_excluded_errors() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {n:1,owner:users:one}");
    q(&c, "INSERT INTO docs {n:2,owner:users:two}");
    q(&c, "INSERT INTO docs {n:3,owner:7}");
    for sql in [
        "SELECT sum(n) FILTER (WHERE owner=users:one) FROM docs",
        "SELECT count(record::id(owner)) FILTER (WHERE n=1) FROM docs",
        "SELECT count(DISTINCT record::id(owner)) FILTER (WHERE n=1) FROM docs",
    ] {
        assert_eq!(q(&c, sql).rows, vec![vec![Value::Integer(1)]], "{sql}");
        assert_eq!(
            c.profile_select(sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            vec![vec![Value::Integer(1)]],
            "profile {sql}"
        );
    }
    let sql = "SELECT sum(n) FILTER (WHERE owner=$owner) FROM docs";
    let params = Parameters::from([(
        "$owner".into(),
        Value::Record(romedb::Record {
            table: "users".into(),
            key: romedb::Key::String("two".into()),
        }),
    )]);
    assert_eq!(
        c.execute(sql, &params).unwrap().rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        c.profile_select(sql, &params).unwrap().result.rows,
        vec![vec![Value::Integer(2)]]
    );
    let invalid = "SELECT count(record::id(owner)) FILTER (WHERE n=3) FROM docs";
    assert!(c.execute(invalid, &Parameters::new()).is_err());
    assert_eq!(
        q(
            &c,
            "SELECT count(record::id(owner)) FILTER (WHERE n<3) FROM docs"
        )
        .rows,
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn count_accepts_composites_and_preserves_null_and_filter_semantics() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    for sql in [
        "INSERT INTO docs {n:1,v:[]}",
        "INSERT INTO docs {n:2,v:{a:1}}",
        "INSERT INTO docs {n:3,v:null}",
        "INSERT INTO docs {n:4}",
        "INSERT INTO docs {n:5,v:false}",
    ] {
        q(&c, sql);
    }
    assert_eq!(
        q(&c, "SELECT count(v),count(*),count(missing) FROM docs").rows,
        vec![vec![
            Value::Integer(3),
            Value::Integer(5),
            Value::Integer(0)
        ]]
    );
    for sql in [
        "SELECT count(array::append(v,1)) FILTER (WHERE n=1) FROM docs",
        "SELECT count(coalesce(v,array::new())) FILTER (WHERE n=3) FROM docs",
    ] {
        assert_eq!(q(&c, sql).rows, vec![vec![Value::Integer(1)]], "{sql}");
        assert_eq!(
            c.profile_select(sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            vec![vec![Value::Integer(1)]]
        );
    }
    assert!(c
        .execute(
            "SELECT count(array::append(v,1)) FILTER (WHERE n=2) FROM docs",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(
        q(&c, "SELECT count(v) FROM docs").rows,
        vec![vec![Value::Integer(3)]]
    );
}

#[test]
fn composite_count_windows_and_grouped_writes_preserve_counts() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE baseline(n,v)");
    for (n, value, native) in [
        (1, "[]", "x'01'"),
        (2, "null", "NULL"),
        (3, "{a:1}", "x'02'"),
        (4, "users:one", "x'03'"),
    ] {
        q(&c, &format!("INSERT INTO docs {{n:{n},v:{value}}}"));
        q(&c, &format!("INSERT INTO baseline VALUES({n},{native})"));
    }
    for window in ["ORDER BY n", "PARTITION BY n%2 ORDER BY n", ""] {
        let sql =
            |table| format!("SELECT n,count(v) OVER ({window}) AS present FROM {table} ORDER BY n");
        let expected = q(&c, &sql("baseline")).rows;
        assert_eq!(q(&c, &sql("docs")).rows, expected, "{window}");
        assert_eq!(
            c.profile_select(&sql("docs"), &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected,
            "profile {window}"
        );
    }
    q(&c, "CREATE TABLE totals");
    q(&c, "DEFINE FIELD n ON totals TYPE integer CHECK (n<2)");
    q(&c, "CREATE UNIQUE INDEX totals_bucket ON totals(bucket)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO totals {bucket:9,n:0}");
    let before = q(&c, "SELECT id,bucket,n FROM totals").rows;
    let insert = "INSERT INTO totals(bucket,n) SELECT n%2,count(v) FROM docs GROUP BY n%2";
    assert!(c.execute(insert, &Parameters::new()).is_err());
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(q(&c, "SELECT id,bucket,n FROM totals").rows, before);
    c.check_collection_integrity("totals", Default::default())
        .unwrap();
    assert_eq!(q(&c, &format!("{insert} HAVING count(v)<2")).affected, 1);
    assert_eq!(
        q(&c, "SELECT n FROM totals WHERE bucket=0").rows,
        vec![vec![Value::Integer(1)]]
    );
    c.check_collection_integrity("totals", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT * FROM totals").rows.is_empty());
}

#[test]
fn count_scalar_arguments_match_native_null_and_binary_semantics() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE baseline(v)");
    for value in [
        "NULL",
        "0",
        "false",
        "1",
        "1.0",
        "''",
        "'A'",
        "'a'",
        "x''",
        "x'46444201'",
    ] {
        q(&c, &format!("INSERT INTO docs(v) VALUES({value})"));
        q(&c, &format!("INSERT INTO baseline VALUES({value})"));
    }
    for expr in [
        "v",
        "CAST(v AS TEXT)",
        "CAST(v AS BLOB)",
        "v COLLATE NOCASE",
        "CASE WHEN v IS NULL THEN NULL ELSE v END",
        "NULLIF(v,0)",
        "x'46444201'",
        "NULL",
    ] {
        for modifier in ["", "ALL ", "DISTINCT "] {
            let sql = |table| format!("SELECT count({modifier}{expr}) FROM {table}");
            let expected = q(&c, &sql("baseline")).rows;
            assert_eq!(q(&c, &sql("docs")).rows, expected, "{modifier}{expr}");
            assert_eq!(
                c.profile_select(&sql("docs"), &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected,
                "profile {modifier}{expr}"
            );
        }
    }
}

#[test]
fn having_nonprojected_record_group_key_preserves_matches() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE links",
        "INSERT INTO links {owner:links:a,n:3}",
        "INSERT INTO links {owner:links:a,n:4}",
    ] {
        q(&c, sql);
    }
    assert_eq!(
        q(
            &c,
            "SELECT count(*) FROM links l GROUP BY l.owner HAVING l.owner=links:a"
        )
        .rows,
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn having_unprojected_document_keys_match_native_columns() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE baseline(k,n)");
    for (key, n) in [
        ("'A'", 1),
        ("'A'", 2),
        ("'a'", 3),
        ("NULL", 4),
        ("1", 5),
        ("1.0", 6),
    ] {
        q(&c, &format!("INSERT INTO docs {{k:{key},n:{n}}}"));
        q(&c, &format!("INSERT INTO baseline VALUES({key},{n})"));
    }
    for (group, having) in [
        ("k", "k='A'"),
        ("k", "k IS NULL"),
        ("k", "k=1"),
        ("k COLLATE NOCASE", "k COLLATE NOCASE='a'"),
        ("k", "count(*)>1 AND k IS NOT NULL"),
    ] {
        let sql = |table| {
            format!(
                "SELECT count(*),sum(n) FROM {table} GROUP BY {group} HAVING {having} ORDER BY 2"
            )
        };
        // Project the native key in the oracle to avoid the pinned engine's
        // own unprojected expression-key bug (including COLLATE keys).
        let expected = q(&c, &format!("SELECT {group},count(*),sum(n) FROM baseline GROUP BY {group} HAVING {having} ORDER BY 3")).rows.into_iter().map(|row| row.into_iter().skip(1).collect::<Vec<_>>()).collect::<Vec<_>>();
        assert_eq!(q(&c, &sql("docs")).rows, expected, "{group}: {having}");
        assert_eq!(
            c.profile_select(&sql("docs"), &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected
        );
    }
}

#[test]
fn aggregate_order_expressions_use_lowered_collection_fields() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE baseline(k,n)");
    for (key, n) in [("a", 1), ("a", 2), ("b", 8), ("c", 4)] {
        q(&c, &format!("INSERT INTO docs {{k:'{key}',n:{n}}}"));
        q(&c, &format!("INSERT INTO baseline VALUES('{key}',{n})"));
    }
    for aggregate in ["sum(n)", "avg(n)", "min(n)", "max(n)", "count(n)"] {
        for order in [
            aggregate.to_owned(),
            format!("{aggregate} DESC"),
            format!("({aggregate}) DESC"),
            "total DESC".into(),
            "2 DESC".into(),
        ] {
            let sql = |table| {
                format!("SELECT k,{aggregate} AS total FROM {table} GROUP BY k HAVING sum(n)>0 ORDER BY {order},k")
            };
            let expected = q(&c, &sql("baseline")).rows;
            assert_eq!(q(&c, &sql("docs")).rows, expected, "{aggregate}: {order}");
            assert_eq!(
                c.profile_select(&sql("docs"), &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected
            );
        }
    }
}

#[test]
fn ordered_grouped_insert_failure_restores_prior_work_and_indexes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "CREATE TABLE totals",
        "DEFINE FIELD total ON totals TYPE integer CHECK (total<5)",
        "CREATE UNIQUE INDEX totals_total ON totals(total)",
        "CREATE TABLE native_totals(total INTEGER UNIQUE CHECK(total<5))",
        "INSERT INTO docs {k:'a',n:1}",
        "INSERT INTO docs {k:'a',n:2}",
        "INSERT INTO docs {k:'b',n:8}",
        "INSERT INTO docs {k:'c',n:4}",
    ] {
        q(&c, sql);
    }
    for target in ["totals", "native_totals"] {
        q(&c, "BEGIN");
        q(&c, &format!("INSERT INTO {target}(total) VALUES(1)"));
        let source = "SELECT sum(n) FROM docs GROUP BY k ORDER BY sum(n)";
        assert_eq!(
            q(&c, source).rows,
            vec![
                vec![Value::Integer(3)],
                vec![Value::Integer(4)],
                vec![Value::Integer(8)]
            ]
        );
        // The final sorted group fails after two otherwise valid candidates.
        assert!(c
            .execute(
                &format!("INSERT INTO {target}(total) {source}"),
                &Parameters::new()
            )
            .is_err());
        assert_eq!(
            q(&c, &format!("SELECT total FROM {target} ORDER BY total")).rows,
            vec![vec![Value::Integer(1)]]
        );
        if target == "totals" {
            c.check_collection_integrity(target, Default::default())
                .unwrap();
        }
        assert_eq!(q(&c, &format!("INSERT INTO {target}(total) SELECT sum(n) FROM docs GROUP BY k HAVING sum(n)<5 ORDER BY sum(n)")).affected, 2);
        assert_eq!(
            q(&c, &format!("SELECT total FROM {target} ORDER BY total")).rows,
            vec![
                vec![Value::Integer(1)],
                vec![Value::Integer(3)],
                vec![Value::Integer(4)]
            ]
        );
        if target == "totals" {
            c.check_collection_integrity(target, Default::default())
                .unwrap();
        }
        q(&c, "ROLLBACK");
        assert!(q(&c, &format!("SELECT total FROM {target}"))
            .rows
            .is_empty());
    }
}

#[test]
fn having_function_keys_preserve_native_scalar_and_binary_arguments() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE baseline(k)");
    for value in [
        Value::String("A".into()),
        Value::String("A".into()),
        Value::String("b".into()),
        Value::Binary(vec![65]),
        Value::Null,
    ] {
        let params = Parameters::from([("$v".into(), value)]);
        c.execute("INSERT INTO docs {k:$v}", &params).unwrap();
        c.execute("INSERT INTO baseline VALUES($v)", &params)
            .unwrap();
    }
    for (key, predicate) in [
        ("lower(k)", "lower(k)='a'"),
        ("hex(k)", "hex(k)='41'"),
        ("length(k)", "length(k)=1"),
        ("typeof(k)", "typeof(k)='blob'"),
    ] {
        let expected = q(
            &c,
            &format!(
                "SELECT {key},count(*) FROM baseline GROUP BY {key} HAVING {predicate} ORDER BY 2"
            ),
        )
        .rows
        .into_iter()
        .map(|r| vec![r[1].clone()])
        .collect::<Vec<_>>();
        for source in ["docs", "(SELECT k FROM docs) d"] {
            let sql = format!(
                "SELECT count(*) FROM {source} GROUP BY {key} HAVING {predicate} ORDER BY 1"
            );
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
fn having_helper_aliases_reuse_projected_aggregates() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {k:'A',n:1}");
    q(&c, "INSERT INTO docs {k:'A',n:2}");
    for output in ["sum(n)", "sum(n)+1"] {
        let sql = format!("SELECT {output} FROM docs GROUP BY k HAVING sum(n)>0");
        let plan = q(&c, &format!("EXPLAIN {sql}"));
        assert_eq!(
            plan.rows
                .iter()
                .filter(|row| row.get(1) == Some(&Value::String("AggStep".into())))
                .count(),
            1,
            "{sql}"
        );
    }
    assert_eq!(
        q(
            &c,
            "SELECT sum(length(k)) FROM docs GROUP BY lower(k) HAVING lower(k)='a'"
        )
        .rows,
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn distinct_grouped_aggregate_pages_match_native_and_validate_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE baseline(k,n)");
    for (key, value) in [("a", "1"), ("b", "1.0"), ("c", "2"), ("d", "NULL")] {
        q(&c, &format!("INSERT INTO docs {{k:'{key}',n:{value}}}"));
        q(&c, &format!("INSERT INTO baseline VALUES('{key}',{value})"));
    }
    // DISTINCT merges integer/real equivalents; the retained representation
    // can differ with the engine plan. Compare logical numeric values here.
    let logical = |rows: Vec<Vec<Value>>| {
        rows.into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|value| match value {
                        Value::Integer(n) => Value::Number(n as f64),
                        value => value,
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    for aggregate in ["sum(n)", "sum(n)+1", "count(n)"] {
        for direction in ["ASC", "DESC"] {
            for offset in 0..4 {
                let sql = |table| {
                    format!("SELECT DISTINCT {aggregate} AS total FROM {table} GROUP BY k ORDER BY {aggregate} {direction} LIMIT 1 OFFSET {offset}")
                };
                let expected = logical(q(&c, &sql("baseline")).rows);
                assert_eq!(
                    logical(q(&c, &sql("docs")).rows),
                    expected,
                    "{aggregate}: {direction}: {offset}"
                );
                assert_eq!(
                    logical(
                        c.profile_select(&sql("docs"), &Parameters::new())
                            .unwrap()
                            .result
                            .rows
                    ),
                    expected
                );
            }
        }
    }
    q(&c, "CREATE TABLE totals");
    q(
        &c,
        "DEFINE FIELD total ON totals TYPE integer CHECK(total<2)",
    );
    q(&c, "CREATE UNIQUE INDEX totals_total ON totals(total)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO totals {total:0}");
    let invalid = "INSERT INTO totals(total) SELECT DISTINCT count(*)+1 FROM docs GROUP BY k ORDER BY count(*)+1 LIMIT 1";
    assert!(c.execute(invalid, &Parameters::new()).is_err());
    assert_eq!(
        q(&c, "SELECT total FROM totals").rows,
        vec![vec![Value::Integer(0)]]
    );
    assert_eq!(q(&c,"INSERT INTO totals(total) SELECT DISTINCT count(*) FROM docs GROUP BY k ORDER BY count(*) LIMIT 1").affected,1);
    assert_eq!(
        q(&c, "SELECT total FROM totals ORDER BY total").rows,
        vec![vec![Value::Integer(0)], vec![Value::Integer(1)]]
    );
    c.check_collection_integrity("totals", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT total FROM totals").rows.is_empty());
}
