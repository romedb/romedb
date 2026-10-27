use romedb::{Database, DdlWorkLimits, Parameters, ResultLimits, TransactionState, Value};
fn limits() -> ResultLimits {
    ResultLimits {
        max_rows: 0,
        max_payload_bytes: 65536,
    }
}
fn q(c: &romedb::Connection, sql: &str) {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}
fn m(c: &romedb::Connection, sql: &str, work: DdlWorkLimits) -> romedb::MeteredDdl {
    c.ddl_metered(sql, &Parameters::new(), limits(), work)
        .unwrap()
}
fn seed(c: &romedb::Connection) {
    q(c, "CREATE TABLE source(n INTEGER, extra TEXT)");
    q(c, "INSERT INTO source(n) VALUES(1),(2),(3),(4),(5)");
    q(c, "CREATE INDEX source_n ON source(n)");
    q(c, "CREATE TABLE docs");
    for n in 1..=5 {
        q(c, &format!("INSERT INTO docs {{id:docs:d{n},n:{n}}}"));
    }
    q(c, "CREATE INDEX docs_n ON docs(n)");
}
#[test]
fn drops_count_removed_rows_but_not_index_or_catalog_writes() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        ":memory:".to_owned(),
        dir.path().join("ddl.db").to_str().unwrap().to_owned(),
    ] {
        let c = Database::open(&path).unwrap().connect().unwrap();
        seed(&c);
        q(&c, "CREATE TABLE \"Ä source\"(n INTEGER)");
        q(&c, "INSERT INTO \"Ä source\" VALUES(1),(2),(3),(4),(5)");
        for sql in [
            "DROP INDEX source_n",
            "DROP INDEX docs_n",
            "DROP INDEX IF EXISTS absent",
        ] {
            let x = m(
                &c,
                sql,
                DdlWorkLimits {
                    max_rows_read: Some(0),
                    max_row_mutations: Some(0),
                    ..Default::default()
                },
            );
            x.outcome.unwrap();
            assert_eq!(x.work.row_mutations, 0);
            assert_eq!(x.work.rows_read, 0);
        }
        for sql in [
            "DROP TABLE main.source",
            "DROP TABLE docs",
            "DROP TABLE \"Ä source\"",
        ] {
            let x = m(
                &c,
                sql,
                DdlWorkLimits {
                    max_rows_read: Some(5),
                    max_row_mutations: Some(5),
                    ..Default::default()
                },
            );
            x.outcome.unwrap();
            assert_eq!(x.work.rows_read, 5, "{sql}: {:?}", x.work);
            assert_eq!(x.work.row_mutations, 5);
            assert!(x.work.schema_rows_read > 0);
            assert!(x.work.vm_steps > 0);
        }
        for sql in ["DROP TABLE IF EXISTS source", "DROP TABLE IF EXISTS docs"] {
            let x = m(
                &c,
                sql,
                DdlWorkLimits {
                    max_rows_read: Some(0),
                    max_row_mutations: Some(0),
                    ..Default::default()
                },
            );
            x.outcome.unwrap();
            assert_eq!(x.work.rows_read, 0);
            assert_eq!(x.work.row_mutations, 0);
        }
        assert!(c
            .execute("SELECT * FROM source", &Parameters::new())
            .is_err());
        if path != ":memory:" {
            drop(c);
            let c = Database::open(&path).unwrap().connect().unwrap();
            assert!(c
                .execute("INFO FOR TABLE docs", &Parameters::new())
                .is_err());
        }
    }
}
#[test]
fn drop_preflight_and_engine_limits_preserve_tables_and_caller_transaction() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    seed(&c);
    q(&c, "BEGIN");
    q(&c, "INSERT INTO source(n) VALUES(6)");
    for table in ["source", "docs"] {
        for (work, flag) in [
            (
                DdlWorkLimits {
                    max_rows_read: Some(2),
                    ..Default::default()
                },
                "read",
            ),
            (
                DdlWorkLimits {
                    max_row_mutations: Some(2),
                    ..Default::default()
                },
                "mutation",
            ),
            (
                DdlWorkLimits {
                    max_schema_rows_read: Some(0),
                    ..Default::default()
                },
                "schema",
            ),
            (
                DdlWorkLimits {
                    max_vm_steps: Some(0),
                    ..Default::default()
                },
                "vm",
            ),
        ] {
            let x = m(&c, &format!("DROP TABLE {table}"), work);
            assert!(x.outcome.is_err());
            assert_eq!(x.work.row_mutations, 0);
            match flag {
                "read" => {
                    assert_eq!(x.work.rows_read, 3);
                    assert!(x.work.read_budget_exhausted);
                }
                "mutation" => {
                    assert!(x.work.mutation_budget_exhausted);
                    assert_eq!(x.work.rows_read, if table == "source" { 6 } else { 5 });
                }
                "schema" => assert!(x.work.schema_budget_exhausted),
                "vm" => assert!(x.work.vm_budget_exhausted),
                _ => unreachable!(),
            }
            assert_eq!(c.transaction_state(), TransactionState::Active);
            assert_eq!(
                c.execute(&format!("SELECT * FROM {table}"), &Parameters::new())
                    .unwrap()
                    .rows
                    .len(),
                if table == "source" { 6 } else { 5 }
            );
            q(&c, &format!("INFO FOR INDEX {table}_n"));
        }
    }
    let x = m(&c, "DROP TABLE source", DdlWorkLimits::default());
    x.outcome.unwrap();
    assert_eq!(x.work.row_mutations, 6);
    q(&c, "ROLLBACK");
    assert_eq!(
        c.execute("SELECT * FROM source", &Parameters::new())
            .unwrap()
            .rows
            .len(),
        5
    );
    assert_eq!(
        x.work.row_mutations, 6,
        "attempts survive explicit rollback"
    );
}
#[test]
fn alter_counts_actual_row_rewrites_and_restores_schema_after_budget_failure() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        ":memory:".to_owned(),
        dir.path().join("alter.db").to_str().unwrap().to_owned(),
    ] {
        let c = Database::open(&path).unwrap().connect().unwrap();
        seed(&c);
        for sql in [
            "ALTER TABLE source ADD COLUMN added INTEGER DEFAULT 7",
            "ALTER TABLE source RENAME COLUMN extra TO renamed",
            "ALTER TABLE source RENAME TO renamed_source",
        ] {
            let x = m(
                &c,
                sql,
                DdlWorkLimits {
                    max_row_mutations: Some(0),
                    ..Default::default()
                },
            );
            x.outcome.unwrap();
            assert_eq!(x.work.rows_read, 0, "{sql}: {:?}", x.work);
            assert_eq!(x.work.row_mutations, 0);
            assert!(x.work.schema_rows_read > 0);
        }
        let x = m(
            &c,
            "ALTER TABLE renamed_source DROP COLUMN renamed",
            DdlWorkLimits {
                max_row_mutations: Some(2),
                ..Default::default()
            },
        );
        assert!(x.outcome.is_err());
        assert!(x.work.mutation_budget_exhausted);
        assert_eq!(x.work.row_mutations, 3);
        q(&c, "SELECT renamed FROM renamed_source");
        let x = m(
            &c,
            "ALTER TABLE renamed_source DROP COLUMN renamed",
            DdlWorkLimits {
                max_rows_read: Some(5),
                max_row_mutations: Some(5),
                ..Default::default()
            },
        );
        x.outcome.unwrap();
        assert_eq!(x.work.rows_read, 5);
        assert_eq!(x.work.row_mutations, 5);
        assert!(c
            .execute("SELECT renamed FROM renamed_source", &Parameters::new())
            .is_err());
        assert_eq!(
            c.execute("SELECT added FROM renamed_source", &Parameters::new())
                .unwrap()
                .rows[0][0],
            Value::Integer(7)
        );
    }
}
#[test]
fn invalid_and_uncovered_ddl_does_not_destroy_data_or_bypass_dependencies() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    seed(&c);
    q(&c, "CREATE TABLE users");
    q(&c, "DEFINE RELATION numbered ON users FROM docs.n");
    for sql in [
        "DROP TABLE docs",
        "DROP INDEX docs_n",
        "ALTER TABLE docs RENAME TO renamed",
        "DROP TABLE __romedb_catalog",
        "DROP TABLE sqlite_schema",
        "DROP TABLE source; DROP TABLE docs",
    ] {
        let x = m(&c, sql, DdlWorkLimits::default());
        assert!(x.outcome.is_err(), "{sql}");
        assert_eq!(x.work.row_mutations, 0);
    }
    for sql in [
        "DROP VIEW IF EXISTS temp.absent",
        "DROP TABLE temp.source",
        "SELECT 1",
        "DEFINE FIELD n ON docs TYPE integer",
    ] {
        assert!(
            c.ddl_metered(sql, &Parameters::new(), limits(), DdlWorkLimits::default())
                .is_none(),
            "{sql}"
        );
    }
    let x = c
        .ddl_metered(
            "DROP TABLE source",
            &Parameters::from([("$unused".into(), Value::Integer(1))]),
            limits(),
            DdlWorkLimits::default(),
        )
        .unwrap();
    assert!(x.outcome.is_err());
    assert_eq!(x.work.rows_read, 0);
    assert_eq!(
        c.execute("SELECT * FROM source", &Parameters::new())
            .unwrap()
            .rows
            .len(),
        5
    );
    assert_eq!(
        c.execute("SELECT * FROM docs", &Parameters::new())
            .unwrap()
            .rows
            .len(),
        5
    );
}

