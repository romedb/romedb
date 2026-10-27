use romedb_service::DatabaseService;
use serde_json::{json, Value};
use std::sync::{Arc, Barrier};

fn request(id: &str, after: u64, statements: &[&str]) -> String {
    json!({"requestId":id,"afterSequence":after,"statements":statements.iter().map(|sql| json!({"sql":sql})).collect::<Vec<_>>()}).to_string()
}

fn query(service: &DatabaseService, id: &str, sql: &[&str]) -> Value {
    let after = service.info().unwrap()["sequence"].as_u64().unwrap();
    serde_json::from_str(&service.query(&request(id, after, sql)).unwrap()).unwrap()
}

fn read(service: &DatabaseService, sql: &str) -> Value {
    let mut body: Value = serde_json::from_str(&request("read", 0, &[sql])).unwrap();
    body["readVersion"] = json!(2);
    serde_json::from_str(&service.read(&body.to_string()).unwrap()).unwrap()
}

#[test]
fn every_request_enforces_foreign_keys_and_replays_atomic_failure() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatabaseService::create(&directory.path().join("app.romedb")).unwrap();
    let setup = query(
        &service,
        "fk-setup",
        &[
            "CREATE TABLE parents (id INTEGER PRIMARY KEY)",
            "CREATE TABLE children (id INTEGER PRIMARY KEY, parent INTEGER REFERENCES parents(id))",
            "INSERT INTO parents VALUES (1)",
            "INSERT INTO children VALUES (1,1)",
        ],
    );
    assert!(setup["error"].is_null(), "{setup}");
    let body = request(
        "fk-fail",
        1,
        &[
            "INSERT INTO parents VALUES (2)",
            "INSERT INTO children VALUES (2,999)",
        ],
    );
    let response = service.query(&body).unwrap();
    let failure: Value = serde_json::from_str(&response).unwrap();
    assert!(failure["error"]["code"].is_string(), "{failure}");
    assert_eq!(service.query(&body).unwrap(), response);
    assert_eq!(
        read(&service, "SELECT id FROM parents ORDER BY id")["results"][0]["rows"],
        json!([[{"type":"Integer","value":"1"}]])
    );
    let deletion = query(&service, "fk-delete", &["DELETE FROM parents WHERE id=1"]);
    assert!(deletion["error"]["code"].is_string(), "{deletion}");
}

#[test]
fn receipts_and_database_identity_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("app.romedb");
    let service = DatabaseService::create(&path).unwrap();
    let identity = service.info().unwrap()["id"].clone();
    let body = request(
        "create",
        0,
        &[
            "CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT)",
            "INSERT INTO items VALUES (1,'original')",
        ],
    );
    let response = service.query(&body).unwrap();
    assert!(
        serde_json::from_str::<Value>(&response).unwrap()["error"].is_null(),
        "{response}"
    );
    assert_eq!(service.query(&body).unwrap(), response);
    drop(service);
    let service = DatabaseService::open(&path).unwrap();
    assert_eq!(service.info().unwrap()["id"], identity);
    assert_eq!(service.info().unwrap()["sequence"], 1);
    assert_eq!(service.query(&body).unwrap(), response);
    let result = read(&service, "SELECT value FROM items");
    assert_eq!(
        result["results"][0]["rows"][0][0],
        json!({"type":"String","value":"original"})
    );
    assert_eq!(service.info().unwrap()["sequence"], 1);
}

#[test]
fn a_failed_atomic_request_rolls_back_and_replays_its_failure() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatabaseService::create(&directory.path().join("app.romedb")).unwrap();
    assert!(query(
        &service,
        "setup",
        &["CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT UNIQUE)"]
    )["error"]
        .is_null());
    let body = request(
        "fail",
        1,
        &[
            "INSERT INTO items VALUES (1,'same')",
            "INSERT INTO items VALUES (2,'same')",
        ],
    );
    let response = service.query(&body).unwrap();
    let failure: Value = serde_json::from_str(&response).unwrap();
    assert!(failure["error"]["code"].is_string(), "{failure}");
    assert_eq!(failure["sequence"], 2);
    assert_eq!(service.query(&body).unwrap(), response);
    assert_eq!(
        read(&service, "SELECT id FROM items")["results"][0]["rows"],
        json!([])
    );
}

