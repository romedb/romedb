// Standalone native-engine reproduction; see fts-cache-snapshot.md.
use std::sync::Arc;
use turso_core::{Connection, Database, DatabaseOpts, OpenFlags, Value};

fn run(c: &Arc<Connection>, sql: &str) -> Vec<Vec<Value>> {
    let mut statement = c.prepare(sql).unwrap();
    let mut rows = Vec::new();
    statement.run_with_row_callback(|row| {
        rows.push(row.get_values().cloned().collect());
        Ok(())
    }).unwrap();
    rows
}
fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| ":memory:".into());
    let db = Database::open_file_with_flags(Database::io_for_path(&path).unwrap(), &path,
        OpenFlags::default(), DatabaseOpts::new().with_index_method(true), None).unwrap();
    let writer = db.connect().unwrap();
    run(&writer, "CREATE TABLE docs(id TEXT UNIQUE, title TEXT)");
    run(&writer, "INSERT INTO docs VALUES ('a','database'),('b','database search'),('c',NULL)");
    run(&writer, "CREATE INDEX texts ON docs USING fts(title)");
    let reader = db.connect().unwrap();
    let query = "SELECT id,fts_score(title,'database') AS score FROM docs WHERE fts_match(title,'database') ORDER BY score DESC,id LIMIT 100";
    let baseline = run(&reader, query);
    assert_eq!(baseline.len(),2);
    run(&writer, "BEGIN");
    run(&writer, "INSERT INTO docs VALUES ('d','database')");
    assert_eq!(run(&writer, query).len(),3);
    assert_eq!(run(&reader, query),baseline,"reader saw uncommitted writer FTS cache");
    run(&reader, "BEGIN");
    assert_eq!(run(&reader, query),baseline);
    run(&writer, "COMMIT");
    assert_eq!(run(&reader, query),baseline,"reader lost its pinned snapshot");
    run(&reader, "ROLLBACK");
    let committed = run(&reader, query);
    assert_eq!(committed.len(),3);
    run(&writer, "BEGIN");
    run(&writer, "DELETE FROM docs WHERE id='a'");
    assert_eq!(run(&writer, query).len(),2);
    assert_eq!(run(&reader, query),committed);
    run(&writer, "SAVEPOINT change");
    run(&writer, "UPDATE docs SET title='nothing' WHERE id='b'");
    assert_eq!(run(&writer, query).len(),1);
    run(&writer, "ROLLBACK TO change");
    run(&writer, "RELEASE change");
    assert_eq!(run(&writer, query).len(),2);
    run(&writer, "ROLLBACK");
    assert_eq!(run(&writer, query),committed);
    assert_eq!(run(&reader, query),committed);
    println!("native FTS isolation, pinned snapshot, commit and rollback checks passed");
}
