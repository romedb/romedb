use romedb::{Database, Document, Field, FieldType, Key, Parameters, Record, Value};
fn record(key: &str) -> Record {
    Record {
        table: "users".into(),
        key: Key::String(key.into()),
    }
}
fn field(path: &[&str], kind: FieldType, required: bool) -> Field {
    Field {
        path: path.iter().map(|p| (*p).into()).collect(),
        kind,
        required,
        nullable: false,
        check: None,
    }
}
fn query(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
#[test]
fn persistent_crud_validation_indexes_and_mixed_rollback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db");
    let path = path.to_str().expect("path");
    {
        let db = Database::open(path).expect("open");
        let c = db.connect().expect("connect");
        query(&c, "CREATE TABLE users;");
        c.define_field("users", field(&["name"], FieldType::String, true), false)
            .expect("field");
        c.create_index("users", "users_email", vec!["email".into()], true)
            .expect("index");
        query(&c, "INSERT INTO users {id: users:u1, name: 'Alice', email: 'a@x', profile: {city: 'Bangkok'}, tags: [1, [2], true]} RETURNING *;");
        assert_eq!(
            c.lookup_index("users", "users_email", &Value::String("a@x".into()))
                .expect("lookup")
                .len(),
            1
        );
        assert!(c
            .execute(
                "INSERT INTO users {id: users:bad, name: 1}",
                &Parameters::new()
            )
            .is_err());
        assert!(c.get(&record("bad")).expect("read").is_none());
        assert!(c
            .execute(
                "INSERT INTO users {id: users:u2, name: 'Duplicate', email: 'a@x'}",
                &Parameters::new()
            )
            .is_err());
        assert!(c.get(&record("u2")).expect("read").is_none());
        query(
            &c,
            "CREATE TABLE accounts (id INTEGER PRIMARY KEY, name TEXT)",
        );
        query(&c, "BEGIN");
        query(&c, "INSERT INTO accounts VALUES (1, 'pending')");
        query(&c, "UPDATE users:u1 {email: 'b@x'}");
        assert!(c
            .lookup_index("users", "users_email", &Value::String("a@x".into()))
            .expect("old key")
            .is_empty());
        query(&c, "ROLLBACK");
        assert!(query(&c, "SELECT * FROM accounts").rows.is_empty());
        assert_eq!(
            c.lookup_index("users", "users_email", &Value::String("a@x".into()))
                .expect("restored index")
                .len(),
            1
        );
        query(&c, "UPDATE users:u1 {email: 'c@x'} RETURNING *");
        query(
            &c,
            "INSERT INTO users {id: users:u2, name: 'Bob', email: 'b@x'}",
        );
        assert!(c
            .execute("UPDATE users:u2 {email: 'c@x'}", &Parameters::new())
            .is_err());
        assert_eq!(
            c.lookup_index("users", "users_email", &Value::String("b@x".into()))
                .expect("failed update rollback")
                .len(),
            1
        );
    }
    {
        let db = Database::open(path).expect("reopen");
        let c = db.connect().expect("connect");
        assert_eq!(
            c.get(&record("u1")).expect("read").expect("exists")["name"],
            Value::String("Alice".into())
        );
        assert_eq!(
            c.lookup_index("users", "users_email", &Value::String("c@x".into()))
                .expect("persisted index")
                .len(),
            1
        );
        query(&c, "DELETE FROM users:u1 RETURNING *");
        assert!(c
            .lookup_index("users", "users_email", &Value::String("c@x".into()))
            .expect("deleted index")
            .is_empty());
        assert!(c.get(&record("u1")).expect("deleted record").is_none());
    }
}
#[test]
fn typed_round_trips_and_definition_build_failure() {
    let db = Database::open(":memory:").expect("open");
    let c = db.connect().expect("connect");
    query(&c, "CREATE TABLE users");
    let doc = Document::from([
        ("id".into(), Value::Record(record("typed"))),
        ("integer".into(), Value::Integer(i64::MAX)),
        ("null".into(), Value::Null),
        ("binary".into(), Value::Binary(vec![0, 255])),
        ("boolean".into(), Value::Boolean(true)),
        (
            "object".into(),
            Value::Object(Document::from([(
                "type".into(),
                Value::String("Record".into()),
            )])),
        ),
    ]);
    let params = Parameters::from([("$doc".into(), Value::Object(doc.clone()))]);
    c.execute("INSERT INTO users DOCUMENT $doc", &params)
        .expect("bound doc");
    assert_eq!(c.get(&record("typed")).expect("read"), Some(doc));
    assert!(c
        .define_field(
            "users",
            field(&["integer"], FieldType::String, false),
            false
        )
        .is_err());
    c.define_field(
        "users",
        field(&["integer"], FieldType::Integer, false),
        false,
    )
    .expect("failed definition left no metadata");
    query(&c, "INSERT INTO users {id: users:1, email: 'same'}");
    query(&c, "INSERT INTO users {id: users:`1`, email: 'same'}");
    assert!(c
        .create_index("users", "email_idx", vec!["email".into()], true)
        .is_err());
    c.create_index("users", "email_idx", vec!["email".into()], false)
        .expect("failed build rollback");
    assert_eq!(
        c.lookup_index("users", "email_idx", &Value::String("same".into()))
            .expect("lookup")
            .len(),
        2
    );
    assert!(c
        .insert("users", Document::from([("id".into(), Value::Integer(1))]))
        .is_err());
}
#[test]
fn sql_boundaries_and_ordinary_tables() {
    let db = Database::open(":memory:").expect("open");
    let c = db.connect().expect("connect");
    query(&c, "CREATE TABLE users");
    for sql in [
        "INSERT INTO users (id) VALUES (1)",
        "DELETE FROM __romedb_catalog",
        "DELETE FROM [__romedb_catalog]",
        "DELETE FROM '__romedb_catalog'",
        "PRAGMA writable_schema=ON",
        "SELECT 1; DELETE FROM __romedb_catalog",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    query(
        &c,
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, name TEXT)",
    );
    query(&c, "CREATE TABLE IF NOT EXISTS accounts;");
    query(&c, "INSERT INTO accounts VALUES (1, 'Alice')");
    assert_eq!(
        query(&c, "SELECT id FROM accounts")
            .exactly_one()
            .expect("one"),
        vec![Value::Integer(1)]
    );
    query(&c, "CREATE TABLE copy AS SELECT * FROM accounts");
    assert_eq!(
        query(&c, "SELECT id FROM copy").rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn romeql_persistent_example_is_executable() {
    let db = Database::open(":memory:").expect("open");
    let c = db.connect().expect("connect");
    query(&c, "CREATE TABLE users");
    query(&c, "DEFINE FIELD name ON users TYPE string REQUIRED");
    query(&c, "DEFINE FIELD profile.city ON users TYPE string");
    query(&c, "CREATE UNIQUE INDEX users_name ON users (name)");
    query(&c, "CREATE INDEX users_city ON users (profile.city)");
    query(
        &c,
        "INSERT INTO users {name: 'Alice', profile: {city: 'Bangkok'}} RETURNING *",
    );
    assert_eq!(
        c.lookup_index("users", "users_city", &Value::String("Bangkok".into()))
            .expect("lookup")
            .len(),
        1
    );
    assert!(c
        .execute("INSERT INTO users {name: 'Alice'}", &Parameters::new())
        .is_err());
    query(&c, "CREATE TABLE relational (name TEXT)");
    query(&c, "CREATE INDEX relational_name ON relational (name)");
}

#[test]
fn abrupt_exit_child() {
    let Ok(path) = std::env::var("ROMEDB_CRASH_TEST_PATH") else {
        return;
    };
    let db = Database::open(&path).expect("open child");
    let c = db.connect().expect("connect child");
    query(&c, "CREATE TABLE users");
    query(&c, "CREATE UNIQUE INDEX users_name ON users (name)");
    query(
        &c,
        "INSERT INTO users {id: users:committed, name: 'durable'}",
    );
    query(&c, "BEGIN");
    query(&c, "UPDATE users:committed {name: 'uncommitted'}");
    query(&c, "INSERT INTO users {id: users:pending, name: 'pending'}");
    // No destructors, close, or application rollback run at process exit.
    std::process::exit(0);
}
#[test]
fn recovery_after_abrupt_process_exit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("crash.db");
    let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "abrupt_exit_child", "--nocapture"])
        .env("ROMEDB_CRASH_TEST_PATH", &path)
        .status()
        .expect("child process");
    assert!(status.success());
    let db = Database::open(path.to_str().expect("path")).expect("recover");
    let c = db.connect().expect("connect");
    assert!(c.get(&record("pending")).expect("read pending").is_none());
    assert_eq!(
        c.lookup_index("users", "users_name", &Value::String("durable".into()))
            .expect("committed index")
            .len(),
        1
    );
    assert!(c
        .lookup_index("users", "users_name", &Value::String("uncommitted".into()))
        .expect("uncommitted index")
        .is_empty());
}