#[test]
fn stale_edits_fail_atomically_and_changed_replay_bodies_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatabaseService::create(&directory.path().join("app.romedb")).unwrap();
    query(
        &service,
        "setup",
        &[
            "CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT)",
            "INSERT INTO items VALUES (1,'original')",
        ],
    );
    query(
        &service,
        "other",
        &["UPDATE items SET value='other' WHERE id=1"],
    );
    let mut body: Value = serde_json::from_str(&request(
        "edit",
        1,
        &["UPDATE items SET value='stale' WHERE id=1"],
    ))
    .unwrap();
    body["ifSequence"] = json!(1);
    let response: Value = serde_json::from_str(&service.query(&body.to_string()).unwrap()).unwrap();
    assert_eq!(response["error"]["code"], "FDB_CONFLICT");
    assert_eq!(
        read(&service, "SELECT value FROM items")["results"][0]["rows"][0][0]["value"],
        "other"
    );
    body["statements"][0]["sql"] = json!("DELETE FROM items");
    assert_eq!(
        service.query(&body.to_string()).unwrap_err().code,
        "FDB_CONFLICT"
    );
    assert_eq!(service.info().unwrap()["sequence"], 3);
}

#[test]
fn read_only_and_reserved_storage_rules_apply_before_execution() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatabaseService::create(&directory.path().join("app.romedb")).unwrap();
    for sql in [
        "CREATE TABLE forbidden;",
        "SELECT * FROM __romedb_service_state",
        "PRAGMA journal_mode",
        "BEGIN",
        "SELECT 1; SELECT 2",
        "SELECT readfile('secret')",
    ] {
        let mut body: Value = serde_json::from_str(&request("invalid", 0, &[sql])).unwrap();
        body["readVersion"] = json!(2);
        assert!(service.read(&body.to_string()).is_err(), "{sql}");
    }
    let metadata = read(&service, "INFO FOR DB").to_string();
    assert!(!metadata.contains("__romedb_service"), "{metadata}");
    assert_eq!(service.info().unwrap()["sequence"], 0);
}

#[test]
fn request_mutation_budgets_preserve_atomicity() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatabaseService::create(&directory.path().join("app.romedb")).unwrap();
    query(
        &service,
        "setup",
        &["CREATE TABLE items (id INTEGER PRIMARY KEY)"],
    );
    let mut body: Value = serde_json::from_str(&request(
        "limited",
        1,
        &["INSERT INTO items VALUES(1)", "INSERT INTO items VALUES(2)"],
    ))
    .unwrap();
    body["workLimits"] = json!({"rowMutations":1});
    let response: Value = serde_json::from_str(&service.query(&body.to_string()).unwrap()).unwrap();
    assert_eq!(response["error"]["code"], "FDB_LIMIT", "{response}");
    assert_eq!(
        read(&service, "SELECT id FROM items")["results"][0]["rows"],
        json!([])
    );
}

#[test]
fn wal_files_are_refused_without_modifying_the_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("app.romedb");
    {
        let database = romedb::Database::open(path.to_str().unwrap()).unwrap();
        database
            .connect()
            .unwrap()
            .execute("CREATE TABLE items;", &romedb::Parameters::new())
            .unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    assert!(DatabaseService::open(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(DatabaseService::create(&path).is_err());
    assert!(DatabaseService::open(&directory.path().join("missing.romedb")).is_err());
}

#[cfg(unix)]
#[test]
fn creating_a_database_refuses_engine_journals_and_dangling_sidecars() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("app.romedb");
    let journal = path.with_extension("db-log");
    std::fs::write(&journal, b"preserve this journal").unwrap();
    assert_eq!(
        DatabaseService::create(&path).err().unwrap().code,
        "FDB_CONFLICT"
    );
    assert!(!path.exists());
    assert_eq!(std::fs::read(&journal).unwrap(), b"preserve this journal");
    std::fs::remove_file(&journal).unwrap();
    let dangling = directory.path().join("app.romedb-wal");
    std::os::unix::fs::symlink(directory.path().join("missing"), &dangling).unwrap();
    assert_eq!(
        DatabaseService::create(&path).err().unwrap().code,
        "FDB_CONFLICT"
    );
    assert!(!path.exists());
    assert!(std::fs::symlink_metadata(dangling).is_ok());
    std::fs::remove_file(directory.path().join("app.romedb-wal")).unwrap();
    let journal = directory.path().join("app.romedb-journal");
    std::fs::write(&journal, b"preserve this rollback journal").unwrap();
    assert_eq!(
        DatabaseService::create(&path).err().unwrap().code,
        "FDB_CONFLICT"
    );
    assert!(!path.exists());
    assert_eq!(
        std::fs::read(journal).unwrap(),
        b"preserve this rollback journal"
    );
}

#[test]
fn service_open_rejects_file_aliases_before_creating_another_journal() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("app.romedb");
    let service = DatabaseService::create(&path).unwrap();
    assert!(query(
        &service,
        "setup",
        &[
            "CREATE TABLE rows(id INTEGER PRIMARY KEY)",
            "INSERT INTO rows VALUES(1)"
        ]
    )["error"]
        .is_null());
    drop(service);
    let before = std::fs::read(&path).unwrap();
    let symbolic = directory.path().join("symbolic.romedb");
    std::os::unix::fs::symlink(&path, &symbolic).unwrap();
    assert_eq!(
        DatabaseService::open(&symbolic).err().unwrap().code,
        "FDB_VALIDATION"
    );
    assert!(!symbolic.with_extension("db-log").exists());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::remove_file(symbolic).unwrap();
    let hard = directory.path().join("hard.romedb");
    std::fs::hard_link(&path, &hard).unwrap();
    assert_eq!(
        DatabaseService::open(&hard).err().unwrap().code,
        "FDB_VALIDATION"
    );
    assert_eq!(
        DatabaseService::open(&path).err().unwrap().code,
        "FDB_VALIDATION"
    );
    assert!(!hard.with_extension("db-log").exists());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::remove_file(hard).unwrap();
    let service = DatabaseService::open(&path).unwrap();
    assert_eq!(service.info().unwrap()["sequence"], 1);
    assert_eq!(
        read(&service, "SELECT id FROM rows")["results"][0]["rows"][0][0]["value"],
        "1"
    );
}

