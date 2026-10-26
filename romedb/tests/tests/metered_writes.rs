use romedb::{Database, Parameters, ResultLimits, TransactionState, WriteWorkLimits};
fn limits() -> ResultLimits {
    ResultLimits {
        max_rows: 100,
        max_payload_bytes: 65536,
    }
}
fn budget(n: u64) -> WriteWorkLimits {
    WriteWorkLimits {
        max_row_mutations: Some(n),
        ..Default::default()
    }
}
#[test]
fn native_and_document_mutations_exclude_managed_indexes() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        ":memory:".to_string(),
        dir.path().join("writes.db").to_str().unwrap().to_string(),
    ] {
        let c = Database::open(&path).unwrap().connect().unwrap();
        let p = Parameters::new();
        for sql in [
            "CREATE TABLE native(id INTEGER PRIMARY KEY,n INTEGER UNIQUE)",
            "CREATE TABLE docs",
            "CREATE UNIQUE INDEX docs_n ON docs(n)",
            "CREATE INDEX docs_extra ON docs(n)",
        ] {
            c.execute(sql, &p).unwrap();
        }
        for (sql, n) in [
            ("INSERT INTO native VALUES(1,10),(2,20)", 2),
            ("UPDATE native SET n=n", 2),
            ("INSERT OR REPLACE INTO native VALUES(1,20)", 3),
            ("DELETE FROM native", 1),
            ("INSERT INTO docs {id:docs:a,n:1}", 1),
            ("UPDATE docs SET n=n WHERE id=docs:a", 1),
            ("INSERT INTO docs {id:docs:b,n:2}", 1),
            ("INSERT OR REPLACE INTO docs(id,n) VALUES(docs:a,2)", 3),
            ("DELETE FROM docs WHERE id=docs:a", 1),
            ("DELETE FROM docs WHERE id=docs:missing", 0),
        ] {
            let r = c.write_metered(sql, &p, limits(), budget(n));
            r.outcome.unwrap_or_else(|e| panic!("{sql}: {e}"));
            assert_eq!(r.work.row_mutations, n, "{sql}");
            assert!(!r.work.mutation_budget_exhausted, "{sql}");
        }
    }
}
#[test]
fn failure_retains_work_and_restores_data_indexes_and_caller_transaction() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let p = Parameters::new();
    for sql in [
        "CREATE TABLE native(n INTEGER)",
        "CREATE TABLE docs",
        "CREATE INDEX docs_n ON docs(n)",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "BEGIN",
        "INSERT INTO native VALUES(99)",
    ] {
        c.execute(sql, &p).unwrap();
    }
    for sql in [
        "INSERT INTO native VALUES(1),(2),(3)",
        "UPDATE docs SET n=n+10",
        "UPDATE docs CONTENT {n:n+10}",
        "UPDATE docs MERGE {n:n+10}",
        "UPDATE docs PATCH [{op:'replace',path:'/n',value:n+10}]",
    ] {
        let r = c.write_metered(sql, &p, limits(), budget(1));
        assert!(r.outcome.is_err(), "{sql}");
        assert_eq!(r.work.row_mutations, 2, "{sql}");
        assert!(r.work.mutation_budget_exhausted);
        assert_eq!(c.transaction_state(), TransactionState::Active);
    }
    assert_eq!(c.execute("SELECT n FROM native", &p).unwrap().rows.len(), 1);
    assert_eq!(
        c.execute("SELECT n FROM docs WHERE n<10", &p)
            .unwrap()
            .rows
            .len(),
        2
    );
    assert_eq!(
        c.execute("SELECT n FROM docs WHERE n>10", &p)
            .unwrap()
            .rows
            .len(),
        0
    );
    let r = c.write_metered("UPDATE docs SET n=n+1", &p, limits(), budget(2));
    assert!(r.outcome.is_ok());
    assert_eq!(r.work.row_mutations, 2);
    c.execute("ROLLBACK", &p).unwrap();
    assert_eq!(
        r.work.row_mutations, 2,
        "snapshot remains attempted work after rollback"
    );
    assert_eq!(c.execute("SELECT n FROM native", &p).unwrap().rows.len(), 0);
}
#[test]
fn read_result_and_vm_budgets_reject_atomically_and_ddl_is_not_a_write() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let p = Parameters::new();
    for sql in [
        "CREATE TABLE native(n INTEGER)",
        "INSERT INTO native VALUES(1),(2),(3)",
    ] {
        c.execute(sql, &p).unwrap();
    }
    let r = c.write_metered(
        "UPDATE native SET n=n+1",
        &p,
        limits(),
        WriteWorkLimits {
            max_rows_read: Some(1),
            ..Default::default()
        },
    );
    assert!(r.outcome.is_err());
    assert_eq!(r.work.rows_read, 2);
    assert!(r.work.read_budget_exhausted);
    let before = c
        .execute("SELECT n FROM native ORDER BY n", &p)
        .unwrap()
        .rows;
    let r = c.write_metered(
        "UPDATE native SET n=n+1 RETURNING n",
        &p,
        ResultLimits {
            max_rows: 1,
            ..limits()
        },
        WriteWorkLimits::default(),
    );
    assert!(r.outcome.is_err());
    assert!(r.work.row_mutations > 0);
    assert_eq!(
        c.execute("SELECT n FROM native ORDER BY n", &p)
            .unwrap()
            .rows,
        before
    );
    let r = c.write_metered(
        "INSERT INTO native VALUES(4)",
        &p,
        limits(),
        WriteWorkLimits {
            max_vm_steps: Some(0),
            ..Default::default()
        },
    );
    assert!(r.outcome.is_err());
    assert_eq!(r.work.row_mutations, 0);
    assert_eq!(r.work.vm_steps, 0);
    for sql in [
        "CREATE TABLE forbidden(n INTEGER)",
        "BEGIN",
        "SELECT * FROM native",
    ] {
        let r = c.write_metered(sql, &p, limits(), WriteWorkLimits::default());
        assert!(r.outcome.is_err());
        assert_eq!(r.work.vm_steps, 0);
    }
    assert_eq!(
        c.execute("SELECT n FROM native ORDER BY n", &p)
            .unwrap()
            .rows,
        before
    );
}

