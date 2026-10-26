//! Public frontend qualification for the Turso 0.8.1 integration.
use romedb::{Connection, Database, Parameters, QueryResult, Value};

fn q(c: &Connection, sql: &str) -> QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

#[test]
fn caller_managed_mvcc_bootstraps_and_recovers_its_published_log() {
    use std::sync::Arc;
    use turso_core::{
        io::UnixIO,
        mvcc::persistent_storage::{DurableStorage, Storage},
        OpenFlags, IO,
    };
    fn open(path: &std::path::Path) -> (Database, Arc<Storage>) {
        let io: Arc<dyn IO> = Arc::new(UnixIO::new().unwrap());
        let file = io
            .open_file(
                &format!("{}-log", path.display()),
                OpenFlags::default(),
                false,
            )
            .unwrap();
        let storage = Arc::new(Storage::new(file, io.clone(), None));
        let db =
            Database::open_with_manual_mvcc_and_io(path.to_str().unwrap(), io, storage.clone())
                .unwrap();
        (db, storage)
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("managed.db");
    let (db, storage) = open(&path);
    let c = db.connect().unwrap();
    assert_eq!(storage.checkpoint_threshold(), -1);
    q(&c, "CREATE TABLE articles");
    q(&c, "PRAGMA wal_checkpoint(TRUNCATE)");
    for invalid in ["", "bad\0tag", &"x".repeat(129)] {
        assert!(c.set_commit_tag(Some(invalid)).is_err());
    }
    c.set_commit_tag(Some("request-proof")).unwrap();
    q(&c, "INSERT INTO articles {id:articles:one,title:'durable'}");
    let checkpoint = std::fs::read(&path).unwrap();
    let log = std::fs::read(format!("{}-log", path.display())).unwrap();
    assert_eq!(checkpoint[18..20], [255, 255]);
    assert_eq!(storage.logical_log_offset(), log.len() as u64);
    assert!(log
        .windows(b"request-proof".len())
        .any(|window| window == b"request-proof"));
    c.set_commit_tag(None).unwrap();

    let recovered = dir.path().join("recovered.db");
    std::fs::write(&recovered, checkpoint).unwrap();
    std::fs::write(format!("{}-log", recovered.display()), &log).unwrap();
    let (restored, restored_storage) = open(&recovered);
    assert_eq!(restored_storage.logical_log_offset(), log.len() as u64);
    assert_eq!(
        q(&restored.connect().unwrap(), "SELECT title FROM articles").rows,
        vec![vec![Value::String("durable".into())]]
    );
}

#[test]
fn concurrent_document_writers_share_transactional_fulltext() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concurrent.db");
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let a = db.connect().unwrap();
    q(&a, "PRAGMA journal_mode=mvcc");
    q(&a, "CREATE TABLE articles");
    q(
        &a,
        "CREATE SEARCH INDEX articles_text ON articles(title) USING FULLTEXT",
    );
    q(
        &a,
        "INSERT INTO articles {id:articles:seed,title:'common seed'}",
    );
    let b = db.connect().unwrap();
    let reader = db.connect().unwrap();
    q(&reader, "BEGIN CONCURRENT");
    assert_eq!(
        q(
            &reader,
            "SELECT id FROM search::text('articles_text','common',100)"
        )
        .rows
        .len(),
        1
    );
    q(&a, "BEGIN CONCURRENT");
    q(&b, "BEGIN CONCURRENT");
    q(
        &a,
        "INSERT INTO articles {id:articles:a,title:'common alpha'}",
    );
    q(
        &b,
        "INSERT INTO articles {id:articles:b,title:'common beta'}",
    );
    assert_eq!(
        q(
            &a,
            "SELECT id FROM search::text('articles_text','common',100)"
        )
        .rows
        .len(),
        2
    );
    assert_eq!(
        q(
            &b,
            "SELECT id FROM search::text('articles_text','common',100)"
        )
        .rows
        .len(),
        2
    );
    q(&a, "COMMIT");
    q(&b, "COMMIT");
    assert_eq!(
        q(
            &reader,
            "SELECT id FROM search::text('articles_text','common',100)"
        )
        .rows
        .len(),
        1
    );
    q(&reader, "COMMIT");
    assert_eq!(
        q(
            &reader,
            "SELECT id FROM search::text('articles_text','common',100)"
        )
        .rows
        .len(),
        3
    );
    q(&a, "BEGIN CONCURRENT");
    q(
        &a,
        "UPDATE articles SET title='removed' WHERE id=articles:a",
    );
    q(&a, "ROLLBACK");
    assert_eq!(
        q(
            &reader,
            "SELECT id FROM search::text('articles_text','alpha',100)"
        )
        .rows
        .len(),
        1
    );
    a.check_collection_integrity("articles", Default::default())
        .unwrap();
    drop(reader);
    drop(b);
    drop(a);
    drop(db);
    let reopened = Database::open(path.to_str().unwrap()).unwrap();
    let c = reopened.connect().unwrap();
    assert_eq!(
        q(
            &c,
            "SELECT id FROM search::text('articles_text','common',100)"
        )
        .rows
        .len(),
        3
    );
}

