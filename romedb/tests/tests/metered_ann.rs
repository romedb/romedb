use romedb::{Database, Parameters, ReadWorkLimits, ResultLimits, TransactionState};

fn q(c: &romedb::Connection, sql: &str) {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}
fn seed(c: &romedb::Connection, count: usize) {
    q(c, "CREATE TABLE docs");
    for n in 0..count {
        q(
            c,
            &format!("INSERT INTO docs {{id:docs:d{n},v:vector32('[{n},0]')}}"),
        );
    }
    c.create_vector_index("docs", "docs_vec", vec!["v".into()], 2, "l2", false)
        .unwrap();
}
fn sql(limit: usize) -> String {
    format!("SELECT id FROM search::vector('docs_vec',vector32('[0,0]'),{limit})")
}
fn result_limits() -> ResultLimits {
    ResultLimits {
        max_rows: 100,
        max_payload_bytes: 65536,
    }
}
fn read(c: &romedb::Connection, sql: &str, max: Option<u64>) -> romedb::MeteredRead {
    c.select_metered(
        sql,
        &Parameters::new(),
        result_limits(),
        ReadWorkLimits {
            max_rows_read: max,
            max_vm_steps: None,
        },
    )
}

#[test]
fn candidate_lookups_are_counted_before_outer_hits_in_cold_and_warm_queries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ann.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        seed(&db.connect().unwrap(), 5);
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    // Every fresh connection has an empty graph cache; its second query is warm.
    for limit in [1, 5] {
        let c = db.connect().unwrap();
        // ANN requests min(4 * limit, graph size) candidates. Each candidate
        // requires one rowid lookup, then the materialized hit table is visited
        // once per result. These expectations are independent of meter output.
        let candidates = (4 * limit).min(5) as u64;
        for _ in 0..2 {
            let m = read(&c, &sql(limit), Some(candidates + limit as u64));
            let rows = m.outcome.unwrap().rows;
            assert_eq!(rows.len(), limit);
            assert_eq!(m.work.rows_read, candidates + limit as u64);
            assert!(!m.work.read_budget_exhausted);
            assert_eq!(
                rows,
                c.search_vectors(
                    "docs_vec",
                    &romedb::Value::vector32(&[0., 0.]).unwrap(),
                    limit
                )
                .unwrap()
                .rows
                .into_iter()
                .map(|r| vec![r[0].clone()])
                .collect::<Vec<_>>()
            );
        }
        let filtered = read(&c, &format!("{} WHERE 0", sql(limit)), Some(candidates));
        assert!(filtered.outcome.unwrap().rows.is_empty());
        assert_eq!(
            filtered.work.rows_read, candidates,
            "eager candidate reads survive an empty outer query"
        );
    }
}

#[test]
fn candidate_and_outer_budgets_retain_failure_work_and_preserve_caller_transaction() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    seed(&c, 5);
    q(&c, "BEGIN");
    q(&c, "CREATE TABLE pending(n INTEGER)");
    q(&c, "INSERT INTO pending VALUES(99)");
    // Limits 0..4 stop within candidate lookup. 5..9 stop in the outer scan.
    for cap in 0..10 {
        let m = read(&c, &sql(5), Some(cap));
        assert_eq!(m.outcome.unwrap_err().code(), "FDB_CANCELLED", "cap={cap}");
        assert_eq!(m.work.rows_read, cap + 1, "cap={cap}");
        assert!(m.work.read_budget_exhausted);
        assert_eq!(c.transaction_state(), TransactionState::Active);
        assert_eq!(
            c.execute("SELECT * FROM pending", &Parameters::new())
                .unwrap()
                .rows
                .len(),
            1
        );
    }
    let vm = c.select_metered(
        &sql(5),
        &Parameters::new(),
        result_limits(),
        ReadWorkLimits {
            max_rows_read: None,
            max_vm_steps: Some(0),
        },
    );
    assert_eq!(vm.outcome.unwrap_err().code(), "FDB_CANCELLED");
    assert_eq!(vm.work.rows_read, 0);
    assert_eq!(vm.work.vm_steps, 0);
    assert!(vm.work.vm_budget_exhausted);
    let result = c.select_metered(
        &sql(5),
        &Parameters::new(),
        ResultLimits {
            max_rows: 0,
            ..result_limits()
        },
        Default::default(),
    );
    assert_eq!(result.outcome.unwrap_err().code(), "FDB_LIMIT");
    assert_eq!(
        result.work.rows_read, 6,
        "five candidates then first emitted row"
    );
    q(&c, "ROLLBACK");
    assert!(c
        .execute("SELECT * FROM pending", &Parameters::new())
        .is_err());
    assert_eq!(read(&c, &sql(5), Some(10)).outcome.unwrap().rows.len(), 5);
}

#[test]
fn empty_invalid_and_replayed_graph_state_do_not_fabricate_candidate_reads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replay.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        seed(&c, 0);
        for query in [sql(5), sql(0)] {
            let m = read(&c, &query, Some(0));
            assert!(m.outcome.unwrap().rows.is_empty());
            assert_eq!(m.work.rows_read, 0);
        }
        // These writes remain in the ANN redo log. Recovery/replay must not be
        // charged as candidate lookups or count a deleted candidate.
        q(&c, "INSERT INTO docs {id:docs:a,v:vector32('[1,0]')}");
        q(&c, "INSERT INTO docs {id:docs:b,v:vector32('[2,0]')}");
        q(&c, "DELETE FROM docs WHERE id=docs:a");
    }
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    for _ in 0..2 {
        let m = read(&c, &sql(5), Some(2));
        assert_eq!(m.outcome.unwrap().rows.len(), 1);
        assert_eq!(m.work.rows_read, 2);
    }
    for query in [
        sql(0),
        "SELECT id FROM search::vector('missing',vector32('[0,0]'),5)".into(),
        "SELECT id FROM search::vector('docs_vec',vector32('[0]'),5)".into(),
    ] {
        assert_eq!(read(&c, &query, Some(0)).work.rows_read, 0);
    }
}
