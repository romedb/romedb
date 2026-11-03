use romedb::{Database, Parameters, Value};
use turso_core::Value as EngineValue;

fn raw(path: &str) -> std::sync::Arc<turso_core::Connection> {
    turso_core::Database::open_file(
        turso_core::Database::io_for_path(path).unwrap(),
        path,
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap()
    .connect()
    .unwrap()
}

fn first_doc(raw: &std::sync::Arc<turso_core::Connection>) -> Vec<u8> {
    let mut statement = raw
        .prepare("SELECT doc FROM __romedb_c_706f737473")
        .unwrap();
    let mut doc = None;
    statement
        .run_with_row_callback(|row| {
            if let Some(EngineValue::Blob(bytes)) = row.get_values().next() {
                doc = Some(bytes.clone());
            }
            Ok(())
        })
        .unwrap();
    doc.unwrap()
}

#[test]
fn files_with_the_earlier_value_prefix_are_refused_before_any_write() {
    for mvcc in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("old.romedb");
        let text = path.to_str().unwrap();
        {
            let database = if mvcc {
                std::fs::File::create(&path).unwrap();
                Database::create_mvcc(text).unwrap()
            } else {
                Database::open(text).unwrap()
            };
            let c = database.connect().unwrap();
            c.execute(
                "INSERT INTO posts {id:posts:a,title:'kept'}",
                &Parameters::new(),
            )
            .unwrap();
            c.execute("PRAGMA wal_checkpoint(TRUNCATE)", &Parameters::new())
                .unwrap();
        }
        {
            let raw = raw(text);
            assert!(first_doc(&raw).starts_with(b"RDB\x01"));
            raw.execute(
                "UPDATE __romedb_c_706f737473 SET doc=CAST('FDB'||char(1)||substr(doc,5) AS BLOB)",
            )
            .unwrap();
            raw.execute("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
            assert!(first_doc(&raw).starts_with(b"FDB\x01"));
        }
        let before = std::fs::read(&path).unwrap();
        let error = if mvcc {
            Database::open_mvcc(text).err().unwrap()
        } else {
            Database::open(text).err().unwrap()
        };
        assert_eq!(error.code(), "RDB_UNSUPPORTED", "{error}");
        assert!(error.to_string().contains("0.2.12 or earlier"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn relational_tables_and_empty_collections_still_open() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("app.romedb");
    let text = path.to_str().unwrap();
    {
        let c = Database::open(text).unwrap().connect().unwrap();
        c.execute("CREATE TABLE posts", &Parameters::new()).unwrap();
        c.execute("CREATE TABLE plain(n INTEGER)", &Parameters::new())
            .unwrap();
        c.execute("INSERT INTO plain VALUES (1)", &Parameters::new())
            .unwrap();
    }
    let c = Database::open(text).unwrap().connect().unwrap();
    assert_eq!(
        c.execute("SELECT n FROM plain", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::Integer(1)]]
    );
    assert!(c
        .execute("SELECT * FROM posts", &Parameters::new())
        .unwrap()
        .rows
        .is_empty());
}