#[test]
fn indexed_values_follow_numeric_null_and_record_identity() {
    let db = Database::open(":memory:").expect("open");
    let c = db.connect().expect("connect");
    query(&c, "CREATE TABLE users");
    query(&c, "CREATE UNIQUE INDEX scalar_key ON users (key)");
    query(&c, "INSERT INTO users {id: users:a, key: 1}");
    assert!(c
        .execute(
            "INSERT INTO users {id: users:b, key: 1.0}",
            &Parameters::new()
        )
        .is_err());
    assert!(c
        .execute(
            "INSERT INTO users {id: users:b, key: true}",
            &Parameters::new()
        )
        .is_err());
    query(&c, "INSERT INTO users {id: users:b, key: '1'}");
    query(&c, "INSERT INTO users {id: users:c, key: null}");
    query(&c, "INSERT INTO users {id: users:d}");
    query(&c, "INSERT INTO users {id: users:e, key: users:a}");
    assert_eq!(
        c.lookup_index("users", "scalar_key", &Value::Number(1.0))
            .expect("numeric equality")
            .len(),
        1
    );
    assert_eq!(
        c.lookup_index("users", "scalar_key", &Value::Record(record("a")))
            .expect("record identity")
            .len(),
        1
    );
    let forged = Value::Binary(
        b"FDB\x01{\"type\":\"Record\",\"value\":{\"table\":\"users\",\"key\":{\"String\":\"a\"}}}"
            .to_vec(),
    );
    c.insert(
        "users",
        Document::from([
            ("id".into(), Value::Record(record("binary"))),
            ("key".into(), forged.clone()),
        ]),
    )
    .expect("binary is distinct from record");
    assert_eq!(
        c.lookup_index("users", "scalar_key", &forged)
            .expect("binary lookup")
            .len(),
        1
    );
}

