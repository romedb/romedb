use romedb::{Database, DdlWorkLimits, Parameters, ResultLimits, TransactionState, Value};
fn limits() -> ResultLimits {
    ResultLimits {
        max_rows: 1000,
        max_payload_bytes: 1_000_000,
    }
}
fn q(c: &romedb::Connection, sql: &str) {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}
fn seed(c: &romedb::Connection) {
    q(c, "CREATE TABLE source(id INTEGER PRIMARY KEY, n INTEGER)");
    q(c, "INSERT INTO source VALUES(1,11),(2,22),(3,33)");
    q(c, "CREATE INDEX source_n ON source(n)");
    q(c, "CREATE TABLE docs");
    q(c, "INSERT INTO docs {id:docs:a,n:11}");
    q(c, "CREATE INDEX docs_n ON docs(n)");
}
#[test]
fn vector_explain_rejects_without_falling_back_to_unmetered_execution() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE vectors");
    q(
        &c,
        "CREATE SEARCH INDEX vec ON vectors(v) USING VECTOR WITH(dimensions=2,metric='l2')",
    );
    q(&c, "INSERT INTO vectors {id:vectors:a,v:vector32('[1,0]')}");
    assert!(
        c.explain_metered(
            "SELECT id FROM search::vector('vec',vector32('[1,0]'),1)",
            &Parameters::new(),
            limits(),
            Default::default(),
        )
        .is_none(),
        "another statement family must not receive an EXPLAIN rejection"
    );
    q(&c, "BEGIN");
    let measured = c
        .explain_metered(
            "EXPLAIN QUERY PLAN SELECT id FROM search::vector('vec',vector32('[1,0]'),1)",
            &Parameters::new(),
            limits(),
            Default::default(),
        )
        .expect("unsupported vector explanation must retain measured rejection");
    assert_eq!(measured.outcome.unwrap_err().code(), "FDB_UNSUPPORTED");
    assert_eq!(measured.work.rows_read, 0);
    assert_eq!(measured.work.row_mutations, 0);
    assert_eq!(c.transaction_state(), TransactionState::Active);
    q(&c, "ROLLBACK");
}
#[test]
fn ordinary_views_count_schema_work_and_never_execute_their_select() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        ":memory:".to_owned(),
        dir.path().join("views.db").to_str().unwrap().to_owned(),
    ] {
        let c = Database::open(&path).unwrap().connect().unwrap();
        seed(&c);
        for sql in [
            "CREATE VIEW main.v AS SELECT n FROM source",
            "CREATE VIEW IF NOT EXISTS v AS SELECT n FROM source",
            "CREATE VIEW bad_eval AS SELECT abs(-9223372036854775808) FROM source",
        ] {
            let m = c
                .ddl_metered(
                    sql,
                    &Parameters::new(),
                    limits(),
                    DdlWorkLimits {
                        max_rows_read: Some(0),
                        max_row_mutations: Some(0),
                        ..Default::default()
                    },
                )
                .unwrap();
            m.outcome.unwrap_or_else(|e| panic!("{sql}: {e}"));
            assert_eq!(m.work.rows_read, 0, "{sql}: {:?}", m.work);
            assert_eq!(m.work.row_mutations, 0);
            assert!(m.work.vm_steps > 0);
        }
        let selected = c.select_metered(
            "SELECT n FROM v ORDER BY n",
            &Parameters::new(),
            limits(),
            Default::default(),
        );
        assert_eq!(selected.outcome.unwrap().rows.len(), 3);
        assert_eq!(selected.work.rows_read, 3);
        assert!(c
            .execute("SELECT * FROM bad_eval", &Parameters::new())
            .is_err());
        q(&c, "BEGIN");
        q(&c, "INSERT INTO source VALUES(4,44)");
        for sql in ["CREATE VIEW pending AS SELECT * FROM source", "DROP VIEW v"] {
            q(&c, "SAVEPOINT view_probe");
            let baseline = c
                .ddl_metered(sql, &Parameters::new(), limits(), Default::default())
                .unwrap();
            baseline.outcome.unwrap();
            q(&c, "ROLLBACK TO view_probe");
            q(&c, "RELEASE view_probe");
            assert!(baseline.work.vm_steps > 1);
            for work in [
                DdlWorkLimits {
                    max_vm_steps: Some(baseline.work.vm_steps - 1),
                    ..Default::default()
                },
                DdlWorkLimits {
                    max_schema_rows_read: Some(0),
                    ..Default::default()
                },
                DdlWorkLimits {
                    max_vm_steps: Some(0),
                    ..Default::default()
                },
            ] {
                let m = c
                    .ddl_metered(sql, &Parameters::new(), limits(), work)
                    .unwrap();
                assert!(m.outcome.is_err(), "{sql}");
                assert!(m.work.schema_budget_exhausted || m.work.vm_budget_exhausted);
                assert_eq!(m.work.row_mutations, 0);
                assert_eq!(c.transaction_state(), TransactionState::Active);
                assert_eq!(
                    c.execute("SELECT * FROM v", &Parameters::new())
                        .unwrap()
                        .rows
                        .len(),
                    4
                );
                assert!(c
                    .execute("SELECT * FROM pending", &Parameters::new())
                    .is_err());
            }
        }
        q(&c, "ROLLBACK");
        for sql in ["DROP VIEW bad_eval", "DROP VIEW IF EXISTS bad_eval"] {
            let m = c
                .ddl_metered(sql, &Parameters::new(), limits(), Default::default())
                .unwrap();
            m.outcome.unwrap();
            assert_eq!(m.work.rows_read, 0);
            assert_eq!(m.work.row_mutations, 0);
        }
        if path != ":memory:" {
            drop(c);
            let c = Database::open(&path).unwrap().connect().unwrap();
            assert_eq!(
                c.execute("SELECT * FROM v", &Parameters::new())
                    .unwrap()
                    .rows
                    .len(),
                3
            );
        }
    }
}
#[test]
fn explain_does_not_execute_reads_writes_or_schema_effects() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    seed(&c);
    for sql in [
        "EXPLAIN SELECT * FROM source",
        "EXPLAIN QUERY PLAN SELECT * FROM source WHERE n=22",
        "EXPLAIN SELECT n FROM docs",
        "EXPLAIN QUERY PLAN SELECT n FROM docs WHERE n=11",
        "EXPLAIN INSERT INTO source VALUES(4,44)",
        "EXPLAIN UPDATE source SET n=0",
        "EXPLAIN DELETE FROM source",
        "EXPLAIN CREATE TABLE absent(n INTEGER)",
        "EXPLAIN DROP TABLE source",
        "EXPLAIN SELECT abs(-9223372036854775808) FROM source",
    ] {
        let m = c
            .explain_metered(
                sql,
                &Parameters::new(),
                limits(),
                DdlWorkLimits {
                    max_rows_read: Some(0),
                    max_row_mutations: Some(0),
                    max_vm_steps: None,
                    ..Default::default()
                },
            )
            .unwrap();
        let result = m.outcome.unwrap_or_else(|e| panic!("{sql}: {e}"));
        assert!(!result.rows.is_empty());
        assert_eq!(m.work.rows_read, 0, "{sql}: {:?}", m.work);
        assert_eq!(m.work.row_mutations, 0);
        assert!(m.work.vm_steps > 0);
        let stopped = c
            .explain_metered(
                sql,
                &Parameters::new(),
                limits(),
                DdlWorkLimits {
                    max_vm_steps: Some(m.work.vm_steps - 1),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(stopped.outcome.is_err(), "{sql}");
        assert!(stopped.work.vm_budget_exhausted);
        assert_eq!(stopped.work.vm_steps, m.work.vm_steps - 1);
        assert_eq!(stopped.work.rows_read, 0);
        assert_eq!(stopped.work.row_mutations, 0);
        assert_eq!(
            result.rows,
            c.execute(sql, &Parameters::new()).unwrap().rows
        );
    }
    assert_eq!(
        c.execute("SELECT n FROM source ORDER BY id", &Parameters::new())
            .unwrap()
            .rows,
        vec![
            vec![Value::Integer(11)],
            vec![Value::Integer(22)],
            vec![Value::Integer(33)]
        ]
    );
    assert!(c
        .execute("SELECT * FROM absent", &Parameters::new())
        .is_err());
}
#[test]
fn explain_result_limits_bindings_errors_and_outer_transaction_are_preserved() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    seed(&c);
    q(&c, "BEGIN");
    q(&c, "INSERT INTO source VALUES(4,44)");
    let p = Parameters::from([("$n".into(), Value::Integer(22))]);
    for sql in [
        "EXPLAIN SELECT * FROM source WHERE n=$n",
        "EXPLAIN QUERY PLAN SELECT * FROM source WHERE n=$n",
    ] {
        let m = c
            .explain_metered(sql, &p, limits(), Default::default())
            .unwrap();
        assert!(!m.outcome.unwrap().rows.is_empty());
        assert_eq!(m.work.rows_read, 0);
        for limit in [
            ResultLimits {
                max_rows: 0,
                ..limits()
            },
            ResultLimits {
                max_payload_bytes: 1,
                ..limits()
            },
        ] {
            let m = c
                .explain_metered(sql, &p, limit, Default::default())
                .unwrap();
            assert!(m.outcome.is_err());
            assert_eq!(m.work.row_mutations, 0);
            assert_eq!(c.transaction_state(), TransactionState::Active);
        }
    }
    for sql in [
        "EXPLAIN SELECT * FROM missing",
        "EXPLAIN SELECT",
        "EXPLAIN SELECT 1; DELETE FROM source",
        "EXPLAIN SELECT * FROM __romedb_catalog",
    ] {
        let m = c
            .explain_metered(sql, &Parameters::new(), limits(), Default::default())
            .unwrap();
        assert!(m.outcome.is_err(), "{sql}");
        assert_eq!(m.work.row_mutations, 0);
    }
    assert!(c
        .explain_metered("EXPLAIN SELECT 1", &p, limits(), Default::default())
        .unwrap()
        .outcome
        .is_err());
    assert!(c
        .explain_metered(
            "DELETE FROM source",
            &Parameters::new(),
            limits(),
            Default::default()
        )
        .is_none());
    assert_eq!(
        c.execute("SELECT * FROM source", &Parameters::new())
            .unwrap()
            .rows
            .len(),
        4
    );
    q(&c, "ROLLBACK");
    assert_eq!(
        c.execute("SELECT * FROM source", &Parameters::new())
            .unwrap()
            .rows
            .len(),
        3
    );
}
#[test]
fn view_coverage_preserves_temp_resolution_and_managed_guards() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    seed(&c);
    q(&c, "CREATE VIEW v AS SELECT n FROM source");
    q(&c, "CREATE TEMP TABLE v(n INTEGER)");
    q(&c, "INSERT INTO temp.v VALUES(99)");
    for sql in [
        "CREATE TEMP VIEW other AS SELECT 1",
        "CREATE MATERIALIZED VIEW other AS SELECT n FROM source",
        "DROP VIEW temp.v",
        "DROP VIEW v",
    ] {
        assert!(
            c.ddl_metered(sql, &Parameters::new(), limits(), Default::default())
                .is_none(),
            "{sql}"
        );
    }
    c.ddl_metered(
        "DROP VIEW main.v",
        &Parameters::new(),
        limits(),
        Default::default(),
    )
    .unwrap()
    .outcome
    .unwrap();
    assert_eq!(
        c.execute("SELECT n FROM v", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::Integer(99)]]
    );
    for sql in [
        "CREATE VIEW bad AS SELECT * FROM docs",
        "CREATE VIEW bad AS SELECT * FROM __romedb_catalog",
    ] {
        assert!(c
            .ddl_metered(sql, &Parameters::new(), limits(), Default::default())
            .unwrap()
            .outcome
            .is_err());
    }
}
#[test]
fn explain_search_distinguishes_nonexecuting_plans_from_materialized_ann() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    for n in 1..=5 {
        q(&c,&format!("INSERT INTO docs {{id:docs:d{n},title:'hello world',location:geo::point(0,0),v:vector32('[1,0]')}}"));
    }
    for (create,query) in [
        ("CREATE SEARCH INDEX docs_text ON docs(title) USING FULLTEXT","SELECT id FROM search::text('docs_text','hello',10)"),
        ("CREATE SEARCH INDEX docs_spatial ON docs(location) USING SPATIAL","SELECT id FROM search::near('docs_spatial',geo::point(0,0),1000000)"),
        ("CREATE SEARCH INDEX docs_vector ON docs(v) USING VECTOR WITH (metric='l2',dimensions=2)","SELECT id FROM search::vector('docs_vector',vector32('[1,0]'),5)"),
    ] {
        q(&c,create);
        for prefix in ["EXPLAIN", "EXPLAIN QUERY PLAN"] {
            let sql=format!("{prefix} {query}");
            if query.contains("search::vector") {
                let rejected = c.explain_metered(&sql, &Parameters::new(), limits(), Default::default()).unwrap();
                assert_eq!(rejected.outcome.unwrap_err().code(), "FDB_UNSUPPORTED");
                assert_eq!(rejected.work.rows_read, 0);
                assert_eq!(rejected.work.row_mutations, 0);
                assert!(!c.execute(&sql, &Parameters::new()).unwrap().rows.is_empty(), "ordinary compatibility path is preserved");
                continue;
            }
            let m=c.explain_metered(&sql,&Parameters::new(),limits(),DdlWorkLimits{max_rows_read:Some(0),max_row_mutations:Some(0),..Default::default()}).unwrap();
            m.outcome.unwrap_or_else(|e|panic!("{sql}: {e}; {:?}",m.work));
            assert_eq!(m.work.rows_read,0);assert_eq!(m.work.row_mutations,0);
        }
    }
}

