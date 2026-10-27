// Current-engine adapter of the historical proposal reproduction.
// Standalone native-engine reproduction; no RomeDB lowering is involved.
use std::sync::Arc;
use turso_core::{Connection, Database, DatabaseOpts, OpenFlags, Value};

fn run(connection: &Arc<Connection>, sql: &str) -> Vec<Vec<Value>> {
    let mut statement = connection.prepare(sql).unwrap();
    let mut rows = Vec::new();
    statement
        .run_with_row_callback(|row| {
            rows.push(row.get_values().cloned().collect());
            Ok(())
        })
        .unwrap();
    rows
}

fn main() {
    let path = ":memory:";
    let db = Database::open_file_with_flags(
        Database::io_for_path(path).unwrap(),
        path,
        OpenFlags::default(),
        DatabaseOpts::new().with_index_method(true),
        None,
        Arc::new(turso_core::SqliteDialect),
    )
    .unwrap();
    let connection = db.connect().unwrap();
    run(&connection, "CREATE TABLE docs(title TEXT)");
    run(&connection, "INSERT INTO docs VALUES ('Fast river')");
    let baseline = run(&connection, "PRAGMA integrity_check");
    run(
        &connection,
        "CREATE INDEX docs_text ON docs USING fts(title)",
    );
    println!(
        "FTS hits: {:?}",
        run(
            &connection,
            "SELECT title FROM docs WHERE fts_match(title,'river')"
        )
    );
    let integrity = run(&connection, "PRAGMA integrity_check");
    let quick = run(&connection, "PRAGMA quick_check");
    println!("integrity_check: {integrity:?}; quick_check: {quick:?}");
    run(&connection, "DROP INDEX docs_text");
    let dropped = run(&connection, "PRAGMA integrity_check");
    println!("after DROP INDEX: {dropped:?}");
    assert_eq!(
        integrity, baseline,
        "FTS backing storage is not a secondary row index"
    );
    assert_eq!(quick, baseline);
    assert_eq!(
        dropped, baseline,
        "dropping FTS must free its backing pages"
    );
}
