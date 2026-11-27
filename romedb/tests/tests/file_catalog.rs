use romedb::{CancellationToken, Database, FileExportOptions, Parameters, Value};

const FIRST: &str = "0193a000-0000-7000-8000-000000000001";
const SECOND: &str = "0193a000-0000-7000-8000-000000000002";

fn rows(c: &romedb::Connection, sql: &str) -> Vec<Vec<Value>> {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .rows
}
fn error(c: &romedb::Connection, sql: &str) -> String {
    match c.execute(sql, &Parameters::new()) {
        Ok(_) => panic!("{sql} must fail"),
        Err(error) => error.to_string(),
    }
}
fn text(value: &str) -> Value {
    Value::String(value.into())
}
/// The host publishes one file row, as RomeDB Cloud does when an upload completes.
fn publish(c: &romedb::Connection, id: &str, name: &str, size: i64) {
    c.set_host_access(true);
    rows(
        c,
        &format!(
            "UPSERT romedb_files:`{id}` {{name:'{name}', description:'', tags:[], metadata:{{}}, \
             size:{size}, mediaType:'application/pdf', status:'ready', createdBy:'user-1'}}"
        ),
    );
    c.set_host_access(false);
}
fn connection() -> romedb::Connection {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    publish(&c, FIRST, "first.pdf", 100);
    publish(&c, SECOND, "second.pdf", 200);
    c
}
fn changes(c: &romedb::Connection) -> Vec<Vec<Value>> {
    rows(
        c,
        "SELECT file, deleted, version FROM romedb_file_changes ORDER BY file",
    )
}
fn file(id: &str) -> Value {
    Value::Record(romedb::Record {
        table: "romedb_files".into(),
        key: romedb::Key::String(id.into()),
    })
}

#[test]
fn a_customer_reads_file_rows_and_joins_them_with_its_own_records() {
    let c = connection();
    assert_eq!(
        rows(
            &c,
            "SELECT name, size FROM romedb_files WHERE status = 'ready' ORDER BY size DESC"
        ),
        vec![
            vec![text("second.pdf"), Value::Integer(200)],
            vec![text("first.pdf"), Value::Integer(100)],
        ]
    );
    rows(&c, "CREATE TABLE articles");
    rows(
        &c,
        &format!("INSERT INTO articles {{id:articles:a, title:'Report', attachment:romedb_files:`{FIRST}`}}"),
    );
    assert_eq!(
        rows(&c, "SELECT attachment FROM articles"),
        vec![vec![file(FIRST)]]
    );
    assert_eq!(
        rows(&c, &format!("SELECT name FROM romedb_files:`{FIRST}`")),
        vec![vec![text("first.pdf")]]
    );
    assert!(changes(&c).is_empty());
    assert_eq!(c.take_file_changes(), 0);
}

