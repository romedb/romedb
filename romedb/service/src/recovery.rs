use super::*;
use std::process::Command;
use turso_core::{
    io::{
        clock::{Clock, MonotonicInstant, WallClockInstant},
        FileSyncType, UnixIO,
    },
    Buffer, Completion, File, OpenFlags, IO,
};

struct FaultIo {
    inner: UnixIO,
    armed: Arc<AtomicBool>,
    fault: String,
    file_suffix: &'static str,
}

struct FaultFile {
    inner: Arc<dyn File>,
    armed: Arc<AtomicBool>,
    selected: bool,
    fault: String,
}

#[test]
fn publication_fault_child() {
    let Ok(path) = std::env::var("ROMEDB_SERVICE_FAULT_PATH") else {
        return;
    };
    let fault = std::env::var("ROMEDB_SERVICE_FAULT_KIND").unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let io = Arc::new(FaultIo {
        inner: UnixIO::new().unwrap(),
        armed: armed.clone(),
        fault: fault.clone(),
        file_suffix: ".db-log",
    });
    let database = Database::open_with_io(&path, io).unwrap();
    let service = DatabaseService::initialize(database).unwrap();
    armed.store(true, Ordering::SeqCst);
    let result = service.query(&update_request());
    assert!(
        !armed.load(Ordering::SeqCst),
        "fault was not reached: {fault}"
    );
    assert_eq!(result.unwrap_err().code, "FDB_STORAGE");
    assert_eq!(service.info().unwrap_err().code, "FDB_STORAGE");
    assert_eq!(service.read(&json!({"requestId":"read","afterSequence":2,"readVersion":2,"statements":[{"sql":"SELECT n FROM docs"}]}).to_string()).unwrap_err().code, "FDB_STORAGE");
    assert_eq!(
        service.query(&update_request()).unwrap_err().code,
        "FDB_STORAGE"
    );
    std::process::exit(74);
}

#[test]
fn atomic_request_fault_child() {
    let Ok(path) = std::env::var("ROMEDB_ATOMIC_FAULT_PATH") else {
        return;
    };
    let fault = std::env::var("ROMEDB_ATOMIC_FAULT_KIND").unwrap();
    let mvcc = std::env::var("ROMEDB_ATOMIC_FAULT_JOURNAL").unwrap() == "mvcc";
    let armed = Arc::new(AtomicBool::new(false));
    let io = Arc::new(FaultIo {
        inner: UnixIO::new().unwrap(),
        armed: armed.clone(),
        fault: fault.clone(),
        file_suffix: if mvcc { ".db-log" } else { "-wal" },
    });
    let database = Database::open_with_io(&path, io).unwrap();
    let connection = database.connect().unwrap();
    let update: Value = serde_json::from_str(&update_request()).unwrap();
    armed.store(true, Ordering::SeqCst);
    let outcome = connection.execute_atomic_request(
        &json!({"readOnly":false,"statements":update["statements"]}).to_string(),
        &CancellationToken::new(),
    );
    assert!(
        !armed.load(Ordering::SeqCst),
        "fault was not reached: {fault}"
    );
    assert_eq!(outcome.unwrap_err().code(), "FDB_STORAGE");
    std::process::exit(74);
}

#[test]
fn atomic_request_publication_faults_preserve_complete_data_and_indexes() {
    for journal in ["wal", "mvcc"] {
        for fault in [
            "enospc",
            "eio",
            "partial",
            "sync",
            "sync-after",
            "stop-before-write",
            "stop-after-write",
            "stop-after-sync",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("app.romedb");
            {
                let database = Database::open(path.to_str().unwrap()).unwrap();
                let connection = database.connect().unwrap();
                if journal == "mvcc" {
                    connection
                        .execute("PRAGMA journal_mode=mvcc", &Parameters::new())
                        .unwrap();
                }
                for statement in fixture_setup()["statements"].as_array().unwrap() {
                    connection
                        .execute(statement["sql"].as_str().unwrap(), &Parameters::new())
                        .unwrap();
                }
                for sql in [
                    "UPDATE docs SET n=2 WHERE id=docs:a",
                    "UPDATE rows SET n=2 WHERE id=1",
                ] {
                    connection.execute(sql, &Parameters::new()).unwrap();
                }
            }
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "recovery::atomic_request_fault_child",
                    "--nocapture",
                ])
                .env("ROMEDB_ATOMIC_FAULT_PATH", &path)
                .env("ROMEDB_ATOMIC_FAULT_KIND", fault)
                .env("ROMEDB_ATOMIC_FAULT_JOURNAL", journal)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(if fault.starts_with("stop-") { 73 } else { 74 }),
                "{journal}/{fault}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let database = Database::open(path.to_str().unwrap()).unwrap();
            let connection = database.connect().unwrap();
            let n = connection
                .execute("SELECT n FROM docs WHERE id=docs:a", &Parameters::new())
                .unwrap()
                .rows;
            let expected = match n.as_slice() {
                [row] if row == &[romedb::Value::Integer(2)] => 2,
                [row] if row == &[romedb::Value::Integer(3)] => 3,
                _ => panic!("unexpected recovered data: {n:?}"),
            };
            assert_index_state(&connection, expected);
        }
    }
}

