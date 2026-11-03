use romedb::{CancellationToken, Database, Document, IntegrityLimits, Parameters, Value};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

fn q(c: &romedb::Connection, sql: &str) -> Vec<Vec<Value>> {
    c.execute(sql, &Parameters::new()).expect(sql).rows
}

fn create(path: &Path) -> Database {
    std::fs::File::create(path).unwrap();
    Database::create_mvcc(path.to_str().unwrap()).unwrap()
}

fn fill(c: &romedb::Connection, count: i64) {
    let body = Value::String("x".repeat(2000));
    q(c, "BEGIN");
    for n in 0..count {
        c.insert(
            "posts",
            Document::from([
                ("n".into(), Value::Integer(n)),
                ("body".into(), body.clone()),
            ]),
        )
        .unwrap();
    }
    q(c, "COMMIT");
}

fn names(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn exported_file_opens_alone_with_the_same_rows_indexes_and_service_state() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("site.romedb");
    let database = create(&source);
    let c = database.connect().unwrap();
    let catalog = c.initialize_service_catalog("site-1").unwrap();
    for sql in [
        "CREATE TABLE posts",
        "DEFINE FIELD title ON posts TYPE string",
        "CREATE UNIQUE INDEX posts_slug ON posts(slug)",
        "CREATE SEARCH INDEX posts_tags ON posts(tags) USING ARRAY",
        "CREATE SEARCH INDEX posts_text ON posts(title,body) USING FULLTEXT",
        "CREATE SEARCH INDEX posts_vec ON posts(v) USING VECTOR WITH (metric='l2',dimensions=2)",
        "CREATE FUNCTION app::answer() RETURNS integer LANGUAGE JAVASCRIPT AS 'return 42n'",
        "CREATE TABLE settings",
        "INSERT INTO settings {name:'theme', value:{dark:true, accent:'#123456'}}",
    ] {
        q(&c, sql);
    }
    for n in 0..300 {
        c.insert(
            "posts",
            Document::from([
                ("slug".into(), Value::String(format!("post-{n}"))),
                ("title".into(), Value::String(format!("Post {n}"))),
                (
                    "body".into(),
                    Value::String(format!("shared words and number {n}")),
                ),
                (
                    "tags".into(),
                    Value::Array(vec![
                        Value::String("all".into()),
                        Value::String(format!("t{}", n % 7)),
                    ]),
                ),
                ("v".into(), Value::vector32(&[n as f32, 1.0]).unwrap()),
                ("bytes".into(), Value::Binary(vec![n as u8, 0, 255])),
            ]),
        )
        .unwrap();
    }
    q(&c, "BEGIN CONCURRENT");
    c.publish_service_receipt(
        catalog.sequence,
        "request-1",
        &"a".repeat(64),
        &format!(
            r#"{{"requestId":"request-1","sequence":{}}}"#,
            catalog.sequence + 1
        ),
    )
    .unwrap();
    q(&c, "COMMIT");
    let destination = directory.path().join("copy.romedb");
    let export = database
        .start_file_export(&destination, &CancellationToken::new())
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(export.bytes, std::fs::metadata(&destination).unwrap().len());
    assert_eq!(
        names(directory.path()),
        [
            "copy.romedb",
            "site.db-log",
            "site.romedb",
            "site.romedb-wal"
        ]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = std::fs::metadata(&destination).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 1);
    }

    let copy = Database::open_mvcc(destination.to_str().unwrap()).unwrap();
    let k = copy.connect().unwrap();
    for sql in [
        "SELECT * FROM posts ORDER BY id",
        "SELECT * FROM settings ORDER BY id",
        "SELECT id FROM posts WHERE slug='post-42'",
        "SELECT id FROM posts WHERE array::contains(tags,'t3') ORDER BY id",
        "SELECT id,score FROM search::text('posts_text','number 17',3) ORDER BY score DESC,id",
        "SELECT app::answer()",
    ] {
        assert_eq!(q(&k, sql), q(&c, sql), "{sql}");
    }
    assert_eq!(
        q(&k, "SELECT count() FROM posts"),
        vec![vec![Value::Integer(300)]]
    );
    assert_eq!(
        q(&k, "PRAGMA journal_mode"),
        vec![vec![Value::String("mvcc".into())]]
    );
    assert_eq!(
        q(&k, "PRAGMA integrity_check"),
        vec![vec![Value::String("ok".into())]]
    );
    k.check_collection_integrity("posts", IntegrityLimits::default())
        .unwrap();
    let copied = k.service_catalog().unwrap();
    assert_eq!(copied.database_id, "site-1");
    assert_eq!(copied.sequence, catalog.sequence + 1);
    assert!(k.service_receipt("request-1").unwrap().is_some());
    assert!(k
        .execute("INSERT INTO posts {slug:'post-42'}", &Parameters::new())
        .is_err());
}

