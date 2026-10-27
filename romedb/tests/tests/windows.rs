use romedb::{Database, Parameters};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
#[test]
fn collection_windows_match_pinned_relational_results() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE samples");
    q(&c, "CREATE TABLE baseline (category TEXT, value INTEGER)");
    for (category, value) in [("a", 2), ("a", 4), ("a", 8), ("b", 3)] {
        q(
            &c,
            &format!("INSERT INTO samples {{category:'{category}',value:{value}}}"),
        );
        q(
            &c,
            &format!("INSERT INTO baseline VALUES ('{category}',{value})"),
        );
    }
    for (projection, tail) in [
        (
            "category,value,row_number() OVER (PARTITION BY category ORDER BY value) AS position",
            "",
        ),
        (
            "category,value,sum(value) OVER (PARTITION BY category ORDER BY value ) AS rolling",
            "",
        ),
        (
            "category,value,count(*) OVER (PARTITION BY category) AS n",
            "",
        ),
        (
            "category,value,sum(value) OVER w AS total",
            "WINDOW w AS (PARTITION BY category ORDER BY value)",
        ),
    ] {
        let actual = q(
            &c,
            &format!("SELECT {projection} FROM samples {tail} ORDER BY category,value"),
        );
        let expected = q(
            &c,
            &format!("SELECT {projection} FROM baseline {tail} ORDER BY category,value"),
        );
        assert_eq!(actual.rows, expected.rows, "{projection}");
    }
}
#[test]
fn window_results_flow_through_validated_insert_select() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE samples");
    q(&c, "INSERT INTO samples {value:2}");
    q(&c, "INSERT INTO samples {value:5}");
    q(&c, "CREATE TABLE ranked");
    q(
        &c,
        "DEFINE FIELD position ON ranked TYPE integer CHECK (position < 2)",
    );
    assert!(c
        .execute(
            "INSERT INTO ranked (position) SELECT row_number() OVER (ORDER BY value) FROM samples",
            &Parameters::new()
        )
        .is_err());
    assert!(q(&c, "SELECT * FROM ranked").rows.is_empty());
    // Window functions remain invalid in RETURNING and predicates.
    assert!(c
        .execute(
            "UPDATE samples SET value=3 RETURNING row_number() OVER ()",
            &Parameters::new()
        )
        .is_err());
    assert!(c
        .execute(
            "SELECT value FROM samples WHERE row_number() OVER ()=1",
            &Parameters::new()
        )
        .is_err());
}

#[test]
fn supported_frames_and_lag_match_native_and_local_aggregate_order_is_rejected() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE samples");
    q(&c, "CREATE TABLE baseline(value INTEGER)");
    q(&c, "INSERT INTO samples(value) VALUES(1),(2),(3)");
    q(&c, "INSERT INTO baseline VALUES(1),(2),(3)");
    for projection in [
        "sum(value) OVER (ORDER BY value ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)",
        "lag(value) OVER (ORDER BY value)",
    ] {
        assert_eq!(
            q(
                &c,
                &format!("SELECT {projection} FROM samples ORDER BY value")
            )
            .rows,
            q(
                &c,
                &format!("SELECT {projection} FROM baseline ORDER BY value")
            )
            .rows
        );
    }
    assert!(c
        .execute(
            "SELECT sum(value ORDER BY value) OVER () FROM samples",
            &Parameters::new()
        )
        .is_err());
}

#[test]
fn grouped_windows_preserve_collation_having_and_pagination() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE samples");
    q(&c, "CREATE TABLE baseline(n INTEGER,t TEXT)");
    q(&c, "INSERT INTO samples (id,n,t) VALUES(samples:a,1,'a'),(samples:b,2,'A'),(samples:c,3,'b'),(samples:d,NULL,NULL),(samples:e,2,'a')");
    q(
        &c,
        "INSERT INTO baseline VALUES(1,'a'),(2,'A'),(3,'b'),(NULL,NULL),(2,'a')",
    );
    let params = Parameters::from([
        ("$minimum".into(), romedb::Value::Integer(1)),
        ("$limit".into(), romedb::Value::Integer(2)),
        ("$offset".into(), romedb::Value::Integer(1)),
    ]);
    for projection in [
        "t COLLATE NOCASE AS k,sum(n) AS total,sum(sum(n)) OVER(ORDER BY t COLLATE NOCASE) AS running",
        "t COLLATE NOCASE AS k,count(*) AS total,sum(count(*)) OVER() AS all_count",
        "t COLLATE NOCASE AS k,max(n) AS total,row_number() OVER(ORDER BY max(n) DESC) AS position",
    ] {
        for tail in [
            "GROUP BY k ORDER BY k",
            "GROUP BY k HAVING count(*)>$minimum ORDER BY k",
            "GROUP BY k ORDER BY k LIMIT $limit OFFSET $offset",
        ] {
            let params = params
                .iter()
                .filter(|(name, _)| tail.contains(name.as_str()))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
            let native = format!("SELECT {projection} FROM baseline {tail}");
            let sql = format!("SELECT {projection} FROM samples {tail}");
            let expected = c.execute(&native, &params).unwrap();
            for actual in [
                c.execute(&sql, &params).unwrap(),
                c.profile_select(&sql, &params).unwrap().result,
            ] {
                assert_eq!(actual.columns, expected.columns, "{sql}");
                assert_eq!(actual.rows, expected.rows, "{sql}");
                assert_eq!(actual.affected, 0, "{sql}");
            }
        }
    }
}

#[test]
fn grouped_window_insert_failure_preserves_prior_work_and_indexes() {
    use romedb::{IntegrityLimits, TransactionState, Value};
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE samples");
    q(
        &c,
        "INSERT INTO samples (n,t) VALUES(1,'a'),(2,'A'),(3,'b'),(2,'a')",
    );
    q(&c, "CREATE TABLE totals");
    q(
        &c,
        "DEFINE FIELD running ON totals TYPE integer CHECK(running<7)",
    );
    q(&c, "CREATE UNIQUE INDEX totals_running ON totals(running)");
    q(&c, "INSERT INTO totals {id:totals:saved,running:1}");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO totals {id:totals:pending,running:2}");
    let source = "SELECT t COLLATE NOCASE AS k,sum(sum(n)) OVER(ORDER BY t COLLATE NOCASE) AS running FROM samples GROUP BY k";
    let sql = format!("INSERT INTO totals (category,running) {source} ORDER BY k");
    let error = c.execute(&sql, &Parameters::new()).unwrap_err();
    assert_eq!(error.code(), "FDB_VALIDATION");
    assert_eq!(c.transaction_state(), TransactionState::Active);
    assert_eq!(
        q(&c, "SELECT running FROM totals ORDER BY running").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    assert!(c
        .lookup_index("totals", "totals_running", &Value::Integer(5))
        .unwrap()
        .is_empty());
    assert_eq!(
        c.check_collection_integrity("totals", IntegrityLimits::default())
            .unwrap()
            .documents,
        2
    );
    let retry = format!("INSERT INTO totals (category,running) {source} HAVING count(*)>1 ORDER BY k RETURNING running");
    assert_eq!(q(&c, &retry).rows, vec![vec![Value::Integer(5)]]);
    assert_eq!(
        c.lookup_index("totals", "totals_running", &Value::Integer(5))
            .unwrap()
            .len(),
        1
    );
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT running FROM totals").rows,
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(
        c.check_collection_integrity("totals", IntegrityLimits::default())
            .unwrap()
            .documents,
        1
    );
}
