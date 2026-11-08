use romedb::{Connection, Database, Parameters, Value};
use std::path::Path;

fn q(c: &Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}
fn rows(c: &Connection, sql: &str) -> Vec<Vec<Value>> {
    q(c, sql).rows
}
fn ids(c: &Connection, sql: &str) -> Vec<String> {
    let mut found: Vec<String> = rows(c, sql)
        .iter()
        .map(|row| format!("{:?}", row[0]))
        .collect();
    found.sort();
    found
}
// A database file as it stands, copied while nothing writes.
fn copy(c: &Connection, from: &Path, to: &Path) {
    q(c, "PRAGMA wal_checkpoint(TRUNCATE)");
    std::fs::copy(from, to).unwrap();
}

fn seed(c: &Connection) {
    q(c, "CREATE TABLE articles");
    q(c, "CREATE INDEX articles_n ON articles(n)");
    q(
        c,
        "CREATE SEARCH INDEX articles_title ON articles(title) USING FULLTEXT",
    );
    q(c, "CREATE TABLE notes");
    q(c, "CREATE TABLE plain(id INTEGER PRIMARY KEY, v TEXT)");
    q(c, "CREATE TABLE logs(k TEXT, v INTEGER)");
    q(
        c,
        "CREATE TABLE counted(id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT)",
    );
    q(c, "BEGIN");
    for n in 0..600 {
        q(c, &format!("INSERT INTO articles {{id:type::record('articles','a{n:04}'),n:{n},title:'title{} common',body:'body{} shared'}}", n % 97, n % 89));
    }
    for n in 0..50 {
        q(
            c,
            &format!("INSERT INTO notes {{id:type::record('notes','n{n:02}'),v:{n}}}"),
        );
        q(c, &format!("INSERT INTO plain VALUES ({n}, 'p{n}')"));
        q(c, &format!("INSERT INTO logs VALUES ('k{n}', {n})"));
        q(c, &format!("INSERT INTO counted(v) VALUES ('c{n}')"));
    }
    q(c, "COMMIT");
}

// Writes while the copy builds its index: every kind of change to every kind of table.
fn writes(c: &Connection) {
    q(c, "BEGIN");
    for n in (0..600).step_by(20) {
        q(c, &format!("UPDATE articles SET body='rewritten{n} shared', n={} WHERE id=type::record('articles','a{n:04}')", n + 5000));
    }
    for n in (5..600).step_by(40) {
        q(
            c,
            &format!("DELETE FROM articles WHERE id=type::record('articles','a{n:04}')"),
        );
    }
    for n in 600..630 {
        q(c, &format!("INSERT INTO articles {{id:type::record('articles','a{n:04}'),n:{n},title:'title new',body:'fresh{n} shared'}}"));
    }
    // A document deleted and created again under the same id gets a new rowid.
    q(c, "DELETE FROM articles WHERE id=articles:a0001");
    q(
        c,
        "INSERT INTO articles {id:articles:a0001,n:1,title:'title back',body:'returned shared'}",
    );
    q(c, "UPDATE notes SET v=v+100 WHERE v < 10");
    q(c, "DELETE FROM notes WHERE v > 45");
    q(c, "INSERT INTO notes {id:notes:extra,v:999}");
    q(c, "UPDATE plain SET v='changed' WHERE id < 5");
    q(c, "DELETE FROM plain WHERE id > 45");
    q(c, "INSERT INTO plain VALUES (1000, 'late')");
    q(c, "DELETE FROM logs WHERE v = 3");
    q(c, "INSERT INTO logs VALUES ('late', 7)");
    q(c, "INSERT INTO counted(v) VALUES ('late1')");
    q(c, "INSERT INTO counted(v) VALUES ('late2')");
    q(c, "DELETE FROM counted WHERE v = 'late2'");
    q(c, "COMMIT");
}