#[test]
fn ordinary_sql_literals_are_not_managed_object_references() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE users");
    query(&c, "CREATE TABLE ordinary(value TEXT)");
    query(
        &c,
        "INSERT INTO ordinary VALUES ('users'),('__romedb_catalog'),('writable_schema')",
    );
    assert_eq!(
        query(
            &c,
            "SELECT upper('users'), '__romedb_catalog', 'writable_schema'"
        )
        .rows[0][0],
        Value::String("USERS".into())
    );
    assert_eq!(
        query(&c, "SELECT value FROM ordinary WHERE value='users'").rows,
        vec![vec![Value::String("users".into())]]
    );
    query(
        &c,
        "UPDATE ordinary SET value='users' WHERE value='writable_schema'",
    );
    assert_eq!(
        query(&c, "DELETE FROM ordinary WHERE value='__romedb_catalog'").affected,
        1
    );
    assert_eq!(query(&c,"WITH c AS (SELECT 'users' AS value) SELECT value FROM c UNION ALL SELECT '__romedb_catalog'").rows.len(),2);
    assert_eq!(
        query(&c, "SELECT (SELECT '__romedb_catalog') AS nested").rows,
        vec![vec![Value::String("__romedb_catalog".into())]]
    );
    query(&c, "CREATE VIEW literal_view AS SELECT 'users' AS value");
    assert_eq!(
        query(&c, "SELECT * FROM literal_view").rows,
        vec![vec![Value::String("users".into())]]
    );
    query(
        &c,
        "CREATE TABLE literal_copy AS SELECT '__romedb_catalog' AS value",
    );
    assert_eq!(
        query(&c, "SELECT * FROM literal_copy").rows,
        vec![vec![Value::String("__romedb_catalog".into())]]
    );
    assert_eq!(
        query(&c, "SELECT value FROM (SELECT 'users' AS value) AS source").rows,
        vec![vec![Value::String("users".into())]]
    );
    for sql in [
        "SELECT * FROM '__romedb_catalog'",
        "SELECT (SELECT name FROM '__romedb_catalog')",
        "WITH c AS (SELECT * FROM '__romedb_catalog') SELECT * FROM c",
        "DELETE FROM '__romedb_catalog' WHERE name='users'",
        "PRAGMA 'writable_schema'=ON",
        "SELECT 'users'; DELETE FROM '__romedb_catalog'",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    query(&c, "INSERT INTO users {id:users:p1}");
    assert_eq!(query(&c, "SELECT * FROM users").rows.len(), 1);
}

