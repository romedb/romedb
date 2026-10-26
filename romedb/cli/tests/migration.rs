#![cfg(target_os = "linux")]
use romedb::{Database, Parameters, Value};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::Path,
    process::Command,
};

fn create(path: &Path) {
    let database = Database::open_with_manual_wal(path.to_str().unwrap()).unwrap();
    let connection = database.connect().unwrap();
    for sql in [
        "CREATE TABLE rows (id INTEGER PRIMARY KEY, name TEXT UNIQUE, payload BLOB)",
        "INSERT INTO rows VALUES(1,'original',x'00ff')",
        "CREATE VIEW row_names AS SELECT name FROM rows",
        "CREATE TABLE docs;",
        "DEFINE FIELD title ON docs TYPE string REQUIRED",
        "DEFINE FIELD v ON docs TYPE vector<2>",
        "CREATE INDEX docs_title ON docs(title)",
        "CREATE SEARCH INDEX docs_text ON docs(title) USING FULLTEXT",
        "CREATE SEARCH INDEX docs_vector ON docs(v) USING VECTOR WITH (dimensions=2,metric='l2')",
        "CREATE SEARCH INDEX docs_spatial ON docs(location) USING SPATIAL",
        "CREATE FUNCTION app::normalize(value string) RETURNS string LANGUAGE JAVASCRIPT AS 'return value.trim().toLowerCase();'",
        "INSERT INTO docs {id:docs:original,title:'originalterm',count:9223372036854775807,v:vector32('[1,0]'),location:geo::point(0,0),optional:null}",
    ] { connection.execute(sql, &Parameters::new()).unwrap_or_else(|error| panic!("{sql}: {error}")); }
}

fn migrate(source: &Path, output: &Path, backup: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_romedb"))
        .arg("migrate-to-mvcc")
        .arg(source)
        .arg(output)
        .arg("--backup")
        .arg(backup)
        .output()
        .unwrap()
}

fn verify(path: &Path, mode: &str) {
    let database = Database::open(path.to_str().unwrap()).unwrap();
    let connection = database.connect().unwrap();
    assert_eq!(
        connection
            .execute("PRAGMA journal_mode", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::String(mode.into())]]
    );
    let result = connection
        .execute("SELECT name,payload FROM rows", &Parameters::new())
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Value::String("original".into()),
            Value::Binary(vec![0, 255])
        ]]
    );
    assert_eq!(
        connection
            .execute("SELECT name FROM row_names", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::String("original".into())]]
    );
    assert_eq!(
        connection
            .execute("SELECT app::normalize('  HELLO  ')", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::String("hello".into())]]
    );
    assert_eq!(
        connection
            .execute("SELECT count FROM docs", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::Integer(i64::MAX)]]
    );
    for sql in [
        "SELECT id FROM docs WHERE title='originalterm'",
        "SELECT id FROM search::text('docs_text','originalterm',10)",
        "SELECT id FROM search::vector('docs_vector',vector32('[1,0]'),1)",
        "SELECT id FROM search::near('docs_spatial',geo::point(0,0),10)",
    ] {
        assert_eq!(
            connection
                .execute(sql, &Parameters::new())
                .unwrap()
                .rows
                .len(),
            1,
            "{sql}"
        );
    }
    let report = connection
        .check_collection_integrity("docs", Default::default())
        .unwrap();
    assert_eq!((report.documents, report.index_entries), (1, 4));
    assert_eq!(
        connection
            .execute("PRAGMA integrity_check", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![Value::String("ok".into())]]
    );
}