#[test]
fn source_reads_failures_and_catalog_exclusion_share_the_write_scope() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let p = Parameters::new();
    for sql in [
        "CREATE TABLE source(n INTEGER)",
        "INSERT INTO source VALUES(1),(2),(3)",
        "CREATE TABLE target(n INTEGER UNIQUE)",
        "CREATE TABLE docs",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
    ] {
        c.execute(sql, &p).unwrap();
    }
    let r = c.write_metered(
        "INSERT INTO target SELECT n FROM source",
        &p,
        limits(),
        WriteWorkLimits::default(),
    );
    r.outcome.unwrap();
    assert_eq!(r.work.row_mutations, 3);
    assert_eq!(r.work.rows_read, 3);
    let r = c.write_metered(
        "INSERT INTO docs(n) SELECT n FROM source",
        &p,
        limits(),
        WriteWorkLimits::default(),
    );
    r.outcome.unwrap();
    assert_eq!(r.work.row_mutations, 3);
    assert_eq!(r.work.rows_read, 3);
    let failed = c.write_metered(
        "INSERT INTO target VALUES(4),(2)",
        &p,
        limits(),
        WriteWorkLimits::default(),
    );
    assert!(failed.outcome.is_err());
    assert_eq!(failed.work.row_mutations, 1);
    assert!(c
        .execute("SELECT n FROM target WHERE n=4", &p)
        .unwrap()
        .rows
        .is_empty());
    let failed = c.write_metered(
        "INSERT INTO docs(n) VALUES(4),(2)",
        &p,
        limits(),
        WriteWorkLimits::default(),
    );
    assert!(failed.outcome.is_err());
    assert_eq!(
        failed.work.row_mutations, 2,
        "failed index constraint retains primary attempt"
    );
    assert!(c
        .execute("SELECT n FROM docs WHERE n=4", &p)
        .unwrap()
        .rows
        .is_empty());
    let sql = "UPDATE docs SET n=n WHERE n=1";
    let before = c.write_metered(sql, &p, limits(), WriteWorkLimits::default());
    before.outcome.unwrap();
    for i in 0..10 {
        c.execute(&format!("CREATE TABLE extra_{i}"), &p).unwrap();
    }
    let after = c.write_metered(sql, &p, limits(), WriteWorkLimits::default());
    after.outcome.unwrap();
    assert_eq!(before.work.rows_read, after.work.rows_read);
    assert_eq!(before.work.vm_steps, after.work.vm_steps);
    assert_eq!(after.work.row_mutations, 1);
}

#[test]
fn object_writes_share_lookup_and_mutation_budgets() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let p = Parameters::new();
    c.execute("CREATE TABLE docs", &p).unwrap();
    for (sql, n) in [
        ("UPSERT docs:a {n:1} RETURNING n", 1),
        ("UPSERT docs:a {n:n+1} RETURNING n", 1),
        ("UPDATE docs:a {n:n+1} RETURNING n", 1),
        ("UPDATE docs {n:n+1} WHERE n>0 RETURNING n", 1),
        ("DELETE FROM docs:a RETURNING n", 1),
        ("DELETE FROM docs:missing RETURNING n", 0),
    ] {
        let r = c.write_metered(sql, &p, limits(), budget(n));
        r.outcome.unwrap_or_else(|e| panic!("{sql}: {e}"));
        assert_eq!(r.work.row_mutations, n, "{sql}");
    }
    c.execute("UPSERT docs:a {n:1}", &p).unwrap();
    let r = c.write_metered(
        "UPDATE docs:a {n:2}",
        &p,
        limits(),
        WriteWorkLimits {
            max_rows_read: Some(0),
            ..Default::default()
        },
    );
    assert!(r.outcome.is_err());
    assert_eq!(r.work.rows_read, 1);
    assert_eq!(r.work.row_mutations, 0);
    let r = c.write_metered("DELETE FROM docs:a", &p, limits(), budget(0));
    assert!(r.outcome.is_err());
    assert_eq!(r.work.row_mutations, 1);
    assert_eq!(c.execute("SELECT docs:a", &p).unwrap().rows.len(), 1);
}