#[test]
fn release_sql_features_and_json_plans_reach_native_optimizer() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    assert_eq!(q(&c, "WITH RECURSIVE nums(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM nums WHERE n<5) SELECT sum(n) FROM nums").rows, vec![vec![Value::Integer(15)]]);
    q(&c, "CREATE TABLE numbers(n INTEGER, label TEXT)");
    q(&c, "INSERT INTO numbers VALUES(1,'a'),(2,'b'),(3,'c')");
    assert_eq!(q(&c, "SELECT n, lag(n,1,0) OVER (ORDER BY n), lead(n,1,0) OVER (ORDER BY n), rank() OVER (ORDER BY n), sum(n) OVER (ORDER BY n GROUPS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW EXCLUDE CURRENT ROW) FROM numbers ORDER BY n").rows, vec![
        vec![Value::Integer(1),Value::Integer(0),Value::Integer(2),Value::Integer(1),Value::Null],
        vec![Value::Integer(2),Value::Integer(1),Value::Integer(3),Value::Integer(2),Value::Integer(1)],
        vec![Value::Integer(3),Value::Integer(2),Value::Integer(0),Value::Integer(3),Value::Integer(3)],
    ]);
    q(&c, "CREATE INDEX numbers_n ON numbers(n NULLS LAST)");
    let plan = q(
        &c,
        "EXPLAIN QUERY PLAN FORMAT=JSON SELECT n FROM numbers WHERE n=2",
    );
    let Value::String(json) = &plan.rows[0][0] else {
        panic!("JSON plan expected")
    };
    assert!(json.contains("numbers_n"), "{json}");
    assert_eq!(
        q(&c, "SELECT n FROM numbers WHERE n=2").rows,
        vec![vec![Value::Integer(2)]]
    );
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE INDEX docs_n ON docs(n)");
    q(&c, "INSERT INTO docs {id:docs:one,n:2}");
    let plan = q(
        &c,
        "EXPLAIN QUERY PLAN FORMAT=JSON SELECT n FROM docs WHERE n=2",
    );
    assert!(matches!(&plan.rows[0][0], Value::String(json) if json.contains("docs_n")));
}

#[test]
fn released_21_database_fts_upgrade_is_explicit_atomic_and_persistent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    std::fs::write(&path, include_bytes!("../fixtures/turso-072-fts.db")).unwrap();
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        q(&c, "SELECT title FROM articles").rows,
        vec![vec![Value::String("legacy searchable text".into())]]
    );
    let search = "SELECT id FROM search::text('articles_text','legacy',10)";
    let error = c.execute(search, &Parameters::new()).unwrap_err();
    assert!(error.to_string().contains("REINDEX"), "{error}");
    q(&c, "BEGIN");
    q(&c, "UPDATE accounts SET balance=101 WHERE id=1");
    for sql in [
        "INSERT INTO articles {id:articles:blocked,title:'legacy blocked'}",
        "UPDATE articles SET title='legacy blocked'",
        "DELETE FROM articles",
    ] {
        let error = c.execute(sql, &Parameters::new()).unwrap_err();
        assert!(error.to_string().contains("REINDEX"), "{sql}: {error}");
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(
            q(&c, "SELECT balance FROM accounts WHERE id=1").rows,
            vec![vec![Value::Integer(101)]]
        );
        assert_eq!(
            q(&c, "SELECT title FROM articles").rows,
            vec![vec![Value::String("legacy searchable text".into())]]
        );
    }
    q(&c, "REINDEX articles_text");
    assert_eq!(q(&c, search).rows.len(), 1);
    q(&c, "ROLLBACK");
    assert!(c
        .execute(search, &Parameters::new())
        .unwrap_err()
        .to_string()
        .contains("REINDEX"));
    q(&c, "REINDEX articles_text");
    q(
        &c,
        "INSERT INTO articles {id:articles:new,title:'legacy second'}",
    );
    assert_eq!(q(&c, search).rows.len(), 2);
    c.check_collection_integrity("articles", Default::default())
        .unwrap();
    q(&c, "PRAGMA wal_checkpoint(TRUNCATE)");
    drop(c);
    drop(db);
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(q(&c, search).rows.len(), 2);
    assert_eq!(
        q(&c, "SELECT balance FROM accounts WHERE id=1").rows,
        vec![vec![Value::Integer(100)]]
    );
}