#[test]
fn publication_errors_and_process_stops_preserve_data_receipt_and_indexes() {
    for fault in [
        "enospc",
        "eio",
        "partial",
        "sync",
        "sync-after",
        "stop-before-write",
        "stop-after-write",
        "stop-after-sync",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("app.romedb");
        let (identity, acknowledged) = fixture(&path);
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "recovery::publication_fault_child",
                "--nocapture",
            ])
            .env("ROMEDB_SERVICE_FAULT_PATH", &path)
            .env("ROMEDB_SERVICE_FAULT_KIND", fault)
            .output()
            .unwrap();
        let expected_exit = if fault.starts_with("stop-") { 73 } else { 74 };
        assert_eq!(
            output.status.code(),
            Some(expected_exit),
            "{fault}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        {
            let service = DatabaseService::open(&path).unwrap();
            assert_eq!(service.info().unwrap()["id"], identity);
            assert_eq!(service.query(&acknowledged.0).unwrap(), acknowledged.1);
            let publication = service.publication.lock().unwrap();
            let connection = &publication.connection;
            let receipt = connection.service_receipt("uncertain").unwrap();
            let expected = if receipt.is_some() { 3 } else { 2 };
            assert_eq!(publication.sequence, if receipt.is_some() { 3 } else { 2 });
            if let Some(receipt) = receipt {
                let response: Value = serde_json::from_str(&receipt.response).unwrap();
                assert!(response["error"].is_null(), "{fault}: {response}");
                assert_eq!(
                    receipt.request_hash,
                    format!("{:x}", Sha256::digest(update_request().as_bytes()))
                );
                assert_eq!(receipt.sequence, 3);
                assert_eq!(
                    service
                        .replay(
                            &publication,
                            &Request::parse(&update_request(), false).unwrap(),
                            &receipt.request_hash
                        )
                        .unwrap(),
                    Some(receipt.response)
                );
            }
            assert_index_state(connection, expected);
        }
        let service = DatabaseService::open(&path).unwrap();
        let response = service.query(&update_request()).unwrap();
        let reply: Value = serde_json::from_str(&response).unwrap();
        assert!(reply["error"].is_null(), "{fault}: {reply}");
        assert_eq!(reply["sequence"], 3);
        assert_eq!(service.query(&update_request()).unwrap(), response);
        assert_index_state(&service.publication.lock().unwrap().connection, 3);
    }
}

fn fixture(path: &Path) -> (Value, (String, String)) {
    let service = DatabaseService::create(path).unwrap();
    let setup = fixture_setup().to_string();
    let reply: Value = serde_json::from_str(&service.query(&setup).unwrap()).unwrap();
    assert!(reply["error"].is_null(), "{reply}");
    let body = json!({"requestId":"acknowledged","afterSequence":1,"statements":[
        {"sql":"UPDATE docs SET n=2 WHERE id=docs:a"},
        {"sql":"UPDATE rows SET n=2 WHERE id=1"}
    ]})
    .to_string();
    let response = service.query(&body).unwrap();
    let reply: Value = serde_json::from_str(&response).unwrap();
    assert!(reply["error"].is_null(), "{reply}");
    let identity = service.info().unwrap()["id"].clone();
    (identity, (body, response))
}

fn fixture_setup() -> Value {
    json!({"requestId":"setup","afterSequence":0,"statements":[
        {"sql":"CREATE TABLE docs;"},
        {"sql":"CREATE UNIQUE INDEX docs_n ON docs(n)"},
        {"sql":"CREATE SEARCH INDEX docs_text ON docs(title) USING FULLTEXT"},
        {"sql":"CREATE SEARCH INDEX docs_vector ON docs(v) USING VECTOR WITH (dimensions=2,metric='l2')"},
        {"sql":"CREATE SEARCH INDEX docs_spatial ON docs(location) USING SPATIAL"},
        {"sql":"CREATE TABLE rows (id INTEGER PRIMARY KEY,n INTEGER)"},
        {"sql":"INSERT INTO docs {id:docs:a,n:1,title:'beforeterm',v:vector32('[1,0]'),location:geo::point(0,0)}"},
        {"sql":"INSERT INTO docs {id:docs:b,n:100,title:'controlterm',v:vector32('[0.1,0.9]'),location:geo::point(20,0)}"},
        {"sql":"INSERT INTO rows VALUES(1,1)"}
    ]})
}

fn update_request() -> String {
    json!({"requestId":"uncertain","afterSequence":2,"statements":[
        {"sql":"UPDATE docs SET n=3,title='afterterm',v=vector32('[0,1]'),location=geo::point(10,0) WHERE id=docs:a"},
        {"sql":"UPDATE rows SET n=3 WHERE id=1"}
    ]}).to_string()
}