#[test]
fn index_maintenance_consumes_shared_reads_and_vm_budget() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let p = Parameters::new();
    for sql in [
        "CREATE TABLE plain",
        "CREATE TABLE indexed",
        "CREATE INDEX indexed_n ON indexed(n)",
        "INSERT INTO plain {id:plain:a,n:1}",
        "INSERT INTO indexed {id:indexed:a,n:1}",
    ] {
        c.execute(sql, &p).unwrap();
    }
    let plain = c.write_metered(
        "UPDATE plain:a {n:1}",
        &p,
        limits(),
        WriteWorkLimits::default(),
    );
    plain.outcome.unwrap();
    let indexed = c.write_metered("UPDATE indexed:a {n:1}", &p, limits(), budget(1));
    indexed.outcome.unwrap();
    assert_eq!(plain.work.row_mutations, 1);
    assert_eq!(indexed.work.row_mutations, 1);
    assert!(
        indexed.work.rows_read > plain.work.rows_read,
        "{:#?} vs {:#?}",
        indexed.work,
        plain.work
    );
    assert!(indexed.work.vm_steps > plain.work.vm_steps);
    for work_limits in [
        WriteWorkLimits {
            max_rows_read: Some(plain.work.rows_read),
            ..Default::default()
        },
        WriteWorkLimits {
            max_vm_steps: Some(indexed.work.vm_steps - 1),
            ..Default::default()
        },
    ] {
        let r = c.write_metered("UPDATE indexed:a {n:2}", &p, limits(), work_limits);
        assert!(r.outcome.is_err());
        // Indexed updates read the previous document before the primary write
        // to maintain entries by key, so the shared budget can end before or
        // after it; either way nothing survives the failed statement.
        assert!(r.work.row_mutations <= 1, "{:#?}", r.work);
        assert!(r.work.read_budget_exhausted || r.work.vm_budget_exhausted);
        assert_eq!(
            c.execute("SELECT n FROM indexed WHERE n=1", &p)
                .unwrap()
                .rows
                .len(),
            1
        );
        assert!(c
            .execute("SELECT n FROM indexed WHERE n=2", &p)
            .unwrap()
            .rows
            .is_empty());
    }
    let exact = c.write_metered(
        "UPDATE indexed:a {n:1}",
        &p,
        limits(),
        WriteWorkLimits {
            max_rows_read: Some(indexed.work.rows_read),
            max_row_mutations: Some(1),
            max_vm_steps: Some(indexed.work.vm_steps),
        },
    );
    exact.outcome.unwrap();
    assert_eq!(exact.work.rows_read, indexed.work.rows_read);
    assert_eq!(exact.work.vm_steps, indexed.work.vm_steps);
}

#[test]
fn search_index_maintenance_does_not_add_logical_writes() {
    for ddl in [
        "CREATE SEARCH INDEX docs_search ON docs(title) USING FULLTEXT",
        "CREATE SEARCH INDEX docs_search ON docs(v) USING VECTOR WITH (metric='l2',dimensions=2)",
    ] {
        let c = Database::open(":memory:").unwrap().connect().unwrap();
        let p = Parameters::new();
        c.execute("CREATE TABLE docs", &p).unwrap();
        c.execute(ddl, &p).unwrap();
        for sql in [
            "INSERT INTO docs {id:docs:a,title:'hello',v:vector32('[1,2]')}",
            "UPDATE docs:a {title:'changed',v:vector32('[2,3]')}",
        ] {
            let r = c.write_metered(sql, &p, limits(), budget(1));
            r.outcome.unwrap_or_else(|e| panic!("{ddl}: {sql}: {e}"));
            assert_eq!(r.work.row_mutations, 1);
            c.check_collection_integrity("docs", Default::default())
                .unwrap();
        }
        let failed = c.write_metered("DELETE FROM docs:a", &p, limits(), budget(0));
        assert!(failed.outcome.is_err());
        assert_eq!(failed.work.row_mutations, 1);
        assert_eq!(c.execute("SELECT docs:a", &p).unwrap().rows.len(), 1);
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
        let r = c.write_metered("DELETE FROM docs:a", &p, limits(), budget(1));
        r.outcome.unwrap();
        assert_eq!(r.work.row_mutations, 1);
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
}