#[test]
fn export_refuses_an_existing_destination_or_sidecar_and_leaves_nothing_behind() {
    let directory = tempfile::tempdir().unwrap();
    let database = create(&directory.path().join("site.romedb"));
    q(&database.connect().unwrap(), "CREATE TABLE posts");
    for name in ["taken.romedb", "stale.db-log", "stale.romedb-wal"] {
        std::fs::write(directory.path().join(name), b"keep").unwrap();
    }
    let before = names(directory.path());
    for name in ["taken.romedb", "stale.romedb"] {
        let error = database
            .start_file_export(&directory.path().join(name), &CancellationToken::new())
            .err()
            .unwrap();
        assert_eq!(error.code(), "FDB_ALREADY_EXISTS", "{name}: {error}");
    }
    let error = database
        .start_file_export(
            &directory.path().join("missing").join("copy.romedb"),
            &CancellationToken::new(),
        )
        .err()
        .unwrap();
    assert_eq!(error.code(), "FDB_NOT_FOUND", "{error}");
    let error = database
        .start_file_export(
            &directory.path().join("it's.romedb"),
            &CancellationToken::new(),
        )
        .unwrap()
        .finish();
    assert!(
        error.is_ok(),
        "the folder path has no quote: {:?}",
        error.err()
    );
    std::fs::remove_file(directory.path().join("it's.romedb")).unwrap();
    assert_eq!(names(directory.path()), before);
    assert_eq!(
        std::fs::read(directory.path().join("taken.romedb")).unwrap(),
        b"keep"
    );
}

#[test]
fn snapshot_export_returns_an_open_file_and_leaves_no_name() {
    let directory = tempfile::tempdir().unwrap();
    let database = create(&directory.path().join("site.romedb"));
    let c = database.connect().unwrap();
    q(&c, "CREATE TABLE posts");
    q(&c, "INSERT INTO posts {title:'one'}");
    let spool = directory.path().join("spool");
    std::fs::create_dir(&spool).unwrap();
    let mut export = database
        .start_file_snapshot(&spool, &CancellationToken::new())
        .unwrap()
        .finish()
        .unwrap();
    assert!(names(&spool).is_empty());
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut export.file, &mut bytes).unwrap();
    assert_eq!(bytes.len() as u64, export.bytes);
    let restored = directory.path().join("restored.romedb");
    std::fs::write(&restored, &bytes).unwrap();
    let copy = Database::open_mvcc(restored.to_str().unwrap()).unwrap();
    assert_eq!(
        q(&copy.connect().unwrap(), "SELECT title FROM posts"),
        vec![vec![Value::String("one".into())]]
    );
}

#[test]
fn writers_keep_committing_during_an_export_and_the_file_holds_the_commits_before_it_started() {
    let directory = tempfile::tempdir().unwrap();
    let database = Arc::new(create(&directory.path().join("site.romedb")));
    let c = database.connect().unwrap();
    q(&c, "PRAGMA mvcc_checkpoint_threshold=65536");
    q(&c, "CREATE TABLE posts");
    q(&c, "CREATE INDEX posts_n ON posts(n)");
    fill(&c, 10_000);
    let commits = Arc::new(Mutex::new(10_000u64));
    let stop = Arc::new(AtomicBool::new(false));
    let during = Arc::new(AtomicU64::new(0));
    let writer = std::thread::spawn({
        let (database, commits, stop) = (database.clone(), commits.clone(), stop.clone());
        move || {
            let w = database.connect().unwrap();
            let body = Value::String("x".repeat(2000));
            while !stop.load(Ordering::SeqCst) {
                let mut committed = commits.lock().unwrap();
                let row = Document::from([
                    ("n".into(), Value::Integer(*committed as i64)),
                    ("body".into(), body.clone()),
                ]);
                match w.insert("posts", row) {
                    Ok(_) => {}
                    Err(error) if error.code() == "FDB_BUSY" => continue,
                    Err(error) => panic!("{error}"),
                }
                *committed += 1;
            }
        }
    });
    while *commits.lock().unwrap() < 10_050 {
        std::thread::yield_now();
    }
    let destination = directory.path().join("copy.romedb");
    let (pending, expected) = {
        let committed = commits.lock().unwrap();
        let pending = database
            .start_file_export(&destination, &CancellationToken::new())
            .unwrap();
        (pending, *committed)
    };
    let reader = database.connect().unwrap();
    let watcher = std::thread::spawn({
        let (commits, during) = (commits.clone(), during.clone());
        move || {
            let first = *commits.lock().unwrap();
            let export = pending.finish();
            during.store(*commits.lock().unwrap() - first, Ordering::SeqCst);
            export
        }
    });
    let mut reads = 0;
    while !watcher.is_finished() {
        match reader.execute("SELECT n FROM posts WHERE n=7", &Parameters::new()) {
            Ok(result) => assert_eq!(result.rows, vec![vec![Value::Integer(7)]]),
            Err(error) if error.code() == "FDB_BUSY" => continue,
            Err(error) => panic!("{error}"),
        }
        reads += 1;
    }
    let export = watcher.join().unwrap().unwrap();
    stop.store(true, Ordering::SeqCst);
    writer.join().unwrap();
    assert!(
        during.load(Ordering::SeqCst) > 0,
        "no write committed during the export"
    );
    assert!(reads > 0);
    let copy = Database::open_mvcc(destination.to_str().unwrap()).unwrap();
    let k = copy.connect().unwrap();
    assert_eq!(
        q(&k, "SELECT count(), max(n) FROM posts"),
        vec![vec![
            Value::Integer(expected as i64),
            Value::Integer(expected as i64 - 1)
        ]]
    );
    assert_eq!(
        q(&k, "PRAGMA integrity_check"),
        vec![vec![Value::String("ok".into())]]
    );
    assert!(export.bytes > 20_000_000);
}