#[test]
fn offline_migration_preserves_source_backup_and_all_index_types() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("app.romedb");
    let output = directory.path().join("service");
    let backup = directory.path().join("backup");
    create(&source);
    let before = fs::read(&source).unwrap();
    let wal_path = directory.path().join("app.romedb-wal");
    let wal = fs::read(&wal_path).unwrap();
    assert!(wal.len() > 32);
    let result = migrate(&source, &output, &backup);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(&source).unwrap(), before);
    assert_eq!(fs::read(&wal_path).unwrap(), wal);
    assert_eq!(fs::read(backup.join("database.romedb")).unwrap(), before);
    assert_eq!(fs::read(backup.join("database.romedb-wal")).unwrap(), wal);
    assert_eq!(
        fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o700
    );
    verify(&output.join("database.romedb"), "mvcc");
    {
        let service =
            romedb_service::DatabaseService::open(&output.join("database.romedb")).unwrap();
        let read = serde_json::json!({"readVersion":2,"requestId":"typed","afterSequence":0,"statements":[{"sql":"SELECT count FROM docs WHERE id=docs:original"}]}).to_string();
        let reply: serde_json::Value = serde_json::from_str(&service.read(&read).unwrap()).unwrap();
        assert_eq!(
            reply["results"][0]["rows"][0][0],
            serde_json::json!({"type":"Integer","value":i64::MAX.to_string()})
        );
        let failure = serde_json::json!({"requestId":"rollback","afterSequence":0,"statements":[
            {"sql":"UPDATE docs SET title='changedterm' WHERE id=docs:original"},
            {"sql":"INSERT INTO rows VALUES(2,'original',NULL)"}
        ]})
        .to_string();
        let reply: serde_json::Value =
            serde_json::from_str(&service.query(&failure).unwrap()).unwrap();
        assert!(reply["error"]["code"].is_string(), "{reply}");
    }
    verify(&output.join("database.romedb"), "mvcc");
    let restored = directory.path().join("restored");
    let result = Command::new(env!("CARGO_BIN_EXE_romedb"))
        .arg("restore-migration")
        .arg(&backup)
        .arg(&restored)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    verify(&restored.join("database.romedb"), "wal");
    assert_eq!(fs::read(backup.join("database.romedb")).unwrap(), before);
    assert_eq!(fs::read(backup.join("database.romedb-wal")).unwrap(), wal);
    verify(&source, "wal");
}

#[test]
fn migration_rejects_live_owners_and_never_replaces_destinations() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("app.romedb");
    create(&source);
    let output = directory.path().join("service");
    let backup = directory.path().join("backup");
    {
        let database = Database::open_with_manual_wal(source.to_str().unwrap()).unwrap();
        let connection = database.connect().unwrap();
        let result = migrate(&source, &output, &backup);
        assert!(!result.status.success());
        assert!(!output.exists());
        assert!(!backup.exists());
        assert_eq!(
            connection
                .execute("SELECT name FROM rows", &Parameters::new())
                .unwrap()
                .rows[0][0],
            Value::String("original".into())
        );
    }
    fs::create_dir(&output).unwrap();
    fs::write(output.join("keep"), b"preserve").unwrap();
    assert!(!migrate(&source, &output, &backup).status.success());
    assert_eq!(fs::read(output.join("keep")).unwrap(), b"preserve");
    assert!(!backup.exists());
    fs::remove_dir_all(&output).unwrap();
    symlink(directory.path().join("missing"), &backup).unwrap();
    assert!(!migrate(&source, &output, &backup).status.success());
    assert!(fs::symlink_metadata(&backup).is_ok());
}

#[test]
fn audit_limits_and_modified_backups_prevent_publication() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("app.romedb");
    create(&source);
    let output = directory.path().join("service");
    let partial = directory.path().join("partial");
    let result = Command::new(env!("CARGO_BIN_EXE_romedb"))
        .arg("migrate-to-mvcc")
        .arg(&source)
        .arg(&output)
        .arg("--backup")
        .arg(&partial)
        .args(["--max-documents", "0"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!output.exists());
    assert!(!partial.join("manifest.json").exists());
    let restored = directory.path().join("restored");
    assert!(!Command::new(env!("CARGO_BIN_EXE_romedb"))
        .arg("restore-migration")
        .arg(&partial)
        .arg(&restored)
        .output()
        .unwrap()
        .status
        .success());
    let backup = directory.path().join("backup");
    let result = migrate(&source, &output, &backup);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    fs::write(backup.join("database.romedb"), b"changed").unwrap();
    assert!(!Command::new(env!("CARGO_BIN_EXE_romedb"))
        .arg("restore-migration")
        .arg(&backup)
        .arg(&restored)
        .output()
        .unwrap()
        .status
        .success());
    assert!(!restored.exists());
    verify(&source, "wal");
}