#[test]
fn schema_and_upsert_literals_retain_values_and_protect_reference_names() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE users");
    query(&c,"CREATE TABLE ordinary(id INTEGER PRIMARY KEY, value TEXT DEFAULT 'users' CHECK(value IN ('users','__romedb_catalog')), CHECK(value <> 'writable_schema'))");
    query(&c, "INSERT INTO ordinary (id) VALUES (1)");
    assert_eq!(
        query(&c, "SELECT value FROM ordinary").rows,
        vec![vec![Value::String("users".into())]]
    );
    query(
        &c,
        "CREATE INDEX ordinary_value ON ordinary(value) WHERE value='users'",
    );
    query(
        &c,
        "ALTER TABLE ordinary ADD COLUMN label TEXT DEFAULT '__romedb_catalog'",
    );
    query(&c,"INSERT INTO ordinary (id,value) VALUES (1,'users') ON CONFLICT(id) DO UPDATE SET value='__romedb_catalog' WHERE ordinary.value='users'");
    assert_eq!(
        query(&c, "SELECT value,label FROM ordinary").rows,
        vec![vec![
            Value::String("__romedb_catalog".into()),
            Value::String("__romedb_catalog".into())
        ]]
    );
    assert!(c
        .execute(
            "UPDATE ordinary SET value='writable_schema'",
            &Parameters::new()
        )
        .is_err());
    for sql in [
        "CREATE TABLE bad(x REFERENCES '__romedb_catalog'(name))",
        "CREATE INDEX bad_index ON '__romedb_catalog'(name)",
        "CREATE TABLE bad(x TEXT CONSTRAINT '__romedb_bad' CHECK(x='users'))",
        "ALTER TABLE ordinary RENAME TO '__romedb_bad'",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    query(&c, "INSERT INTO users {id:users:p1}");
    assert_eq!(query(&c, "SELECT * FROM users").rows.len(), 1);
}