#[test]
fn a_copy_with_a_new_index_takes_every_write_made_while_it_built() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b, c) = (
        dir.path().join("a.db"),
        dir.path().join("b.db"),
        dir.path().join("c.db"),
    );
    let source_db = Database::open(a.to_str().unwrap()).unwrap();
    let source = source_db.connect().unwrap();
    seed(&source);
    copy(&source, &a, &b);
    let copy_db = Database::open(b.to_str().unwrap()).unwrap();
    let built = copy_db.connect().unwrap();
    // A job builds on its copy with batch commits, as Cloud does.
    built.set_offline_build(true);
    q(
        &built,
        "CREATE SEARCH INDEX articles_body ON articles(body) USING FULLTEXT",
    );
    built.set_offline_build(false);
    writes(&source);

    let report = built.catch_up_from(&source, "articles_body").unwrap();
    assert!(report.tables >= 6, "{report:?}");
    assert_eq!(report.documents, 30 + 30 + 1, "{report:?}");

    for sql in [
        "SELECT * FROM articles ORDER BY id",
        "SELECT * FROM notes ORDER BY id",
        "SELECT * FROM plain ORDER BY id",
        "SELECT * FROM logs ORDER BY k, v",
        "SELECT * FROM counted ORDER BY id",
    ] {
        assert_eq!(rows(&built, sql), rows(&source, sql), "{sql}");
    }
    // The new index finds what an index built on the database now would find.
    copy(&source, &a, &c);
    let reference_db = Database::open(c.to_str().unwrap()).unwrap();
    let reference = reference_db.connect().unwrap();
    q(
        &reference,
        "CREATE SEARCH INDEX articles_body ON articles(body) USING FULLTEXT",
    );
    for word in [
        "shared",
        "rewritten20",
        "fresh601",
        "returned",
        "body3",
        "body5",
    ] {
        let sql = format!("SELECT id FROM search::text('articles_body','{word}',10000)");
        assert_eq!(ids(&built, &sql), ids(&reference, &sql), "{word}");
    }
    assert_eq!(
        ids(
            &built,
            "SELECT id FROM search::text('articles_body','shared',10000)"
        )
        .len(),
        600 - 15 + 30
    );
    // The indexes that already existed were copied with their tables, and still match.
    for sql in [
        "SELECT id FROM search::text('articles_title','common',10000)",
        "SELECT id FROM search::text('articles_title','new',10000)",
        "SELECT id FROM articles WHERE n > 4999",
    ] {
        assert_eq!(ids(&built, sql), ids(&source, sql), "{sql}");
    }
    for table in ["articles", "notes"] {
        let mine = built
            .check_collection_integrity(table, Default::default())
            .unwrap();
        let theirs = source
            .check_collection_integrity(table, Default::default())
            .unwrap();
        assert_eq!(mine.documents, theirs.documents, "{table}");
        assert_eq!(
            mine.index_entries,
            reference
                .check_collection_integrity(table, Default::default())
                .unwrap()
                .index_entries,
            "{table}"
        );
    }
    // The AUTOINCREMENT counter came along: the next row takes the same id in both.
    q(&built, "INSERT INTO counted(v) VALUES ('next')");
    q(&source, "INSERT INTO counted(v) VALUES ('next')");
    assert_eq!(
        rows(&built, "SELECT id FROM counted WHERE v = 'next'"),
        rows(&source, "SELECT id FROM counted WHERE v = 'next'")
    );
    // A second catch-up finds nothing left to copy.
    q(&built, "DELETE FROM counted WHERE v = 'next'");
    q(&source, "DELETE FROM counted WHERE v = 'next'");
    let again = built.catch_up_from(&source, "articles_body").unwrap();
    assert_eq!(
        (again.rows_deleted, again.rows_inserted),
        (0, 0),
        "{again:?}"
    );
}

#[test]
fn a_schema_or_catalog_change_during_the_build_is_refused() {
    for change in [
        "CREATE TABLE later(v TEXT)",
        "CREATE TABLE more_docs",
        "CREATE INDEX notes_v ON notes(v)",
        "CREATE INDEX plain_v ON plain(v)",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.db"), dir.path().join("b.db"));
        let source_db = Database::open(a.to_str().unwrap()).unwrap();
        let source = source_db.connect().unwrap();
        seed(&source);
        copy(&source, &a, &b);
        let copy_db = Database::open(b.to_str().unwrap()).unwrap();
        let built = copy_db.connect().unwrap();
        q(
            &built,
            "CREATE SEARCH INDEX articles_body ON articles(body) USING FULLTEXT",
        );
        q(&source, change);
        let before = rows(&built, "SELECT count(*) FROM articles");
        let error = built.catch_up_from(&source, "articles_body").unwrap_err();
        assert!(
            error.to_string().contains("changed during the index build"),
            "{change}: {error}"
        );
        assert_eq!(
            rows(&built, "SELECT count(*) FROM articles"),
            before,
            "{change}"
        );
    }
}

#[test]
fn an_index_on_a_plain_table_follows_its_copied_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a.db"), dir.path().join("b.db"));
    let source_db = Database::open(a.to_str().unwrap()).unwrap();
    let source = source_db.connect().unwrap();
    seed(&source);
    copy(&source, &a, &b);
    let copy_db = Database::open(b.to_str().unwrap()).unwrap();
    let built = copy_db.connect().unwrap();
    q(&built, "CREATE INDEX plain_v ON plain(v)");
    writes(&source);
    let report = built.catch_up_from(&source, "plain_v").unwrap();
    assert_eq!(report.documents, 0, "{report:?}");
    for sql in [
        "SELECT * FROM plain ORDER BY id",
        "SELECT id FROM plain WHERE v = 'changed' ORDER BY id",
        "SELECT * FROM articles ORDER BY id",
    ] {
        assert_eq!(rows(&built, sql), rows(&source, sql), "{sql}");
    }
    let plan = rows(
        &built,
        "EXPLAIN QUERY PLAN SELECT id FROM plain WHERE v = 'changed'",
    );
    assert!(format!("{plan:?}").contains("plain_v"), "{plan:?}");
    assert!(built.catch_up_from(&source, "missing_index").is_err());
}

#[test]
fn a_collection_with_no_index_before_takes_its_first() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a.db"), dir.path().join("b.db"));
    let source_db = Database::open(a.to_str().unwrap()).unwrap();
    let source = source_db.connect().unwrap();
    seed(&source);
    copy(&source, &a, &b);
    let copy_db = Database::open(b.to_str().unwrap()).unwrap();
    let built = copy_db.connect().unwrap();
    built.set_offline_build(true);
    q(
        &built,
        "CREATE SEARCH INDEX notes_v ON notes(label) USING FULLTEXT",
    );
    built.set_offline_build(false);
    q(
        &source,
        "INSERT INTO notes {id:notes:labelled,v:1,label:'found here'}",
    );
    let report = built.catch_up_from(&source, "notes_v").unwrap();
    assert_eq!(report.documents, 1, "{report:?}");
    assert_eq!(
        ids(&built, "SELECT id FROM search::text('notes_v','found',10)"),
        vec![format!(
            "{:?}",
            rows(&source, "SELECT id FROM notes WHERE id=notes:labelled")[0][0]
        )]
    );
}