#[test]
fn removed_dml_limits_reject_before_mutation_and_preserve_transactions() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE native(n INTEGER)");
    q(&c, "CREATE TABLE docs");
    for table in ["native", "docs"] {
        q(&c, &format!("INSERT INTO {table}(n) VALUES(1),(2),(3)"));
        q(&c, "BEGIN");
        q(&c, &format!("INSERT INTO {table}(n) VALUES(4)"));
        for limit in ["0", "1", "1 OFFSET 1", "-1", "NULL", "1.5", "'1'", "$limit"] {
            for write in [
                format!("UPDATE {table} SET n=99"),
                format!("DELETE FROM {table}"),
            ] {
                let sql = format!("{write} RETURNING n LIMIT {limit}");
                let error = c.execute(&sql, &Parameters::new()).unwrap_err();
                assert!(error.to_string().contains("LIMIT"), "{sql}: {error}");
                assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
            }
        }
        assert_eq!(
            q(&c, &format!("SELECT count(*) FROM {table} WHERE n<5")).rows,
            vec![vec![Value::Integer(4)]]
        );
        q(&c, "ROLLBACK");
    }
}

#[test]
fn simultaneous_threads_commit_disjoint_fulltext_documents() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("threads.db").to_str().unwrap()).unwrap();
    let setup = db.connect().unwrap();
    q(&setup, "PRAGMA journal_mode=mvcc");
    q(&setup, "CREATE TABLE articles");
    q(
        &setup,
        "CREATE SEARCH INDEX articles_text ON articles(title) USING FULLTEXT",
    );
    let connections: Vec<_> = (0..4).map(|_| db.connect().unwrap()).collect();
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        for (id, c) in connections.into_iter().enumerate() {
            let barrier = &barrier;
            scope.spawn(move || {
                q(&c, "BEGIN CONCURRENT");
                let inserted = c.execute(&format!("INSERT INTO articles {{id:type::record('articles',{id}),title:'parallel commit'}}"), &Parameters::new());
                barrier.wait();
                inserted.unwrap();
                q(&c, "COMMIT");
            });
        }
    });
    assert_eq!(
        q(
            &setup,
            "SELECT id FROM search::text('articles_text','parallel',10)"
        )
        .rows
        .len(),
        4
    );
    setup
        .check_collection_integrity("articles", Default::default())
        .unwrap();
}

#[test]
fn conflicting_document_update_is_reported_and_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("conflict.db").to_str().unwrap()).unwrap();
    let a = db.connect().unwrap();
    q(&a, "PRAGMA journal_mode=mvcc");
    q(&a, "CREATE TABLE articles");
    q(
        &a,
        "CREATE SEARCH INDEX articles_text ON articles(title) USING FULLTEXT",
    );
    q(
        &a,
        "INSERT INTO articles {id:articles:one,title:'original'}",
    );
    let b = db.connect().unwrap();
    q(&a, "BEGIN CONCURRENT");
    q(&b, "BEGIN CONCURRENT");
    q(&b, "SELECT title FROM articles WHERE id=articles:one");
    q(
        &a,
        "UPDATE articles SET title='winner' WHERE id=articles:one",
    );
    q(&a, "COMMIT");
    let error = b
        .execute(
            "UPDATE articles SET title='retry' WHERE id=articles:one",
            &Parameters::new(),
        )
        .unwrap_err();
    assert_eq!(error.code(), "FDB_WRITE_CONFLICT", "{error}");
    if b.transaction_state() == romedb::TransactionState::Active {
        q(&b, "ROLLBACK");
    }
    assert_eq!(
        q(
            &a,
            "SELECT id FROM search::text('articles_text','winner',10)"
        )
        .rows
        .len(),
        1
    );
    q(&b, "BEGIN CONCURRENT");
    q(
        &b,
        "UPDATE articles SET title='retry' WHERE id=articles:one",
    );
    q(&b, "COMMIT");
    assert_eq!(
        q(
            &a,
            "SELECT id FROM search::text('articles_text','retry',10)"
        )
        .rows
        .len(),
        1
    );
    assert!(q(
        &a,
        "SELECT id FROM search::text('articles_text','winner',10)"
    )
    .rows
    .is_empty());
    a.check_collection_integrity("articles", Default::default())
        .unwrap();
}