#[test]
fn a_customer_changes_only_the_display_fields_and_each_change_is_recorded() {
    let c = connection();
    rows(
        &c,
        &format!("UPDATE romedb_files:`{FIRST}` MERGE {{name:'renamed.pdf', description:'Signed', tags:['contract'], metadata:{{year:2026}}}}"),
    );
    assert_eq!(
        rows(
            &c,
            &format!("SELECT name, description, size FROM romedb_files:`{FIRST}`")
        ),
        vec![vec![
            text("renamed.pdf"),
            text("Signed"),
            Value::Integer(100)
        ]]
    );
    assert_eq!(
        changes(&c),
        vec![vec![file(FIRST), Value::Boolean(false), Value::Integer(1)]]
    );
    assert_eq!(c.take_file_changes(), 1);
    assert_eq!(c.take_file_changes(), 0);
    rows(
        &c,
        "UPDATE romedb_files SET description = 'All' WHERE size > 0",
    );
    assert_eq!(
        changes(&c),
        vec![
            vec![file(FIRST), Value::Boolean(false), Value::Integer(2)],
            vec![file(SECOND), Value::Boolean(false), Value::Integer(1)],
        ]
    );
    // A write that changes nothing records nothing.
    rows(
        &c,
        "UPDATE romedb_files SET description = 'All' WHERE size > 0",
    );
    assert_eq!(c.take_file_changes(), 2);

    for sql in [
        format!("UPDATE romedb_files:`{FIRST}` MERGE {{size:1}}"),
        format!("UPDATE romedb_files:`{FIRST}` MERGE {{status:'deleted'}}"),
        format!("UPDATE romedb_files:`{FIRST}` MERGE {{mediaType:'text/html'}}"),
        format!("UPDATE romedb_files:`{FIRST}` MERGE {{createdBy:'me'}}"),
        format!("UPDATE romedb_files:`{FIRST}` MERGE {{owner:'me'}}"),
        "UPDATE romedb_files SET size = 0".to_owned(),
        "UPDATE romedb_files UNSET size".to_owned(),
        format!("UPDATE romedb_files:`{FIRST}` CONTENT {{name:'only.pdf'}}"),
    ] {
        let message = error(&c, &sql);
        assert!(
            message.contains("is set by the service"),
            "{sql}: {message}"
        );
    }
    for (sql, field) in [
        (
            format!("UPDATE romedb_files:`{FIRST}` MERGE {{name:''}}"),
            "name",
        ),
        (
            format!("UPDATE romedb_files:`{FIRST}` MERGE {{name:7}}"),
            "name",
        ),
        (
            format!("UPDATE romedb_files:`{FIRST}` MERGE {{description:[]}}"),
            "description",
        ),
        (
            format!("UPDATE romedb_files:`{FIRST}` MERGE {{tags:'a'}}"),
            "tags",
        ),
        (
            format!("UPDATE romedb_files:`{FIRST}` MERGE {{tags:['a', 1]}}"),
            "tags",
        ),
        (
            format!("UPDATE romedb_files:`{FIRST}` MERGE {{metadata:'x'}}"),
            "metadata",
        ),
        ("UPDATE romedb_files UNSET name".to_owned(), "name"),
    ] {
        let message = error(&c, &sql);
        assert!(
            message.contains(&format!("romedb_files.{field} must be")),
            "{sql}: {message}"
        );
    }
    c.execute(
        &format!("UPDATE romedb_files:`{FIRST}` MERGE {{metadata:{{blob:$blob}}}}"),
        &Parameters::from([("$blob".into(), text(&"x".repeat(17_000)))]),
    )
    .unwrap_err();
    assert_eq!(c.take_file_changes(), 0);
    assert_eq!(
        rows(
            &c,
            "SELECT name, size, status FROM romedb_files ORDER BY size"
        ),
        vec![
            vec![text("renamed.pdf"), Value::Integer(100), text("ready")],
            vec![text("second.pdf"), Value::Integer(200), text("ready")],
        ]
    );
}

#[test]
fn deleting_a_row_records_the_request_in_the_same_transaction() {
    let c = connection();
    rows(&c, "BEGIN");
    rows(&c, &format!("DELETE romedb_files:`{FIRST}`"));
    assert_eq!(
        changes(&c),
        vec![vec![file(FIRST), Value::Boolean(true), Value::Integer(1)]]
    );
    rows(&c, "ROLLBACK");
    assert!(changes(&c).is_empty());
    assert_eq!(
        rows(&c, "SELECT count(*) FROM romedb_files"),
        vec![vec![Value::Integer(2)]]
    );
    c.take_file_changes();

    // A change and then a delete of the same file is one row, and it stays a delete.
    rows(
        &c,
        &format!("UPDATE romedb_files:`{FIRST}` MERGE {{name:'x.pdf'}}"),
    );
    rows(&c, "DELETE FROM romedb_files WHERE size >= 100");
    assert_eq!(
        changes(&c),
        vec![
            vec![file(FIRST), Value::Boolean(true), Value::Integer(2)],
            vec![file(SECOND), Value::Boolean(true), Value::Integer(1)],
        ]
    );
    assert_eq!(c.take_file_changes(), 3);
    assert!(rows(&c, "SELECT name FROM romedb_files").is_empty());
}