#[test]
fn ordinary_trigger_literals_persist_without_permitting_managed_references() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("triggers.db");
    let path = path.to_str().unwrap();
    {
        let db = Database::open(path).unwrap();
        let c = db.connect().unwrap();
        query(&c, "CREATE TABLE users");
        query(&c, "CREATE TABLE ordinary(label TEXT)");
        query(&c, "CREATE TABLE audit(value TEXT)");
        query(&c,"CREATE TRIGGER audit_insert AFTER INSERT ON ordinary WHEN new.label='users' BEGIN INSERT INTO audit VALUES ('__romedb_catalog'); UPDATE audit SET value='users' WHERE value='__romedb_catalog'; DELETE FROM audit WHERE value='writable_schema'; SELECT CASE WHEN new.label='users' THEN '__romedb_catalog' ELSE 'writable_schema' END; END");
        query(&c, "INSERT INTO ordinary VALUES ('users')");
        assert_eq!(
            query(&c, "SELECT * FROM audit").rows,
            vec![vec![Value::String("users".into())]]
        );
        query(&c, "BEGIN");
        query(&c, "INSERT INTO ordinary VALUES ('users')");
        query(&c, "ROLLBACK");
        assert_eq!(query(&c, "SELECT * FROM audit").rows.len(), 1);
        query(&c,"CREATE TRIGGER reject_insert BEFORE INSERT ON ordinary WHEN new.label='writable_schema' BEGIN SELECT RAISE(ABORT,'__romedb_catalog'); END");
        let rejected = c
            .execute(
                "INSERT INTO ordinary VALUES ('writable_schema')",
                &Parameters::new(),
            )
            .unwrap_err();
        assert!(rejected.to_string().contains("__romedb_catalog"));
        assert_eq!(query(&c, "SELECT * FROM ordinary").rows.len(), 1);
        for body in [
            "INSERT INTO '__romedb_catalog' (name) VALUES ('users')",
            "UPDATE '__romedb_catalog' SET name='users'",
            "DELETE FROM '__romedb_catalog'",
            "SELECT * FROM '__romedb_catalog'",
            "SELECT * FROM users",
        ] {
            let error = c
                .execute(
                    &format!("CREATE TRIGGER forbidden AFTER INSERT ON ordinary BEGIN {body}; END"),
                    &Parameters::new(),
                )
                .unwrap_err();
            assert_eq!(error.code(), "FDB_UNSUPPORTED", "{body}");
        }
        assert_eq!(c.execute("CREATE TRIGGER forbidden AFTER INSERT ON '__romedb_catalog' BEGIN SELECT 'users'; END",&Parameters::new()).unwrap_err().code(),"FDB_UNSUPPORTED");
    }
    let db = Database::open(path).unwrap();
    let c = db.connect().unwrap();
    query(&c, "INSERT INTO ordinary VALUES ('users')");
    assert_eq!(
        query(&c, "SELECT * FROM audit").rows,
        vec![
            vec![Value::String("users".into())],
            vec![Value::String("users".into())]
        ]
    );
    query(&c, "INSERT INTO users {id:users:p1}");
    assert_eq!(query(&c, "SELECT * FROM users").rows.len(), 1);
}