fn assert_index_state(connection: &Connection, n: i64) {
    let query = |sql: &str| connection.execute(sql, &Parameters::new()).unwrap().rows;
    assert_eq!(
        query("SELECT n FROM docs WHERE id=docs:a"),
        vec![vec![romedb::Value::Integer(n)]]
    );
    assert_eq!(
        query("SELECT n FROM rows WHERE id=1"),
        vec![vec![romedb::Value::Integer(n)]]
    );
    let record = |key: &str| {
        romedb::Value::Record(romedb::Record {
            table: "docs".into(),
            key: romedb::Key::String(key.into()),
        })
    };
    assert_eq!(
        query(&format!("SELECT id FROM docs WHERE n={n}")),
        vec![vec![record("a")]]
    );
    let (present, absent, near, far, vector) = if n == 3 {
        ("afterterm", "beforeterm", 10, 0, "a")
    } else {
        ("beforeterm", "afterterm", 0, 10, "b")
    };
    assert_eq!(
        query(&format!(
            "SELECT id FROM search::text('docs_text','{present}',10)"
        )),
        vec![vec![record("a")]]
    );
    assert!(query(&format!(
        "SELECT id FROM search::text('docs_text','{absent}',10)"
    ))
    .is_empty());
    assert_eq!(
        query(&format!(
            "SELECT id FROM search::near('docs_spatial',geo::point({near},0),1)"
        )),
        vec![vec![record("a")]]
    );
    assert!(query(&format!(
        "SELECT id FROM search::near('docs_spatial',geo::point({far},0),1)"
    ))
    .is_empty());
    assert_eq!(
        query("SELECT id FROM search::vector('docs_vector',vector32('[0,1]'),1)"),
        vec![vec![record(vector)]]
    );
    let integrity = connection
        .check_collection_integrity("docs", Default::default())
        .unwrap();
    assert_eq!((integrity.documents, integrity.index_entries), (2, 8));
    assert_eq!(
        query("PRAGMA integrity_check"),
        vec![vec![romedb::Value::String("ok".into())]]
    );
}

impl File for FaultFile {
    fn lock_file(&self, exclusive: bool) -> turso_core::Result<()> {
        self.inner.lock_file(exclusive)
    }
    fn unlock_file(&self) -> turso_core::Result<()> {
        self.inner.unlock_file()
    }
    fn pread(&self, pos: u64, completion: Completion) -> turso_core::Result<Completion> {
        self.inner.pread(pos, completion)
    }
    fn pwrite(
        &self,
        pos: u64,
        buffer: Arc<Buffer>,
        completion: Completion,
    ) -> turso_core::Result<Completion> {
        if self.selected && !self.fault.contains("sync") && self.armed.swap(false, Ordering::SeqCst)
        {
            match self.fault.as_str() {
                "stop-before-write" => std::process::exit(73),
                "stop-after-write" => {
                    assert!(self.inner.pwrite(pos, buffer, completion)?.succeeded());
                    std::process::exit(73);
                }
                "partial" => {
                    let prefix =
                        Arc::new(Buffer::new(buffer.as_slice()[..buffer.len() / 2].to_vec()));
                    assert!(!prefix.is_empty());
                    assert!(self
                        .inner
                        .pwrite(pos, prefix, Completion::new_write(|_| {}))?
                        .succeeded());
                }
                _ => {}
            }
            let code = if self.fault == "enospc" { 28 } else { 5 };
            return Err(turso_core::io_error(
                std::io::Error::from_raw_os_error(code),
                "injected service log write",
            ));
        }
        self.inner.pwrite(pos, buffer, completion)
    }
    fn sync(&self, completion: Completion, kind: FileSyncType) -> turso_core::Result<Completion> {
        if self.selected && self.fault.contains("sync") && self.armed.swap(false, Ordering::SeqCst)
        {
            if matches!(self.fault.as_str(), "sync-after" | "stop-after-sync") {
                assert!(self.inner.sync(completion, kind)?.succeeded());
                if self.fault == "stop-after-sync" {
                    std::process::exit(73);
                }
            }
            return Err(turso_core::io_error(
                std::io::Error::from_raw_os_error(5),
                "injected service log sync",
            ));
        }
        self.inner.sync(completion, kind)
    }
    fn size(&self) -> turso_core::Result<u64> {
        self.inner.size()
    }
    fn truncate(&self, len: u64, completion: Completion) -> turso_core::Result<Completion> {
        self.inner.truncate(len, completion)
    }
}

impl Clock for FaultIo {
    fn current_time_monotonic(&self) -> MonotonicInstant {
        self.inner.current_time_monotonic()
    }
    fn current_time_wall_clock(&self) -> WallClockInstant {
        self.inner.current_time_wall_clock()
    }
}

impl IO for FaultIo {
    fn open_file(
        &self,
        path: &str,
        flags: OpenFlags,
        direct: bool,
    ) -> turso_core::Result<Arc<dyn File>> {
        Ok(Arc::new(FaultFile {
            inner: self.inner.open_file(path, flags, direct)?,
            armed: self.armed.clone(),
            selected: path.ends_with(self.file_suffix),
            fault: self.fault.clone(),
        }))
    }
    fn remove_file(&self, path: &str) -> turso_core::Result<()> {
        self.inner.remove_file(path)
    }
}
