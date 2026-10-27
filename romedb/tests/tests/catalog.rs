use romedb::{Database, Key, Parameters, Record, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
fn setup() -> (Database, romedb::Connection) {
    let db = Database::open(":memory:").expect("open");
    let c = db.connect().expect("connect");
    q(&c, "CREATE TABLE users");
    q(&c, "DEFINE FIELD name ON users TYPE string REQUIRED");
    q(&c, "CREATE UNIQUE INDEX users_name ON users (name)");
    (db, c)
}
#[test]
fn caller_storage_preserves_catalog_indexes_and_rollback_across_reopen() {
    // A file-shaped path with a memory backend ensures every open, including
    // catalog initialization and WAL access, uses the supplied storage.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("custom-storage.db");
    let io = std::sync::Arc::new(turso_core::MemoryIO::new());
    {
        let db = Database::open_with_io(path.to_str().unwrap(), io.clone()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE users");
        q(&c, "DEFINE FIELD name ON users TYPE string REQUIRED");
        q(&c, "CREATE UNIQUE INDEX users_name ON users(name)");
        q(&c, "INSERT INTO users {id:users:u1,name:'Alice'}");
        q(&c, "BEGIN");
        q(&c, "UPDATE users:u1 {name:'Rolled back'}");
        q(&c, "ROLLBACK");
        q(&c, "PRAGMA wal_checkpoint(TRUNCATE)");
    }
    assert!(!path.exists());
    let db = Database::open_with_io(path.to_str().unwrap(), io).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        q(&c, "SELECT name FROM users WHERE name='Alice'").rows,
        vec![vec![Value::String("Alice".into())]]
    );
    assert_eq!(
        c.execute(
            "INSERT INTO users {id:users:u2,name:'Alice'}",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_CONSTRAINT"
    );
    assert_eq!(
        c.execute("INSERT INTO users {id:users:u2}", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    let audit = c
        .check_collection_integrity("users", Default::default())
        .unwrap();
    assert_eq!(
        (audit.documents, audit.indexes, audit.index_entries),
        (1, 1, 1)
    );
}
#[test]
fn upsert_inserts_or_shallow_patches_only_its_id() {
    let (_db, c) = setup();
    q(
        &c,
        "UPSERT users:u1 {name: 'Alice', profile: {city: 'Bangkok', zip: 1}} RETURNING *",
    );
    q(
        &c,
        "UPSERT users {id: users:u1, profile: {city: 'Berlin'}} RETURNING *",
    );
    let row = q(&c, "SELECT * FROM users").exactly_one().expect("one");
    let Value::Object(doc) = &row[0] else {
        panic!("document");
    };
    assert_eq!(doc["name"], Value::String("Alice".into()));
    let Value::Object(profile) = &doc["profile"] else {
        panic!("profile");
    };
    assert!(!profile.contains_key("zip"));
    q(&c, "UPSERT users:u2 {name: 'Bob'}");
    for sql in [
        "UPSERT users {name: 'Missing id'}",
        "UPSERT users:u1 {id: users:u1, name:'wrong source'}",
        "UPSERT users:u3 {profile:{}}",
        "UPSERT users:u2 {name:'Alice'}",
        "UPSERT users {id: other:u1,name:'wrong table'}",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert_eq!(
        q(&c, "SELECT name FROM users ORDER BY name").rows,
        vec![
            vec![Value::String("Alice".into())],
            vec![Value::String("Bob".into())]
        ]
    );
    q(&c, "BEGIN");
    q(&c, "UPSERT users:u1 {name:'Changed'}");
    q(&c, "UPSERT users:u4 {name:'New'}");
    q(&c, "ROLLBACK");
    assert_eq!(
        c.lookup_index("users", "users_name", &Value::String("Alice".into()))
            .expect("restored index")
            .len(),
        1
    );
    assert!(c
        .get(&Record {
            table: "users".into(),
            key: Key::String("u4".into())
        })
        .expect("rolled back id")
        .is_none());
}
#[test]
fn index_drop_removes_metadata_and_rollback_restores_uniqueness() {
    let (_db, c) = setup();
    q(&c, "INSERT INTO users {id:users:u1,name:'Alice'}");
    q(&c, "BEGIN");
    q(&c, "DROP INDEX users_name");
    assert!(c
        .lookup_index("users", "users_name", &Value::String("Alice".into()))
        .is_err());
    q(&c, "INSERT INTO users {id:users:u2,name:'Alice'}");
    q(&c, "ROLLBACK");
    assert!(c
        .execute(
            "INSERT INTO users {id:users:u2,name:'Alice'}",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(
        c.lookup_index("users", "users_name", &Value::String("Alice".into()))
            .expect("restored metadata")
            .len(),
        1
    );
    q(&c, "DROP INDEX users_name");
    q(&c, "DROP INDEX IF EXISTS users_name");
    q(&c, "CREATE INDEX users_name ON users (name)");
    q(&c, "INSERT INTO users {id:users:u2,name:'Alice'}");
    assert_eq!(
        c.lookup_index("users", "users_name", &Value::String("Alice".into()))
            .expect("rebuilt index")
            .len(),
        2
    );
}
#[test]
fn field_removal_is_metadata_only_and_incompatible_definitions_fail() {
    let (_db, c) = setup();
    assert!(c
        .execute(
            "DEFINE FIELD OVERWRITE name ON users TYPE object",
            &Parameters::new()
        )
        .is_err());
    q(&c, "INSERT INTO users {id:users:u1,name:'Alice'}");
    q(&c, "REMOVE FIELD name ON users");
    assert_eq!(
        q(&c, "SELECT name FROM users").rows,
        vec![vec![Value::String("Alice".into())]]
    );
    assert!(c
        .execute(
            "INSERT INTO users {id:users:u2,name:'Alice'}",
            &Parameters::new()
        )
        .is_err());
    q(&c, "DEFINE FIELD name ON users TYPE string");
    q(&c, "DEFINE FIELD profile ON users TYPE object");
    assert!(c
        .execute(
            "CREATE INDEX object_idx ON users (profile)",
            &Parameters::new()
        )
        .is_err());
    q(&c, "CREATE INDEX city_idx ON users (profile.city)");
    assert!(c
        .execute(
            "DEFINE FIELD OVERWRITE profile ON users TYPE string",
            &Parameters::new()
        )
        .is_err());
    q(&c, "DROP INDEX city_idx");
    q(&c, "DEFINE FIELD OVERWRITE profile ON users TYPE string");
}
#[test]
fn collection_drop_is_atomic_and_links_remain_weak() {
    let (_db, c) = setup();
    q(&c, "UPSERT users:u1 {name:'Alice'}");
    q(&c, "CREATE TABLE posts");
    q(&c, "INSERT INTO posts {id:posts:p1,author:users:u1}");
    q(&c, "BEGIN");
    q(&c, "DROP TABLE users");
    q(&c, "ROLLBACK");
    assert_eq!(q(&c, "SELECT * FROM users").rows.len(), 1);
    assert_eq!(
        c.lookup_index("users", "users_name", &Value::String("Alice".into()))
            .expect("restored index")
            .len(),
        1
    );
    q(&c, "DROP TABLE users");
    q(&c, "DROP TABLE IF EXISTS users");
    assert_eq!(q(&c, "SELECT * FROM posts").rows.len(), 1);
    q(&c, "CREATE TABLE users");
    q(&c, "CREATE UNIQUE INDEX users_name ON users (name)");
    q(&c, "INSERT INTO users {id:users:u2}");
    assert_eq!(q(&c, "SELECT * FROM users").rows.len(), 1);
}
#[test]
fn info_is_logical_and_if_not_exists_never_converts_models() {
    let (_db, c) = setup();
    q(
        &c,
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, name TEXT)",
    );
    q(&c, "CREATE INDEX accounts_name ON accounts (name)");
    q(
        &c,
        "CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY)",
    );
    q(&c, "CREATE TABLE IF NOT EXISTS accounts");
    q(
        &c,
        "CREATE INDEX IF NOT EXISTS users_name ON users (different)",
    );
    let info = q(&c, "INFO FOR TABLE users").exactly_one().expect("info");
    assert!(!format!("{info:?}").contains("__romedb_"));
    let Value::Object(info) = &info[0] else {
        panic!("info object");
    };
    assert_eq!(info["model"], Value::String("document".into()));
    let index = q(&c, "INFO FOR INDEX users_name")
        .exactly_one()
        .expect("index");
    let Value::Object(index) = &index[0] else {
        panic!("index object");
    };
    assert_eq!(
        index["path"],
        Value::Array(vec![Value::String("name".into())])
    );
    for sql in [
        "INFO FOR DB",
        "INFO FOR TABLE accounts",
        "INFO FOR INDEX accounts_name",
    ] {
        assert!(!format!("{:?}", q(&c, sql)).contains("__romedb_"));
    }
    q(&c, "DROP INDEX accounts_name");
    q(&c, "DROP TABLE accounts");
    assert!(c
        .execute("INFO FOR TABLE accounts", &Parameters::new())
        .is_err());
    assert!(c
        .execute("CREATE INDEX users ON users (name)", &Parameters::new())
        .is_err());
}

#[test]
fn lifecycle_persists_across_reopen_and_rebuild() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let path = temp.path().join("catalog.db");
    let path = path.to_str().expect("path");
    {
        let db = Database::open(path).expect("open");
        let c = db.connect().expect("connect");
        q(&c, "CREATE TABLE users");
        q(&c, "DEFINE FIELD name ON users TYPE string REQUIRED");
        q(&c, "CREATE UNIQUE INDEX users_name ON users (name)");
        q(&c, "UPSERT users:u1 {name:'Alice'}");
        q(&c, "REMOVE FIELD name ON users");
        q(&c, "DROP INDEX users_name");
    }
    {
        let db = Database::open(path).expect("reopen");
        let c = db.connect().expect("connect");
        q(&c, "UPSERT users:u2 {name:'Alice'}");
        q(&c, "UPSERT users:u3 {}");
        assert!(c
            .execute(
                "CREATE UNIQUE INDEX users_name ON users (name)",
                &Parameters::new()
            )
            .is_err());
        q(&c, "CREATE INDEX users_name ON users (name)");
        assert_eq!(
            c.lookup_index("users", "users_name", &Value::String("Alice".into()))
                .expect("rebuilt index")
                .len(),
            2
        );
        q(&c, "DROP TABLE users");
    }
    let db = Database::open(path).expect("reopen drop");
    let c = db.connect().expect("connect");
    assert!(c
        .execute("INFO FOR TABLE users", &Parameters::new())
        .is_err());
    q(&c, "CREATE TABLE users");
    q(&c, "CREATE UNIQUE INDEX users_name ON users (name)");
}
#[test]
fn unsupported_catalog_version_is_rejected() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let path = temp.path().join("version.db");
    let path = path.to_str().expect("path");
    let db = Database::open(path).expect("open");
    let c = db.connect().expect("connect");
    q(&c, "CREATE TABLE users");
    let engine = turso_core::Database::open_file(
        turso_core::Database::io_for_path(path).expect("io"),
        path,
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .expect("raw engine");
    let raw = engine.connect().expect("raw connection");
    raw.execute("UPDATE __romedb_catalog SET metadata=json_set(metadata,'$.version',99)")
        .expect("future version fixture");
    assert!(c
        .execute("INFO FOR TABLE users", &Parameters::new())
        .expect_err("future version")
        .to_string()
        .contains("metadata version 99"));
    assert!(c.execute("INFO FOR DB", &Parameters::new()).is_err());
    assert!(c.execute("DROP TABLE users", &Parameters::new()).is_err());
    raw.execute("UPDATE __romedb_catalog SET metadata=json_remove(metadata,'$.version')")
        .expect("legacy prototype fixture");
    q(&c, "UPSERT users:u1 {name:'legacy'}");
    assert_eq!(
        q(&c, "SELECT name FROM users").rows,
        vec![vec![Value::String("legacy".into())]]
    );
}

#[test]
fn separate_connections_keep_document_and_index_snapshots_consistent() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("snapshots.db");
    let path = path.to_str().unwrap();
    {
        let db = Database::open(path).unwrap();
        let writer = db.connect().unwrap();
        let reader = db.connect().unwrap();
        q(&writer, "CREATE TABLE users");
        q(&writer, "CREATE UNIQUE INDEX users_name ON users(name)");
        q(&writer, "INSERT INTO users {id:users:u1,name:'Original'}");
        q(&writer, "BEGIN");
        q(&writer, "UPDATE users:u1 {name:'Pending'}");
        assert_eq!(
            q(&reader, "SELECT name FROM users").rows,
            vec![vec![Value::String("Original".into())]]
        );
        assert_eq!(
            reader
                .lookup_index("users", "users_name", &Value::String("Original".into()))
                .unwrap()
                .len(),
            1
        );
        assert!(reader
            .lookup_index("users", "users_name", &Value::String("Pending".into()))
            .unwrap()
            .is_empty());
        q(&writer, "ROLLBACK");
        q(&reader, "BEGIN");
        q(&reader, "SELECT * FROM users");
        q(&writer, "UPDATE users:u1 {name:'Committed'}");
        assert_eq!(
            q(&reader, "SELECT name FROM users").rows,
            vec![vec![Value::String("Original".into())]]
        );
        assert_eq!(
            reader
                .lookup_index("users", "users_name", &Value::String("Original".into()))
                .unwrap()
                .len(),
            1
        );
        assert!(reader
            .lookup_index("users", "users_name", &Value::String("Committed".into()))
            .unwrap()
            .is_empty());
        let error = reader
            .execute("UPDATE users:u1 {name:'Stale'}", &Parameters::new())
            .unwrap_err();
        assert!(
            matches!(
                error,
                romedb::Error::Engine(
                    turso_core::LimboError::Busy | turso_core::LimboError::BusySnapshot
                )
            ),
            "{error}"
        );
        assert_eq!(error.code(), "FDB_BUSY_SNAPSHOT");
        if reader.transaction_state() == romedb::TransactionState::Active {
            q(&reader, "ROLLBACK");
        }
        assert_eq!(
            q(&reader, "SELECT name FROM users").rows,
            vec![vec![Value::String("Committed".into())]]
        );
        assert_eq!(
            reader
                .lookup_index("users", "users_name", &Value::String("Committed".into()))
                .unwrap()
                .len(),
            1
        );
        assert!(reader
            .lookup_index("users", "users_name", &Value::String("Stale".into()))
            .unwrap()
            .is_empty());
        q(&reader, "UPDATE users:u1 {name:'Retry'}");
    }
    let db = Database::open(path).unwrap();
    let reader = db.connect().unwrap();
    assert_eq!(
        q(&reader, "SELECT name FROM users").rows,
        vec![vec![Value::String("Retry".into())]]
    );
    assert_eq!(
        reader
            .lookup_index("users", "users_name", &Value::String("Retry".into()))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn contending_index_builds_leave_no_partial_catalog_or_storage() {
    let directory = tempfile::tempdir().unwrap();
    for finish in ["COMMIT", "ROLLBACK"] {
        let path = directory.path().join(finish);
        let path = path.to_str().unwrap();
        {
            let db = Database::open(path).unwrap();
            let a = db.connect().unwrap();
            let b = db.connect().unwrap();
            q(&a, "CREATE TABLE users");
            q(&a, "INSERT INTO users {id:users:u1,name:'Alice'}");
            q(&a, "BEGIN");
            q(&a, "CREATE UNIQUE INDEX users_name ON users(name)");
            let error = b
                .execute(
                    "CREATE UNIQUE INDEX users_name ON users(name)",
                    &Parameters::new(),
                )
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    romedb::Error::Engine(
                        turso_core::LimboError::Busy | turso_core::LimboError::BusySnapshot
                    )
                ),
                "{finish}: {error}"
            );
            assert_eq!(error.code(), "FDB_BUSY");
            assert_eq!(b.transaction_state(), romedb::TransactionState::Autocommit);
            q(&a, finish);
            q(
                &b,
                "CREATE UNIQUE INDEX IF NOT EXISTS users_name ON users(name)",
            );
            assert_eq!(
                b.lookup_index("users", "users_name", &Value::String("Alice".into()))
                    .unwrap()
                    .len(),
                1
            );
            assert!(b
                .execute(
                    "INSERT INTO users {id:users:u2,name:'Alice'}",
                    &Parameters::new()
                )
                .is_err());
            assert_eq!(q(&a, "SELECT * FROM users").rows.len(), 1);
        }
        let db = Database::open(path).unwrap();
        let c = db.connect().unwrap();
        assert_eq!(
            c.lookup_index("users", "users_name", &Value::String("Alice".into()))
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn relational_info_reports_views_and_native_index_details_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inspection.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c,"CREATE TABLE ordinary(id INTEGER PRIMARY KEY,label TEXT NOT NULL UNIQUE DEFAULT 'new')");
        q(
            &c,
            "CREATE INDEX ordinary_expr ON ordinary(lower(label) DESC) WHERE label IS NOT NULL",
        );
        q(&c, "CREATE VIEW visible AS SELECT id,label FROM ordinary");
        q(&c, "CREATE TABLE docs");
        q(&c, "CREATE INDEX docs_label ON docs(label)");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    let info = |sql: &str| {
        let Value::Object(info) = q(&c, sql).rows.remove(0).remove(0) else {
            panic!("info object")
        };
        info
    };
    let table = info("INFO FOR TABLE ordinary");
    assert_eq!(table["kind"], Value::String("table".into()));
    let Value::Array(columns) = &table["columns"] else {
        panic!("columns")
    };
    let Value::Object(label) = &columns[1] else {
        panic!("column")
    };
    assert_eq!(label["name"], Value::String("label".into()));
    assert_eq!(label["notnull"], Value::Integer(1));
    assert_eq!(label["hidden"], Value::Integer(0));
    assert_eq!(label["dflt_value"], Value::String("'new'".into()));
    let Value::Array(indexes) = &table["indexes"] else {
        panic!("indexes")
    };
    assert_eq!(indexes.len(), 2);
    assert!(indexes.iter().any(|index|matches!(index,Value::Object(i) if i["origin"]==Value::String("u".into()) && i["unique"]==Value::Integer(1))));
    assert!(indexes.iter().any(|index|matches!(index,Value::Object(i) if i["name"]==Value::String("ordinary_expr".into()) && i["partial"]==Value::Integer(1))));
    let index = info("INFO FOR INDEX ordinary_expr");
    let Value::Array(columns) = &index["columns"] else {
        panic!("index columns")
    };
    let Value::Object(key) = &columns[0] else {
        panic!("key")
    };
    assert_eq!(key["name"], Value::String("lower (label)".into()));
    assert_eq!(key["desc"], Value::Integer(1));
    assert_eq!(key["coll"], Value::String("BINARY".into()));
    assert_eq!(key["key"], Value::Integer(1));
    let view = info("INFO FOR TABLE visible");
    assert_eq!(view["kind"], Value::String("view".into()));
    assert_eq!(view["indexes"], Value::Array(vec![]));
    let database = info("INFO FOR DB");
    assert!(
        matches!(&database["views"],Value::Array(views) if views.len()==1 && matches!(&views[0],Value::Object(v) if v["name"]==Value::String("visible".into())))
    );
    assert!(!format!("{database:?}").contains("__romedb_"));
    assert!(!format!("{:?}", info("INFO FOR TABLE docs")).contains("__romedb_"));
    q(&c, "BEGIN");
    q(&c, "DROP INDEX ordinary_expr");
    assert!(matches!(&info("INFO FOR TABLE ordinary")["indexes"],Value::Array(i) if i.len()==1));
    q(&c, "ROLLBACK");
    assert_eq!(info("INFO FOR TABLE ordinary"), table);
}

#[test]
fn reopening_rejects_incompatible_physical_collection_storage() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("schema.db");
    let path = path.to_str().unwrap();
    {
        let db = Database::open(path).unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE docs", &Parameters::new()).unwrap();
        c.execute("INSERT INTO docs {n:1}", &Parameters::new())
            .unwrap();
    }
    {
        let engine = turso_core::Database::open_file(
            turso_core::Database::io_for_path(path).unwrap(),
            path,
            std::sync::Arc::new(turso_core::SqliteDialect),
        )
        .unwrap();
        let raw = engine.connect().unwrap();
        raw.execute("ALTER TABLE __romedb_c_646f6373 ADD COLUMN extra TEXT")
            .unwrap();
    }
    let db = Database::open(path).unwrap();
    let error = db
        .connect()
        .err()
        .expect("reject changed storage on reopen");
    assert_eq!(error.code(), "FDB_STORAGE");
}

#[test]
fn missing_catalog_cannot_hide_persisted_collection_storage() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("orphan.db");
    let path = path.to_str().unwrap();
    {
        let db = Database::open(path).unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE docs", &Parameters::new()).unwrap();
        c.execute("INSERT INTO docs {n:1}", &Parameters::new())
            .unwrap();
    }
    {
        let engine = turso_core::Database::open_file(
            turso_core::Database::io_for_path(path).unwrap(),
            path,
            std::sync::Arc::new(turso_core::SqliteDialect),
        )
        .unwrap();
        let raw = engine.connect().unwrap();
        raw.execute("DROP TABLE __romedb_catalog").unwrap();
    }
    let db = Database::open(path).unwrap();
    let error = db
        .connect()
        .err()
        .expect("orphan storage must reject reconnect");
    assert_eq!(error.code(), "FDB_STORAGE");
    assert!(error.to_string().contains("orphan managed storage"));
}

#[test]
fn reopening_rejects_missing_indexes_and_unexpected_managed_dependencies() {
    for mutation in [
        "DROP INDEX docs_n",
        "CREATE INDEX unexpected ON __romedb_catalog(metadata)",
        "CREATE INDEX unexpected ON __romedb_c_646f6373(doc)",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dependencies.db");
        let path = path.to_str().unwrap();
        {
            let db = Database::open(path).unwrap();
            let c = db.connect().unwrap();
            q(&c, "CREATE TABLE docs");
            q(&c, "CREATE INDEX docs_n ON docs(n)");
            q(&c, "INSERT INTO docs {n:1}");
        }
        {
            let engine = turso_core::Database::open_file(
                turso_core::Database::io_for_path(path).unwrap(),
                path,
                std::sync::Arc::new(turso_core::SqliteDialect),
            )
            .unwrap();
            engine.connect().unwrap().execute(mutation).unwrap();
        }
        let db = Database::open(path).unwrap();
        let error = db.connect().err().expect(mutation);
        assert_eq!(error.code(), "FDB_STORAGE", "{mutation}");
    }
}
