use romedb::{Database, Parameters, ResultLimits, SchemaWorkLimits, TransactionState, Value};
fn results() -> ResultLimits {
    ResultLimits {
        max_rows: 0,
        max_payload_bytes: 65536,
    }
}
fn checked(c: &romedb::Connection, sql: &str) -> romedb::MeteredSchema {
    c.schema_metered(
        sql,
        &Parameters::new(),
        results(),
        SchemaWorkLimits::default(),
    )
    .unwrap()
}
fn seed(c: &romedb::Connection) {
    let p = Parameters::new();
    c.execute("CREATE TABLE docs", &p).unwrap();
    for n in 1..=5 {
        c.execute(&format!("INSERT INTO docs {{id:docs:d{n},n:{n}}}"), &p)
            .unwrap();
    }
}
#[test]
fn field_definitions_charge_existing_documents_without_logical_writes() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        ":memory:".to_owned(),
        dir.path().join("schema.db").to_str().unwrap().to_owned(),
    ] {
        let c = Database::open(&path).unwrap().connect().unwrap();
        seed(&c);
        for sql in [
            "DEFINE FIELD n ON docs TYPE integer REQUIRED CHECK (n>0)",
            "DEFINE FIELD OVERWRITE n ON docs TYPE number REQUIRED CHECK (n>=1)",
        ] {
            let m = checked(&c, sql);
            m.outcome.unwrap_or_else(|e| panic!("{sql}: {e}"));
            assert_eq!(m.work.rows_read, 5);
            assert_eq!(m.work.row_mutations, 0);
            assert_eq!(m.work.schema_rows_read, 0);
            assert!(m.work.vm_steps > 0);
        }
        let m = checked(&c, "REMOVE FIELD n ON docs");
        m.outcome.unwrap();
        assert_eq!(m.work.rows_read, 0);
        assert_eq!(m.work.row_mutations, 0);
        // Removal changes the declaration only, retaining all document values.
        let rows = c
            .execute("SELECT n FROM docs ORDER BY n", &Parameters::new())
            .unwrap();
        assert_eq!(rows.rows.len(), 5);
        assert_eq!(rows.rows[0][0], Value::Integer(1));
        if path != ":memory:" {
            drop(c);
            let reopened = Database::open(&path).unwrap().connect().unwrap();
            assert_eq!(
                reopened
                    .execute("SELECT n FROM docs", &Parameters::new())
                    .unwrap()
                    .rows
                    .len(),
                5
            );
            checked(&reopened, "DEFINE FIELD n ON docs TYPE integer")
                .outcome
                .unwrap();
        }
    }
}
#[test]
fn field_failures_retain_reads_and_restore_prior_definition_and_caller_work() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let p = Parameters::new();
    seed(&c);
    checked(&c, "DEFINE FIELD n ON docs TYPE integer")
        .outcome
        .unwrap();
    c.execute("BEGIN", &p).unwrap();
    c.execute("INSERT INTO docs {id:docs:prior,n:6}", &p)
        .unwrap();
    for (limits, read_stop) in [
        (
            SchemaWorkLimits {
                max_rows_read: Some(2),
                ..Default::default()
            },
            true,
        ),
        (
            SchemaWorkLimits {
                max_vm_steps: Some(0),
                ..Default::default()
            },
            false,
        ),
    ] {
        let m = c
            .schema_metered(
                "DEFINE FIELD OVERWRITE n ON docs TYPE number",
                &p,
                results(),
                limits,
            )
            .unwrap();
        assert!(m.outcome.is_err());
        assert_eq!(m.work.row_mutations, 0);
        if read_stop {
            assert!(m.work.read_budget_exhausted);
            assert_eq!(m.work.rows_read, 3);
        } else {
            assert!(m.work.vm_budget_exhausted);
        }
        assert_eq!(c.transaction_state(), TransactionState::Active);
        assert_eq!(c.execute("SELECT n FROM docs", &p).unwrap().rows.len(), 6);
        // Failed replacement must not relax the old integer constraint.
        assert!(c
            .execute("INSERT INTO docs {id:docs:bad,n:1.5}", &p)
            .is_err());
    }
    let m = checked(&c, "DEFINE FIELD OVERWRITE n ON docs TYPE string");
    assert!(m.outcome.is_err());
    assert_eq!(
        m.work.rows_read, 6,
        "validation currently materializes its source scan"
    );
    assert_eq!(m.work.row_mutations, 0);
    let m = checked(&c, "DEFINE FIELD extra ON docs TYPE integer REQUIRED");
    assert!(m.outcome.is_err());
    assert_eq!(m.work.rows_read, 6);
    c.execute("COMMIT", &p).unwrap();
    c.execute("INSERT INTO docs {id:docs:after,n:7}", &p)
        .unwrap();
    assert_eq!(
        checked(&c, "DEFINE FIELD OVERWRITE n ON docs TYPE number")
            .work
            .rows_read,
        7
    );
}
#[test]
fn relation_and_function_declarations_are_metadata_only_and_transactional() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let p = Parameters::new();
    for sql in [
        "CREATE TABLE users",
        "CREATE TABLE posts",
        "CREATE INDEX posts_author ON posts(author)",
        "INSERT INTO posts {id:posts:a,author:users:u1}",
    ] {
        c.execute(sql, &p).unwrap();
    }
    c.execute("BEGIN", &p).unwrap();
    for sql in [
        "DEFINE RELATION authored ON users FROM posts.author",
        "CREATE FUNCTION app::answer() RETURNS integer LANGUAGE JAVASCRIPT AS 'return 42n'",
        "CREATE OR REPLACE FUNCTION app::answer() RETURNS integer LANGUAGE JAVASCRIPT AS 'return 43n'",
    ] {
        let m = checked(&c, sql); m.outcome.unwrap_or_else(|e| panic!("{sql}: {e}"));
        assert_eq!(m.work.rows_read, 0); assert_eq!(m.work.row_mutations, 0);
    }
    assert_eq!(
        c.execute("SELECT app::answer()", &p).unwrap().rows[0][0],
        Value::Integer(43)
    );
    c.execute("ROLLBACK", &p).unwrap();
    assert!(c.execute("INFO FOR RELATION authored", &p).is_err());
    assert!(c.execute("SELECT app::answer()", &p).is_err());
    checked(&c, "DEFINE RELATION authored ON users FROM posts.author")
        .outcome
        .unwrap();
    checked(
        &c,
        "CREATE FUNCTION app::answer() RETURNS integer LANGUAGE JAVASCRIPT AS 'return 42n'",
    )
    .outcome
    .unwrap();
    assert!(checked(&c, "CREATE OR REPLACE FUNCTION app::answer() RETURNS integer LANGUAGE JAVASCRIPT AS 'return }'").outcome.is_err());
    assert_eq!(
        c.execute("SELECT app::answer()", &p).unwrap().rows[0][0],
        Value::Integer(42)
    );
    assert!(
        checked(&c, "DEFINE RELATION authored ON users FROM posts.author")
            .outcome
            .is_err()
    );
    for sql in [
        "DROP RELATION authored",
        "DROP FUNCTION app::answer",
        "DROP RELATION IF EXISTS authored",
        "DROP FUNCTION IF EXISTS app::answer",
    ] {
        let m = checked(&c, sql);
        m.outcome.unwrap();
        assert_eq!(m.work.rows_read, 0);
        assert_eq!(m.work.row_mutations, 0);
    }
    assert_eq!(c.execute("SELECT * FROM posts", &p).unwrap().rows.len(), 1);
    c.execute("INFO FOR INDEX posts_author", &p).unwrap();
}
#[test]
fn logical_schema_rejects_bindings_and_does_not_execute_other_families() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    seed(&c);
    let p = Parameters::new();
    for sql in [
        "DROP TABLE docs",
        "ALTER TABLE docs RENAME TO renamed",
        "CREATE TABLE other",
        "DELETE FROM docs",
        "INFO FOR DB",
        "SELECT 1",
    ] {
        assert!(
            c.schema_metered(sql, &p, results(), SchemaWorkLimits::default())
                .is_none(),
            "{sql}"
        );
    }
    let p = Parameters::from([("$unused".into(), Value::Integer(1))]);
    for sql in [
        "DEFINE FIELD n ON docs TYPE integer",
        "REMOVE FIELD n ON docs",
        "CREATE FUNCTION app::x() RETURNS integer LANGUAGE JAVASCRIPT AS 'return 1'",
    ] {
        let m = c
            .schema_metered(sql, &p, results(), SchemaWorkLimits::default())
            .unwrap();
        assert!(m.outcome.is_err());
        assert_eq!(m.work.rows_read, 0);
    }
    for sql in ["DEFINE FIELD", "REMOVE FIELD n ON docs; DELETE FROM docs"] {
        assert!(checked(&c, sql).outcome.is_err());
    }
    checked(&c, "DEFINE FIELD n ON docs TYPE integer")
        .outcome
        .unwrap();
    assert_eq!(
        c.execute("SELECT n FROM docs", &Parameters::new())
            .unwrap()
            .rows
            .len(),
        5
    );
}