#[test]
fn independent_connections_publish_a_complete_sequence() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(DatabaseService::create(&directory.path().join("app.romedb")).unwrap());
    query(
        &service,
        "setup",
        &["CREATE TABLE items (id INTEGER PRIMARY KEY)"],
    );
    let barrier = Arc::new(Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|index| {
            let service = service.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let body = request(
                    &format!("client_{index}"),
                    1,
                    &[&format!("INSERT INTO items VALUES({index})")],
                );
                barrier.wait();
                let response: Value = serde_json::from_str(&service.query(&body).unwrap()).unwrap();
                assert!(response["error"].is_null(), "{response}");
                assert_eq!(service.query(&body).unwrap(), response.to_string());
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(service.info().unwrap()["sequence"], 5);
    assert_eq!(
        read(&service, "SELECT id FROM items ORDER BY id")["results"][0]["rows"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    service.begin_shutdown();
    assert_eq!(service.info().unwrap_err().code, "FDB_CLOSED");
}

#[test]
fn an_evicted_receipt_cannot_execute_an_old_request_again() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatabaseService::create(&directory.path().join("app.romedb")).unwrap();
    let original = request(
        "original",
        0,
        &[
            "CREATE TABLE items (id INTEGER PRIMARY KEY)",
            "INSERT INTO items VALUES(1)",
        ],
    );
    service.query(&original).unwrap();
    for index in 0..64 {
        assert!(query(&service, &format!("later_{index}"), &["SELECT 1"])["error"].is_null());
    }
    assert_eq!(service.query(&original).unwrap_err().code, "FDB_CONFLICT");
    assert_eq!(service.info().unwrap()["sequence"], 65);
    assert_eq!(
        read(&service, "SELECT id FROM items")["results"][0]["rows"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn portable_parameters_keep_integer_float_binary_and_missing_types() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatabaseService::create(&directory.path().join("app.romedb")).unwrap();
    query(&service, "setup", &["CREATE TABLE docs;"]);
    let document = json!({"type":"Object","value":{
        "id":{"type":"Record","value":{"table":"docs","key":{"type":"Integer","value":"9223372036854775807"}}},
        "count":{"type":"Integer","value":"9223372036854775807"},
        "number":{"type":"Number","value":"3ff0000000000000"},
        "negativeZero":{"type":"Number","value":"8000000000000000"},
        "binary":{"type":"Binary","value":[0,255]},
        "null":{"type":"Null"},
        "nested":{"type":"Object","value":{"type":{"type":"String","value":"Integer"},"value":{"type":"String","value":"user content"}}}
    }});
    let body = json!({"requestId":"typed","afterSequence":1,"statements":[{"sql":"INSERT INTO docs DOCUMENT $doc RETURNING *","params":{"$doc":document}}]}).to_string();
    let response: Value = serde_json::from_str(&service.query(&body).unwrap()).unwrap();
    assert!(response["error"].is_null(), "{response}");
    let result = read(&service, "SELECT * FROM docs");
    let returned = &result["results"][0]["rows"][0][0];
    assert_eq!(returned, &document);
    assert!(returned["value"].get("missing").is_none());
}

#[test]
fn validation_failure_rolls_back_all_managed_indexes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("app.romedb");
    let service = DatabaseService::create(&path).unwrap();
    let setup = query(&service, "setup", &[
        "CREATE TABLE docs;",
        "DEFINE FIELD title ON docs TYPE string REQUIRED",
        "DEFINE FIELD v ON docs TYPE vector<2>",
        "CREATE INDEX docs_title ON docs(title)",
        "CREATE SEARCH INDEX docs_text ON docs(title) USING FULLTEXT",
        "CREATE SEARCH INDEX docs_vector ON docs(v) USING VECTOR WITH (dimensions=2,metric='l2')",
        "CREATE SEARCH INDEX docs_spatial ON docs(location) USING SPATIAL",
        "INSERT INTO docs {id:docs:original,title:'originalterm',v:vector32('[1,0]'),location:geo::point(0,0)}",
    ]);
    assert!(setup["error"].is_null(), "{setup}");
    let failed = query(&service, "rejected", &[
        "UPDATE docs:original SET title='changedterm',v=vector32('[10,0]'),location=geo::point(10,10)",
        "INSERT INTO docs {id:docs:invalid,title:123}",
    ]);
    assert_eq!(failed["error"]["code"], "FDB_VALIDATION", "{failed}");
    assert!(
        failed["executionWork"]["rowMutations"].as_u64().unwrap() > 0,
        "first mutation did not execute: {failed}"
    );
    let scalar = "SELECT id FROM docs WHERE title='originalterm'";
    let text = "SELECT id FROM search::text('docs_text','originalterm',10)";
    let vector = "SELECT id FROM search::vector('docs_vector',vector32('[1,0]'),1)";
    let spatial = "SELECT id FROM search::near('docs_spatial',geo::point(0,0),10)";
    for sql in [scalar, text, vector, spatial] {
        assert_eq!(
            read(&service, sql)["results"][0]["rows"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "{sql}"
        );
    }
    assert_eq!(
        read(
            &service,
            "SELECT id FROM search::text('docs_text','changedterm',10)"
        )["results"][0]["rows"],
        json!([])
    );
    assert!(read(&service, &format!("EXPLAIN QUERY PLAN {scalar}"))
        .to_string()
        .contains("docs_title"));
    assert!(read(&service, &format!("EXPLAIN QUERY PLAN {text}"))
        .to_string()
        .contains("QUERY INDEX METHOD fts"));
    drop(service);
    let database = romedb::Database::open_for_service(path.to_str().unwrap()).unwrap();
    let integrity = database
        .connect()
        .unwrap()
        .check_collection_integrity("docs", Default::default())
        .unwrap();
    assert_eq!(integrity.documents, 1);
    assert_eq!(integrity.index_entries, 4);
}

#[test]
fn stored_functions_and_analyzers_resolve_inside_the_read_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatabaseService::create(&directory.path().join("app.romedb")).unwrap();
    let setup = query(&service, "setup", &[
        "CREATE TABLE docs;",
        "CREATE SEARCH INDEX docs_text ON docs(title) USING FULLTEXT",
        "CREATE FUNCTION app::normalize(value string) RETURNS string LANGUAGE JAVASCRIPT AS 'return value.trim().toLowerCase();'",
    ]);
    assert!(setup["error"].is_null(), "{setup}");
    let result = read(
        &service,
        "SELECT app::normalize('  HELLO  '),search::analyze('docs_text','Hello')",
    );
    assert_eq!(result["results"][0]["rows"][0][0]["value"], "hello");
    assert_eq!(result["results"][0]["rows"][0][1]["type"], "Array");
}

#[test]
fn a_document_insert_creates_its_missing_collection() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatabaseService::create(&directory.path().join("app.romedb")).unwrap();
    let insert = query(
        &service,
        "first-post",
        &["INSERT INTO posts { id: posts:first, title: 'first post', content: 'Hello, this is the first post' }"],
    );
    assert!(insert["error"].is_null(), "{insert}");
    assert_eq!(insert["sequence"], 1);
    assert_eq!(
        read(&service, "SELECT title FROM posts:first")["results"][0]["rows"],
        json!([[{"type":"String","value":"first post"}]])
    );
    let second = query(
        &service,
        "second-post",
        &["INSERT INTO posts { id: posts:second, title: 'second post' }"],
    );
    assert!(second["error"].is_null(), "{second}");
    assert_eq!(second["sequence"], 2);
}

#[test]
fn a_failed_insert_into_a_missing_collection_leaves_no_collection() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatabaseService::create(&directory.path().join("app.romedb")).unwrap();
    let failure = query(
        &service,
        "failed-post",
        &[
            "INSERT INTO posts { id: posts:first, title: 'first post' }",
            "INSERT INTO posts { id: posts:first, title: 'duplicate' }",
        ],
    );
    assert!(failure["error"]["code"].is_string(), "{failure}");
    assert_eq!(failure["sequence"], 1);
    let missing: Value = serde_json::from_str(
        &service
            .read(&{
                let mut body: Value =
                    serde_json::from_str(&request("read", 0, &["SELECT id FROM posts"])).unwrap();
                body["readVersion"] = json!(2);
                body.to_string()
            })
            .unwrap_or_else(|error| json!({"error":error.code}).to_string()),
    )
    .unwrap();
    assert!(!missing["error"].is_null(), "{missing}");
}

#[test]
fn a_retried_write_gets_a_fresh_work_budget() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(DatabaseService::create(&directory.path().join("app.romedb")).unwrap());
    let setup = query(
        &service,
        "setup",
        &["CREATE TABLE items (id INTEGER PRIMARY KEY)"],
    );
    assert!(setup["error"].is_null(), "{setup}");
    let mut body: Value = serde_json::from_str(&request(
        "slow",
        1,
        &[
            "INSERT INTO items VALUES(1)",
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<20000) SELECT count(*) FROM n",
        ],
    ))
    .unwrap();
    body["workLimits"] = json!({"rowMutations":1});
    let slow = {
        let service = service.clone();
        std::thread::spawn(move || service.query(&body.to_string()).unwrap())
    };
    std::thread::sleep(std::time::Duration::from_millis(100));
    let other = query(&service, "other", &["INSERT INTO items VALUES(2)"]);
    assert!(other["error"].is_null(), "{other}");
    let slow: Value = serde_json::from_str(&slow.join().unwrap()).unwrap();
    assert!(slow["error"].is_null(), "{slow}");
    assert_eq!(slow["sequence"], 3);
    assert_eq!(slow["executionWork"]["rowMutations"], 1);
    assert_eq!(
        read(&service, "SELECT id FROM items ORDER BY id")["results"][0]["rows"],
        json!([[{"type":"Integer","value":"1"}],[{"type":"Integer","value":"2"}]])
    );
}

