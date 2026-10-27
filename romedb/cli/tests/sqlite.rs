use romedb::{Database, Parameters, Value};
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn cli(args: &[&str], source: &Path, destination: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_romedb-cli"));
    command.arg("sqlite").args(args).arg(source);
    if let Some(path) = destination {
        command.arg(path);
    }
    command.output().unwrap()
}
fn report(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{output:?}"))
}
fn query(conn: &romedb::Connection, sql: &str) -> Vec<Vec<Value>> {
    conn.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .rows
}

#[test]
fn stock_sqlite_adoption_preserves_types_schema_and_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("original.db");
    let destination = temp.path().join("adopted.db");
    let sqlite = Connection::open(&source).unwrap();
    sqlite.execute_batch("CREATE TABLE parent(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, n INTEGER, r REAL, b BLOB, empty TEXT) STRICT;
        CREATE TABLE child(id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent(id) ON DELETE CASCADE);
        CREATE TABLE audit(event TEXT);
        CREATE VIEW names AS SELECT id,name FROM parent;
        CREATE INDEX parent_name ON parent(lower(name)) WHERE name IS NOT NULL;
        CREATE TRIGGER inserted AFTER INSERT ON parent BEGIN INSERT INTO audit VALUES(new.name); END;
        INSERT INTO parent VALUES(10,'deleted',NULL,NULL,NULL,NULL); DELETE FROM parent;
        INSERT INTO parent(name,n,r,b,empty) VALUES('Tân 雪',-9223372036854775808,1.25,x'00ff',NULL);
        INSERT INTO child VALUES(1,11);
        PRAGMA user_version=17; PRAGMA application_id=42;").unwrap();
    drop(sqlite);
    let before = fs::read(&source).unwrap();
    let checked = cli(&["check"], &source, None);
    assert!(checked.status.success(), "{checked:?}");
    assert_eq!(report(&checked)["compatible"], true);
    assert_eq!(fs::read(&source).unwrap(), before);
    let output = cli(&["import"], &source, Some(&destination));
    assert!(output.status.success(), "{output:?}");
    assert_eq!(report(&output)["compatible"], true);
    {
        let db = Database::open(destination.to_str().unwrap()).unwrap();
        let conn = db.connect().unwrap();
        assert_eq!(
            query(&conn, "SELECT id,name,n,r,b,empty FROM parent"),
            vec![vec![
                Value::Integer(11),
                Value::String("Tân 雪".into()),
                Value::Integer(i64::MIN),
                Value::Number(1.25),
                Value::Binary(vec![0, 255]),
                Value::Null
            ]]
        );
        assert_eq!(
            query(&conn, "SELECT name FROM names"),
            vec![vec![Value::String("Tân 雪".into())]]
        );
        assert_eq!(
            query(&conn, "PRAGMA user_version"),
            vec![vec![Value::Integer(17)]]
        );
        assert_eq!(
            query(&conn, "PRAGMA application_id"),
            vec![vec![Value::Integer(42)]]
        );
        query(&conn, "PRAGMA foreign_keys=ON");
        assert!(conn
            .execute("INSERT INTO child VALUES(2,999)", &Parameters::new())
            .is_err());
        assert!(conn
            .execute("UPDATE parent SET n='invalid'", &Parameters::new())
            .is_err());
        query(
            &conn,
            "UPDATE parent SET name='updated',n=9223372036854775807",
        );
        query(&conn, "INSERT INTO parent(name) VALUES('new')");
        assert_eq!(
            query(&conn, "SELECT id FROM parent WHERE name='new'"),
            vec![vec![Value::Integer(12)]]
        );
        assert_eq!(
            query(&conn, "SELECT count(*) FROM audit"),
            vec![vec![Value::Integer(3)]]
        );
        query(&conn, "DELETE FROM parent WHERE id=11");
        assert_eq!(
            query(&conn, "SELECT count(*) FROM child"),
            vec![vec![Value::Integer(0)]]
        );
        query(&conn, "PRAGMA wal_checkpoint(TRUNCATE)");
    }
    let readback = Connection::open(&destination).unwrap();
    assert_eq!(
        readback
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        readback
            .query_row("SELECT name FROM parent", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "new"
    );
    assert_eq!(fs::read(&source).unwrap(), before);
    drop(readback);
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
}

#[test]
fn snapshot_includes_committed_wal_but_excludes_uncommitted_changes() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("wal.db");
    let destination = temp.path().join("copy.db");
    let sqlite = Connection::open(&source).unwrap();
    sqlite
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        CREATE TABLE events(n INTEGER); INSERT INTO events VALUES(1);
        BEGIN IMMEDIATE; INSERT INTO events VALUES(2);",
        )
        .unwrap();
    let before = fs::read(&source).unwrap();
    let wal_path = temp.path().join("wal.db-wal");
    let wal_before = fs::read(&wal_path).unwrap();
    assert!(!wal_before.is_empty());
    let output = cli(&["import"], &source, Some(&destination));
    assert!(output.status.success(), "{output:?}");
    let db = Database::open(destination.to_str().unwrap()).unwrap();
    assert_eq!(
        query(&db.connect().unwrap(), "SELECT n FROM events"),
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(fs::read(&source).unwrap(), before);
    assert_eq!(fs::read(&wal_path).unwrap(), wal_before);
    sqlite.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn rejects_unsupported_schema_without_publishing_or_touching_source() {
    for (schema, reason) in [
        (
            "PRAGMA auto_vacuum=FULL; CREATE TABLE items(n INT)",
            "auto_vacuum",
        ),
        (
            "PRAGMA auto_vacuum=INCREMENTAL; CREATE TABLE items(n INT)",
            "auto_vacuum",
        ),
        (
            "CREATE TABLE items(k TEXT PRIMARY KEY, v INT) WITHOUT ROWID",
            "WITHOUT ROWID",
        ),
        (
            "CREATE TABLE items(n INT, x INT GENERATED ALWAYS AS(n+1) VIRTUAL)",
            "generated columns",
        ),
        (
            "CREATE TABLE items(n INT, x INT GENERATED ALWAYS AS(n+1) STORED)",
            "generated columns",
        ),
        (
            "CREATE VIRTUAL TABLE items USING fts5(body)",
            "virtual tables",
        ),
        (
            "CREATE VIRTUAL TABLE items USING rtree(id,minx,maxx,miny,maxy)",
            "virtual tables",
        ),
        (
            "PRAGMA encoding='UTF-16le'; CREATE TABLE items(n INT)",
            "UTF-16",
        ),
        (
            "CREATE TABLE __romedb_catalog(name TEXT, metadata TEXT)",
            "reserved object",
        ),
        (
            "CREATE TABLE __turso_internal_collision(n INT)",
            "reserved object",
        ),
        (
            "CREATE VIEW unknown_fn AS SELECT made_up_function(1)",
            "cannot be queried",
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("original.db");
        let destination = temp.path().join("copy.db");
        Connection::open(&source)
            .unwrap()
            .execute_batch(schema)
            .unwrap();
        let before = fs::read(&source).unwrap();
        for args in ["check", "import"] {
            let output = cli(
                &[args],
                &source,
                if args == "import" {
                    Some(&destination)
                } else {
                    None
                },
            );
            assert!(!output.status.success(), "{schema}: {output:?}");
            let body = report(&output);
            assert_eq!(body["compatible"], false, "{body}");
            assert!(
                body["unsupported"].to_string().contains(reason),
                "{schema}: {body}"
            );
            assert!(!destination.exists());
            assert_eq!(fs::read(&source).unwrap(), before);
            assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
        }
    }
}

#[test]
fn refuses_overwrite_sidecars_aliases_and_invalid_files() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.db");
    let destination = temp.path().join("copy.db");
    Connection::open(&source)
        .unwrap()
        .execute_batch("CREATE TABLE items(n INT)")
        .unwrap();
    let before = fs::read(&source).unwrap();
    assert!(!cli(&["import"], &source, Some(&source)).status.success());
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = temp.path().join(format!("copy.db{suffix}"));
        fs::write(&path, b"retain me").unwrap();
        assert!(!cli(&["import"], &source, Some(&destination))
            .status
            .success());
        assert_eq!(fs::read(&path).unwrap(), b"retain me");
        fs::remove_file(path).unwrap();
    }
    fs::hard_link(&source, &destination).unwrap();
    assert!(!cli(&["import"], &source, Some(&destination))
        .status
        .success());
    fs::remove_file(&destination).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(temp.path().join("missing"), &destination).unwrap();
        assert!(!cli(&["import"], &source, Some(&destination))
            .status
            .success());
        fs::remove_file(&destination).unwrap();
    }
    assert_eq!(fs::read(&source).unwrap(), before);
    fs::write(&source, vec![0xff; 4096]).unwrap();
    assert!(!cli(&["import"], &source, Some(&destination))
        .status
        .success());
    assert!(!destination.exists());
    assert_eq!(fs::read(&source).unwrap(), vec![0xff; 4096]);
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}