#[test]
fn schema_changes_during_an_export_leave_the_copy_at_its_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let database = Arc::new(create(&directory.path().join("site.romedb")));
    let c = database.connect().unwrap();
    q(&c, "CREATE TABLE posts");
    fill(&c, 5_000);
    let rounds = Arc::new(Mutex::new(0u64));
    let stop = Arc::new(AtomicBool::new(false));
    let changer = std::thread::spawn({
        let (database, rounds, stop) = (database.clone(), rounds.clone(), stop.clone());
        move || {
            let d = database.connect().unwrap();
            while !stop.load(Ordering::SeqCst) {
                let mut round = rounds.lock().unwrap();
                let i = *round;
                for sql in [
                    format!("CREATE TABLE t{i}"),
                    format!("INSERT INTO t{i} {{a:{i}}}"),
                    format!("CREATE INDEX t{i}_a ON t{i}(a)"),
                    format!("DROP TABLE IF EXISTS t{}", i.wrapping_sub(2)),
                ] {
                    q(&d, &sql);
                }
                *round += 1;
            }
        }
    });
    while *rounds.lock().unwrap() < 3 {
        std::thread::yield_now();
    }
    let destination = directory.path().join("copy.romedb");
    let (pending, at) = {
        let round = rounds.lock().unwrap();
        let pending = database
            .start_file_export(&destination, &CancellationToken::new())
            .unwrap();
        (pending, *round)
    };
    pending.finish().unwrap();
    let after = *rounds.lock().unwrap();
    stop.store(true, Ordering::SeqCst);
    changer.join().unwrap();
    assert!(after > at, "no schema change ran during the export");
    let k = Database::open_mvcc(destination.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    for i in at - 2..at {
        assert_eq!(
            q(&k, &format!("SELECT a FROM t{i}")),
            vec![vec![Value::Integer(i as i64)]]
        );
    }
    for i in [at - 3, at] {
        assert!(k
            .execute(&format!("SELECT a FROM t{i}"), &Parameters::new())
            .is_err());
    }
    assert_eq!(
        q(&k, "PRAGMA integrity_check"),
        vec![vec![Value::String("ok".into())]]
    );
}

#[test]
fn cancelled_export_stops_and_removes_its_folder() {
    let directory = tempfile::tempdir().unwrap();
    let database = create(&directory.path().join("site.romedb"));
    let c = database.connect().unwrap();
    q(&c, "CREATE TABLE posts");
    fill(&c, 10_000);
    let before = names(directory.path());
    let token = CancellationToken::new();
    let pending = database
        .start_file_export(&directory.path().join("copy.romedb"), &token)
        .unwrap();
    token.cancel();
    let error = pending.finish().err().unwrap();
    assert_eq!(error.code(), "FDB_CANCELLED", "{error}");
    assert_eq!(names(directory.path()), before);
    let token = CancellationToken::new();
    token.cancel();
    let error = database
        .start_file_export(&directory.path().join("copy.romedb"), &token)
        .err()
        .unwrap();
    assert_eq!(error.code(), "FDB_CANCELLED");
    assert_eq!(names(directory.path()), before);
}

#[test]
fn export_needs_an_mvcc_database() {
    let directory = tempfile::tempdir().unwrap();
    let database = Database::open(directory.path().join("wal.romedb").to_str().unwrap()).unwrap();
    q(&database.connect().unwrap(), "CREATE TABLE posts");
    let error = database
        .start_file_export(
            &directory.path().join("copy.romedb"),
            &CancellationToken::new(),
        )
        .err()
        .unwrap();
    assert_eq!(error.code(), "FDB_UNSUPPORTED");
    assert!(error.to_string().contains("migrate-to-mvcc"), "{error}");
}