#[test]
fn a_customer_cannot_add_rows_or_touch_the_change_records() {
    let c = connection();
    rows(
        &c,
        &format!("UPDATE romedb_files:`{FIRST}` MERGE {{name:'x.pdf'}}"),
    );
    for sql in [
        "INSERT INTO romedb_files {name:'mine.pdf', size:1}".to_owned(),
        "INSERT INTO romedb_files (name, size) VALUES ('mine.pdf', 1)".to_owned(),
        "UPSERT romedb_files:mine {name:'mine.pdf', size:1}".to_owned(),
        format!("INSERT OR REPLACE INTO romedb_files (id, name) VALUES (romedb_files:`{FIRST}`, 'swap.pdf')"),
        "INSERT INTO romedb_files (name) SELECT name FROM romedb_files".to_owned(),
        "INSERT INTO romedb_file_changes {deleted:true}".to_owned(),
        "UPDATE romedb_file_changes SET deleted = true".to_owned(),
        "DELETE FROM romedb_file_changes".to_owned(),
    ] {
        let message = error(&c, &sql);
        assert!(message.contains("managed by RomeDB Cloud"), "{sql}: {message}");
    }
    assert_eq!(
        rows(&c, "SELECT count(*) FROM romedb_files"),
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        changes(&c),
        vec![vec![file(FIRST), Value::Boolean(false), Value::Integer(1)]]
    );
}