#[test]
fn ddl_respects_temp_shadowing_and_explicit_main_targets() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    seed(&c);
    q(&c, "CREATE TEMP TABLE source(n INTEGER)");
    q(&c, "INSERT INTO temp.source VALUES(91),(92)");
    q(&c, "CREATE INDEX temp.source_n ON source(n)");
    for sql in [
        "DROP TABLE source",
        "DROP INDEX source_n",
        "ALTER TABLE source ADD COLUMN extra INTEGER",
    ] {
        assert!(
            c.ddl_metered(sql, &Parameters::new(), limits(), DdlWorkLimits::default())
                .is_none(),
            "{sql}"
        );
    }
    let x = m(
        &c,
        "DROP TABLE main.source",
        DdlWorkLimits {
            max_rows_read: Some(5),
            max_row_mutations: Some(5),
            ..Default::default()
        },
    );
    x.outcome.unwrap();
    assert_eq!(x.work.rows_read, 5);
    assert_eq!(x.work.row_mutations, 5);
    assert_eq!(
        c.execute("SELECT * FROM temp.source", &Parameters::new())
            .unwrap()
            .rows
            .len(),
        2
    );
    assert!(
        c.execute("ATTACH ':memory:' AS other", &Parameters::new())
            .is_err(),
        "attachment support remains disabled on the RomeDB connection"
    );
    assert!(c
        .ddl_metered(
            "DROP TABLE other.remote",
            &Parameters::new(),
            limits(),
            DdlWorkLimits::default()
        )
        .is_none());
}