#[test]
fn malformed_parameters_fail_the_request_without_stopping_the_service() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatabaseService::create(&directory.path().join("app.romedb")).unwrap();
    for (index, value) in [
        json!(5),
        json!({"type":"Integer","value":"99999999999999999999"}),
        json!({"type":"Number","value":"not a number"}),
        json!({"type":"Bytes","value":"AA"}),
    ]
    .into_iter()
    .enumerate()
    {
        let body = json!({"requestId":format!("bad-{index}"),"afterSequence":index,"statements":[{"sql":"SELECT $v","params":{"$v":value}}]}).to_string();
        let reply: Value = serde_json::from_str(&service.query(&body).unwrap()).unwrap();
        assert_eq!(reply["error"]["code"], "FDB_VALIDATION", "{reply}");
        assert_eq!(reply["sequence"], index + 1);
    }
    let after = query(&service, "after", &["SELECT 1"]);
    assert!(after["error"].is_null(), "{after}");
    assert_eq!(after["sequence"], 5);
}

#[test]
fn a_record_target_insert_sets_the_document_id() {
    let directory = tempfile::tempdir().unwrap();
    let service = DatabaseService::create(&directory.path().join("app.romedb")).unwrap();
    let insert = query(
        &service,
        "target-insert",
        &["INSERT INTO posts:first {\n    foo: 'bar'\n}"],
    );
    assert!(insert["error"].is_null(), "{insert}");
    assert_eq!(
        read(&service, "SELECT foo FROM posts:first")["results"][0]["rows"],
        json!([[{"type":"String","value":"bar"}]])
    );
    let duplicate = query(
        &service,
        "target-duplicate",
        &["INSERT INTO posts:first {foo: 'again'}"],
    );
    assert_eq!(duplicate["error"]["code"], "FDB_CONSTRAINT", "{duplicate}");
}