#[test]
fn checkpointed_offline_backup_restores_schema_values_indexes_and_history() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.db");
    let backup = dir.path().join("backup.db");
    let restored = dir.path().join("restore");
    std::fs::create_dir(&restored).unwrap();
    let restored = restored.join("restored.db");
    let migration = romedb::Migration {
        version: 1,
        name: "initial".into(),
        sql: "CREATE TABLE users; DEFINE FIELD name ON users TYPE string REQUIRED; DEFINE FIELD score ON users TYPE integer CHECK(score>0); DEFINE FIELD embedding ON users TYPE vector<3>; CREATE UNIQUE INDEX users_name ON users(name); CREATE INDEX users_city ON users(profile.city); CREATE TABLE audit(value TEXT); CREATE VIEW audit_view AS SELECT value FROM audit;".into(),
    };
    let document = Document::from([
        ("id".into(), Value::Record(record("saved"))),
        ("name".into(), Value::String("Alice".into())),
        ("score".into(), Value::Integer(7)),
        ("large".into(), Value::Integer(i64::MAX)),
        (
            "fraction".into(),
            Value::Number(f64::from_bits(0x3fd5555555555555)),
        ),
        ("binary".into(), Value::Binary(vec![0, 255, 128])),
        ("reference".into(), Value::Record(record("missing"))),
        (
            "embedding".into(),
            Value::vector64(&[0.1, 0.2, 0.3]).unwrap(),
        ),
        (
            "array".into(),
            Value::Array(vec![Value::Null, Value::Boolean(true)]),
        ),
        (
            "profile".into(),
            Value::Object(Document::from([(
                "city".into(),
                Value::String("Bangkok".into()),
            )])),
        ),
    ]);
    let info;
    {
        let db = Database::open(source.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        c.migrate(std::slice::from_ref(&migration)).unwrap();
        c.insert("users", document.clone()).unwrap();
        query(&c, "INSERT INTO audit VALUES ('saved')");
        query(&c, "BEGIN");
        query(&c, "UPDATE users:saved {name:'rolled back'}");
        query(&c, "INSERT INTO audit VALUES ('rolled back')");
        query(&c, "ROLLBACK");
        info = query(&c, "INFO FOR TABLE users").rows;
        assert_eq!(
            query(&c, "PRAGMA wal_checkpoint(TRUNCATE)").rows,
            vec![vec![
                Value::Integer(0),
                Value::Integer(0),
                Value::Integer(0)
            ]]
        );
        let wal = source.with_file_name("source.db-wal");
        assert_eq!(std::fs::metadata(wal).unwrap().len(), 0);
    }
    // All source handles are closed and this test exclusively owns the files.
    // A main-file copy is only sufficient after the successful checkpoint.
    std::fs::copy(&source, &backup).unwrap();
    let snapshot_bytes = std::fs::read(&backup).unwrap();
    {
        let db = Database::open(source.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        query(
            &c,
            "UPDATE users:saved {name:'later',profile:{city:'Osaka'}}",
        );
        query(&c, "INSERT INTO audit VALUES ('later')");
        query(&c, "DROP INDEX users_city");
    }
    // Restore into a fresh directory, without any old WAL or SHM sidecars.
    std::fs::copy(&backup, &restored).unwrap();
    {
        let db = Database::open(restored.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        assert_eq!(
            query(&c, "PRAGMA integrity_check").rows,
            vec![vec![Value::String("ok".into())]]
        );
        let saved = c.get(&record("saved")).unwrap().unwrap();
        assert_eq!(saved, document);
        let Value::Number(fraction) = saved["fraction"] else {
            panic!("number lost its type")
        };
        assert_eq!(fraction.to_bits(), 0x3fd5555555555555);
        assert_eq!(query(&c, "INFO FOR TABLE users").rows, info);
        assert_eq!(
            query(&c, "SELECT * FROM audit_view").rows,
            vec![vec![Value::String("saved".into())]]
        );
        let report = c.migrate(std::slice::from_ref(&migration)).unwrap();
        assert_eq!(report.already_applied, 1);
        assert!(report.applied.is_empty());
        let mut changed = migration.clone();
        changed.sql.push(' ');
        assert_eq!(c.migrate(&[changed]).unwrap_err().code(), "FDB_VALIDATION");
        for sql in [
            "UPDATE users:saved {score:0}",
            "INSERT INTO users {id:users:duplicate,name:'Alice'}",
            "INSERT INTO users {id:users:invalid,score:1}",
        ] {
            assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
        }
        assert_eq!(
            c.lookup_index("users", "users_city", &Value::String("Bangkok".into()))
                .unwrap(),
            vec![document.clone()]
        );
        query(&c, "BEGIN");
        query(&c, "UPDATE users:saved {profile:{city:'Paris'}}");
        assert!(c
            .lookup_index("users", "users_city", &Value::String("Bangkok".into()))
            .unwrap()
            .is_empty());
        query(&c, "DELETE FROM users:saved");
        assert!(c
            .lookup_index("users", "users_city", &Value::String("Paris".into()))
            .unwrap()
            .is_empty());
        query(&c, "ROLLBACK");
        assert_eq!(
            c.lookup_index("users", "users_city", &Value::String("Bangkok".into()))
                .unwrap(),
            vec![document.clone()]
        );
        query(&c, "INSERT INTO users {id:users:new,name:'Bob',score:2}");
    }
    let db = Database::open(restored.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(c.get(&record("saved")).unwrap(), Some(document));
    assert!(c.get(&record("new")).unwrap().is_some());
    assert_eq!(query(&c, "SELECT * FROM audit_view").rows.len(), 1);
    // Restoring and writing the working copy never changes the backup artifact.
    assert_eq!(std::fs::read(backup).unwrap(), snapshot_bytes);
}