#[test]
fn spatial_explain_reads_no_documents_or_index_entries() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE drivers");
    q(&c, "CREATE TABLE zones");
    q(&c, "BEGIN");
    for i in 0..500u64 {
        let longitude = ((i * 7919) % 36001) as f64 / 100.0 - 180.0;
        let latitude = ((i * 104_729) % 17001) as f64 / 100.0 - 85.0;
        let fleet = if i % 100 == 0 { "night" } else { "day" };
        q(&c, &format!(
            "INSERT INTO drivers {{id:type::record('drivers',{i}),available:{},fleet:'{fleet}',location:geo::point({longitude},{latitude})}}",
            i % 3 == 0
        ));
    }
    q(
        &c,
        "INSERT INTO zones {id:zones:a,shape:ST_MakeEnvelope(0, 0, 20, 20)}",
    );
    q(
        &c,
        "CREATE SEARCH INDEX drivers_location ON drivers(location) USING SPATIAL",
    );
    q(
        &c,
        "CREATE SEARCH INDEX zones_shape ON zones(shape) USING SPATIAL WITH (geometry = 'region')",
    );
    q(&c, "CREATE INDEX drivers_fleet ON drivers(fleet)");
    q(&c, "COMMIT");
    let nearest = |filter: &str| {
        format!("SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point(10, 10)) IS NOT NULL AND {filter} ORDER BY ST_DistanceSphere(location, ST_Point(10, 10)), id LIMIT 5")
    };
    let mut readers = Vec::new();
    for (sql, index) in [
        (nearest("available = true"), "drivers_location"),
        (nearest("fleet = 'day'"), "drivers_location"),
        // EXPLAIN reads nothing, so it cannot count the fleet; it shows the
        // spatial plan, and execution may still choose the fleet index.
        (nearest("fleet = 'night'"), "drivers_location"),
        ("SELECT id FROM drivers WHERE ST_DWithin(location, ST_Point(10, 10), 500000) AND fleet = 'day'".to_owned(), "drivers_location"),
        ("SELECT id FROM drivers WHERE ST_Covers(ST_MakeEnvelope(0, 0, 20, 20), location)".to_owned(), "drivers_location"),
        ("SELECT id FROM zones WHERE ST_Covers(shape, ST_Point(10, 10))".to_owned(), "zones_shape"),
        ("SELECT id FROM search::near('drivers_location', ST_Point(10, 10), 500000)".to_owned(), "drivers_location"),
        ("SELECT id FROM search::bbox('drivers_location', 0, 0, 20, 20)".to_owned(), "drivers_location"),
        ("SELECT id FROM search::polygon('drivers_location', ST_MakeEnvelope(0, 0, 20, 20), 'covers')".to_owned(), "drivers_location"),
        ("SELECT id FROM search::nearest('drivers_location', ST_Point(10, 10), 5)".to_owned(), "drivers_location"),
        ("SELECT id FROM search::containing('zones_shape', ST_Point(10, 10), 'covers')".to_owned(), "zones_shape"),
    ] {
        for explain in ["EXPLAIN QUERY PLAN", "EXPLAIN"] {
            let statement = format!("{explain} {sql}");
            let measured = c
                .explain_metered(&statement, &Parameters::new(), limits(), Default::default())
                .expect(&statement);
            let rows = match measured.outcome {
                Ok(result) => result.rows,
                Err(error) => panic!("{statement}: {error}"),
            };
            if measured.work.rows_read != 0 {
                readers.push(format!("{} rows read: {statement}", measured.work.rows_read));
            }
            assert_eq!(measured.work.row_mutations, 0, "{statement}");
            if explain == "EXPLAIN QUERY PLAN" {
                assert!(format!("{rows:?}").contains(index), "{statement}: {rows:?}");
            }
        }
    }
    assert!(readers.is_empty(), "{readers:#?}");
}