#[test]
fn drop_reserves_parent_mutations_before_foreign_key_cascades() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    for sql in ["PRAGMA foreign_keys=ON","CREATE TABLE parent(id INTEGER PRIMARY KEY)",
        "CREATE TABLE child(id INTEGER PRIMARY KEY,parent_id INTEGER REFERENCES parent(id) ON DELETE CASCADE)",
        "INSERT INTO parent VALUES(1),(2)","INSERT INTO child VALUES(1,1),(2,1),(3,2),(4,2)"] { q(&c,sql); }
    q(&c, "BEGIN");
    let failed = m(
        &c,
        "DROP TABLE parent",
        DdlWorkLimits {
            max_row_mutations: Some(5),
            ..Default::default()
        },
    );
    assert!(failed.outcome.is_err());
    assert!(failed.work.mutation_budget_exhausted);
    assert_eq!(
        failed.work.row_mutations, 4,
        "two parent rows reserved; fourth child exceeds the remaining three"
    );
    assert_eq!(c.transaction_state(), TransactionState::Active);
    assert_eq!(
        c.execute("SELECT * FROM parent", &Parameters::new())
            .unwrap()
            .rows
            .len(),
        2
    );
    assert_eq!(
        c.execute("SELECT * FROM child", &Parameters::new())
            .unwrap()
            .rows
            .len(),
        4
    );
    let done = m(
        &c,
        "DROP TABLE parent",
        DdlWorkLimits {
            max_row_mutations: Some(6),
            ..Default::default()
        },
    );
    done.outcome.unwrap();
    assert_eq!(done.work.row_mutations, 6);
    assert!(done.work.rows_read >= 2);
    assert!(c
        .execute("SELECT * FROM child", &Parameters::new())
        .unwrap()
        .rows
        .is_empty());
    q(&c, "ROLLBACK");
    assert_eq!(
        c.execute("SELECT * FROM parent", &Parameters::new())
            .unwrap()
            .rows
            .len(),
        2
    );
    assert_eq!(
        c.execute("SELECT * FROM child", &Parameters::new())
            .unwrap()
            .rows
            .len(),
        4
    );
}