#[test]
fn a_customer_cannot_change_the_definition_or_take_a_reserved_name() {
    let c = connection();
    for sql in [
        "DROP TABLE romedb_files",
        "DROP TABLE romedb_file_changes",
        "DEFINE FIELD owner ON romedb_files TYPE string",
        "DEFINE SCHEMA ON romedb_files STRICT",
        "REMOVE FIELD name ON romedb_files",
        "CREATE INDEX files_name ON romedb_files(name)",
        "CREATE SEARCH INDEX files_text ON romedb_files(name) USING FULLTEXT",
        "CREATE EDGE TABLE attached FROM romedb_files TO romedb_files",
    ] {
        let message = error(&c, sql);
        assert!(
            message.contains("managed by RomeDB Cloud"),
            "{sql}: {message}"
        );
    }
    for sql in [
        "CREATE TABLE romedb_mine",
        "CREATE TABLE ROMEDB_Mine",
        "CREATE TABLE romedb_mine (a INTEGER)",
        "CREATE TABLE \"romedb_mine\" (a INTEGER)",
        "INSERT INTO romedb_mine {a:1}",
        "UPSERT romedb_mine:a {a:1}",
        "CREATE VIEW romedb_view AS SELECT 1",
    ] {
        let message = error(&c, sql);
        assert!(message.contains("are reserved"), "{sql}: {message}");
    }
    rows(&c, "CREATE TABLE plain (a INTEGER)");
    let message = error(&c, "ALTER TABLE plain RENAME TO romedb_plain");
    assert!(message.contains("are reserved"), "{message}");
    // The names stay free for anything that is not a table.
    rows(&c, "CREATE TABLE notes");
    rows(
        &c,
        "INSERT INTO notes {romedb_files:'a field with that name'}",
    );
    assert_eq!(
        rows(&c, "SELECT count(*) FROM romedb_files"),
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn the_host_writes_rows_freely_and_takes_the_change_records_away() {
    let c = connection();
    rows(
        &c,
        &format!("UPDATE romedb_files:`{FIRST}` MERGE {{name:'x.pdf'}}"),
    );
    rows(&c, &format!("DELETE romedb_files:`{SECOND}`"));
    c.take_file_changes();

    c.set_host_access(true);
    assert!(c.host_access());
    // The host acts on what it read, and removes a record only at the version it read.
    assert_eq!(
        rows(
            &c,
            "SELECT file, deleted, version FROM romedb_file_changes ORDER BY file"
        ),
        vec![
            vec![file(FIRST), Value::Boolean(false), Value::Integer(1)],
            vec![file(SECOND), Value::Boolean(true), Value::Integer(1)],
        ]
    );
    rows(
        &c,
        &format!("UPDATE romedb_files:`{FIRST}` MERGE {{size:150, description:'by the service'}}"),
    );
    rows(
        &c,
        &format!(
            "DELETE FROM romedb_file_changes WHERE file = romedb_files:`{FIRST}` AND version = 1"
        ),
    );
    rows(
        &c,
        &format!(
            "DELETE FROM romedb_file_changes WHERE file = romedb_files:`{SECOND}` AND version = 7"
        ),
    );
    publish(&c, "0193a000-0000-7000-8000-000000000003", "third.pdf", 300);
    c.set_host_access(true);
    rows(
        &c,
        "DELETE romedb_files:`0193a000-0000-7000-8000-000000000003`",
    );
    c.set_host_access(false);

    // Nothing the host did was recorded as a customer change.
    assert_eq!(c.take_file_changes(), 0);
    assert_eq!(
        changes(&c),
        vec![vec![file(SECOND), Value::Boolean(true), Value::Integer(1)]]
    );
    assert_eq!(
        rows(&c, "SELECT name, size, description FROM romedb_files"),
        vec![vec![
            text("x.pdf"),
            Value::Integer(150),
            text("by the service")
        ]]
    );
}

#[test]
fn a_copy_that_becomes_another_database_carries_no_file_rows() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("site.romedb");
    std::fs::File::create(&source).unwrap();
    let database = Database::create_mvcc(source.to_str().unwrap()).unwrap();
    let c = database.connect().unwrap();
    publish(&c, FIRST, "first.pdf", 100);
    publish(&c, SECOND, "second.pdf", 200);
    rows(&c, "CREATE TABLE articles");
    rows(
        &c,
        &format!("INSERT INTO articles {{id:articles:a, attachment:romedb_files:`{FIRST}`}}"),
    );
    rows(&c, &format!("DELETE romedb_files:`{SECOND}`"));

    let export = |name: &str, new_identity: bool| {
        let destination = directory.path().join(name);
        let options = FileExportOptions {
            new_identity,
            ..Default::default()
        };
        database
            .start_file_export(&destination, &options, &CancellationToken::new())
            .unwrap()
            .finish()
            .unwrap();
        Database::open(destination.to_str().unwrap())
            .unwrap()
            .connect()
            .unwrap()
    };
    let moved = export("moved.romedb", true);
    assert!(rows(&moved, "SELECT name FROM romedb_files").is_empty());
    assert!(changes(&moved).is_empty());
    // The application's own link stays; it now points at no row, as any weak link can.
    assert_eq!(
        rows(&moved, "SELECT attachment FROM articles"),
        vec![vec![file(FIRST)]]
    );
    let message = error(&moved, "INSERT INTO romedb_files {name:'mine.pdf'}");
    assert!(message.contains("managed by RomeDB Cloud"), "{message}");

    // A copy that stays the same database keeps its rows and its requests.
    let same = export("same.romedb", false);
    assert_eq!(
        rows(&same, "SELECT name FROM romedb_files"),
        vec![vec![text("first.pdf")]]
    );
    assert_eq!(
        changes(&same),
        vec![vec![file(SECOND), Value::Boolean(true), Value::Integer(1)]]
    );
}

#[test]
fn a_credential_without_file_access_reads_rows_and_changes_none() {
    let c = connection();
    let rename = format!("UPDATE romedb_files:`{FIRST}` MERGE {{name:'x.pdf'}}");
    let delete = format!("DELETE romedb_files:`{SECOND}`");

    c.set_file_access(false, false);
    assert!(error(&c, &rename).contains("needs the files:write scope"));
    assert!(error(&c, &delete).contains("needs the files:delete scope"));
    assert!(error(&c, "DELETE FROM romedb_files WHERE size > 0").contains("files:delete"));
    assert_eq!(
        rows(&c, "SELECT count(*) FROM romedb_files"),
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(c.take_file_changes(), 0);
    assert!(changes(&c).is_empty());

    c.set_file_access(true, false);
    rows(&c, &rename);
    assert!(error(&c, &delete).contains("files:delete"));
    c.set_file_access(false, true);
    assert!(error(&c, &rename).contains("files:write"));
    rows(&c, &delete);
    assert_eq!(c.take_file_changes(), 2);

    // The host's own requests do not depend on a customer credential.
    c.set_host_access(true);
    rows(
        &c,
        &format!("UPDATE romedb_files:`{FIRST}` MERGE {{size:5}}"),
    );
    rows(&c, &format!("DELETE romedb_files:`{FIRST}`"));
}

// RomeDB Cloud sends exactly these statements (cloud/src/files-catalog.mjs). Keep both in step.
#[test]
fn the_statements_the_service_sends_run_as_written() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let fields = [
        "name",
        "description",
        "tags",
        "metadata",
        "size",
        "mediaType",
        "checksum",
        "generation",
        "status",
        "createdBy",
        "createdAt",
        "updatedAt",
    ];
    let row = format!(
        "{{{}}}",
        fields
            .iter()
            .map(|field| format!("{field}:${field}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let values = Parameters::from([
        ("$name".into(), text("a.pdf")),
        ("$description".into(), text("")),
        ("$tags".into(), Value::Array(vec![])),
        ("$metadata".into(), Value::Object(Default::default())),
        ("$size".into(), Value::Integer(10)),
        ("$mediaType".into(), text("application/pdf")),
        ("$checksum".into(), text("oql8lPwSS58=")),
        ("$generation".into(), text("\"etag-1\"")),
        ("$status".into(), text("ready")),
        ("$createdBy".into(), Value::Null),
        ("$createdAt".into(), Value::Integer(1_795_000_000_000)),
        ("$updatedAt".into(), Value::Integer(1_795_000_000_000)),
    ]);

    c.set_host_access(true);
    rows(&c, "CREATE TABLE IF NOT EXISTS romedb_files");
    rows(&c, "CREATE TABLE IF NOT EXISTS romedb_files");
    rows(&c, "DELETE FROM romedb_files");
    rows(&c, "DELETE FROM romedb_file_changes");
    for _ in 0..2 {
        c.execute(&format!("UPSERT romedb_files:`{FIRST}` {row}"), &values)
            .unwrap();
    }
    rows(&c, &format!("DELETE romedb_files:`{SECOND}`"));
    c.set_host_access(false);

    assert_eq!(
        rows(
            &c,
            "SELECT name, size, checksum, createdBy, createdAt FROM romedb_files"
        ),
        vec![vec![
            text("a.pdf"),
            Value::Integer(10),
            text("oql8lPwSS58="),
            Value::Null,
            Value::Integer(1_795_000_000_000)
        ]]
    );
    rows(
        &c,
        &format!("UPDATE romedb_files:`{FIRST}` MERGE {{name:'b.pdf'}}"),
    );
    assert_eq!(
        rows(
            &c,
            "SELECT file, deleted, version FROM romedb_file_changes LIMIT 100"
        ),
        vec![vec![file(FIRST), Value::Boolean(false), Value::Integer(1)]]
    );
    assert_eq!(
        rows(
            &c,
            &format!("SELECT name, description, tags, metadata FROM romedb_files:`{FIRST}`")
        ),
        vec![vec![
            text("b.pdf"),
            text(""),
            Value::Array(vec![]),
            Value::Object(Default::default())
        ]]
    );
    c.set_host_access(true);
    c.execute(
        &format!("DELETE FROM romedb_file_changes WHERE file = romedb_files:`{FIRST}` AND version = $version"),
        &Parameters::from([("$version".into(), Value::Integer(1))]),
    )
    .unwrap();
    assert!(changes(&c).is_empty());
}
