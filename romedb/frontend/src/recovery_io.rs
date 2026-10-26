//! Deterministic process exits and returned synchronous filesystem failures.
use crate::{Database, Parameters, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use turso_core::io::{
    clock::{Clock, MonotonicInstant, WallClockInstant},
    FileSyncType, UnixIO,
};
use turso_core::{Buffer, Completion, File, OpenFlags, IO};

struct StopIo {
    inner: UnixIO,
    armed: Arc<AtomicBool>,
    phase: String,
    operation: String,
}
struct StopFile {
    inner: Arc<dyn File>,
    armed: Arc<AtomicBool>,
    selected: bool,
    operation: String,
}
impl StopFile {
    fn stop(&self, operation: &str) {
        if self.selected && self.operation == operation && self.armed.load(Ordering::SeqCst) {
            std::process::exit(73);
        }
    }
}
impl File for StopFile {
    fn lock_file(&self, exclusive: bool) -> turso_core::Result<()> {
        self.inner.lock_file(exclusive)
    }
    fn unlock_file(&self) -> turso_core::Result<()> {
        self.inner.unlock_file()
    }
    fn pread(&self, pos: u64, c: Completion) -> turso_core::Result<Completion> {
        self.inner.pread(pos, c)
    }
    fn pwrite(&self, pos: u64, b: Arc<Buffer>, c: Completion) -> turso_core::Result<Completion> {
        self.stop("write");
        self.inner.pwrite(pos, b, c)
    }
    fn sync(&self, c: Completion, kind: FileSyncType) -> turso_core::Result<Completion> {
        self.stop("sync");
        self.inner.sync(c, kind)
    }
    fn size(&self) -> turso_core::Result<u64> {
        self.inner.size()
    }
    fn truncate(&self, len: u64, c: Completion) -> turso_core::Result<Completion> {
        self.inner.truncate(len, c)
    }
}
impl Clock for StopIo {
    fn current_time_monotonic(&self) -> MonotonicInstant {
        self.inner.current_time_monotonic()
    }
    fn current_time_wall_clock(&self) -> WallClockInstant {
        self.inner.current_time_wall_clock()
    }
}
impl IO for StopIo {
    fn open_file(
        &self,
        path: &str,
        flags: OpenFlags,
        direct: bool,
    ) -> turso_core::Result<Arc<dyn File>> {
        Ok(Arc::new(StopFile {
            inner: self.inner.open_file(path, flags, direct)?,
            armed: self.armed.clone(),
            selected: if self.phase == "commit" {
                path.ends_with("-wal")
            } else {
                path.ends_with("app.db")
            },
            operation: self.operation.clone(),
        }))
    }
    fn remove_file(&self, path: &str) -> turso_core::Result<()> {
        self.inner.remove_file(path)
    }
}
fn q(c: &crate::Connection, sql: &str) -> crate::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
#[test]
fn io_stop_child() {
    let Ok(path) = std::env::var("ROMEDB_IO_STOP_PATH") else {
        return;
    };
    let phase = std::env::var("ROMEDB_IO_STOP_PHASE").unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let io = Arc::new(StopIo {
        inner: UnixIO::new().unwrap(),
        armed: armed.clone(),
        phase: phase.clone(),
        operation: std::env::var("ROMEDB_IO_STOP_OP").unwrap(),
    });
    let db = Database {
        engine: turso_core::Database::open_file(
            io,
            &path,
            std::sync::Arc::new(turso_core::SqliteDialect),
        )
        .unwrap(),
        manual_wal: false,
    };
    let c = db.connect().unwrap();
    q(&c, "PRAGMA synchronous=FULL");
    q(&c, "BEGIN");
    q(&c, "UPDATE items SET n=2");
    q(&c, "INSERT INTO events VALUES(2)");
    if phase == "commit" {
        armed.store(true, Ordering::SeqCst);
    }
    q(&c, "COMMIT");
    if phase == "checkpoint" {
        armed.store(true, Ordering::SeqCst);
    }
    q(&c, "PRAGMA wal_checkpoint(TRUNCATE)");
    panic!("selected I/O boundary was not reached");
}
#[test]
fn commit_checkpoint_io_boundaries_recover_atomic_state() {
    for phase in ["commit", "checkpoint"] {
        for operation in ["write", "sync"] {
            let dir = std::env::temp_dir().join(format!(
                "romedb-io-stop-{}-{phase}-{operation}",
                std::process::id()
            ));
            std::fs::create_dir(&dir).unwrap();
            let path = dir.join("app.db");
            {
                let db = Database::open(path.to_str().unwrap()).unwrap();
                let c = db.connect().unwrap();
                q(&c, "CREATE TABLE items");
                q(&c, "CREATE UNIQUE INDEX item_n ON items(n)");
                q(&c, "INSERT INTO items {id:items:a,n:1}");
                q(&c, "CREATE TABLE events(n INTEGER PRIMARY KEY)");
                q(&c, "INSERT INTO events VALUES(1)");
                q(&c, "PRAGMA wal_checkpoint(TRUNCATE)");
            }
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "recovery_io::io_stop_child", "--nocapture"])
                .env("ROMEDB_IO_STOP_PATH", &path)
                .env("ROMEDB_IO_STOP_PHASE", phase)
                .env("ROMEDB_IO_STOP_OP", operation)
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("I/O child timeout");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            assert_eq!(
                status.code(),
                Some(73),
                "{phase}/{operation}: boundary must be reached"
            );
            for _ in 0..2 {
                let db = Database::open(path.to_str().unwrap()).unwrap();
                let c = db.connect().unwrap();
                let n = q(&c, "SELECT n FROM items").rows[0][0].clone();
                assert!(n == Value::Integer(1) || n == Value::Integer(2));
                if phase == "checkpoint" {
                    assert_eq!(n, Value::Integer(2));
                }
                assert_eq!(q(&c, "SELECT count(*) FROM events").rows, vec![vec![n]]);
                c.check_collection_integrity("items", crate::IntegrityLimits::default())
                    .unwrap();
                assert_eq!(
                    q(&c, "PRAGMA integrity_check").rows,
                    vec![vec![Value::String("ok".into())]]
                );
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

// Returned failures differ from the process-stop cases above: the public query
// must signal the injected failure before the child terminates without Drop/close
// retries. Reopening is the supported recovery boundary after storage failure.
struct FaultIo {
    inner: UnixIO,
    armed: Arc<AtomicBool>,
    phase: String,
    fault: String,
}
struct FaultFile {
    inner: Arc<dyn File>,
    armed: Arc<AtomicBool>,
    selected: bool,
    wal: bool,
    fault: String,
}
impl FaultFile {
    fn take_fault(&self, sync: bool, pos: u64) -> bool {
        self.selected
            && (self.fault == "sync") == sync
            // Leave the WAL header intact and interrupt the first frame instead.
            && (sync || !self.wal || pos >= 32)
            && self.armed.swap(false, Ordering::SeqCst)
    }
    fn failure(&self, operation: &'static str) -> turso_core::LimboError {
        let error = match self.fault.as_str() {
            "enospc" => std::io::Error::from_raw_os_error(28),
            "eio" | "sync" => std::io::Error::from_raw_os_error(5),
            "partial" => std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
            other => panic!("unknown injected failure {other}"),
        };
        turso_core::io_error(error, operation)
    }
}
impl File for FaultFile {
    fn lock_file(&self, exclusive: bool) -> turso_core::Result<()> {
        self.inner.lock_file(exclusive)
    }
    fn unlock_file(&self) -> turso_core::Result<()> {
        self.inner.unlock_file()
    }
    fn pread(&self, pos: u64, c: Completion) -> turso_core::Result<Completion> {
        self.inner.pread(pos, c)
    }
    fn pwrite(&self, pos: u64, b: Arc<Buffer>, c: Completion) -> turso_core::Result<Completion> {
        // For partial writes, pass through the small WAL frame header and
        // truncate the following page payload, rather than only its header.
        if (self.fault != "partial" || b.len() >= 512) && self.take_fault(false, pos) {
            if self.fault == "partial" {
                // Model UnixIO making positive short-write progress, then a
                // zero-progress syscall: the prefix reaches the real file and
                // the backend returns UnexpectedEof, never a false full write.
                let prefix = Arc::new(Buffer::new(b.as_slice()[..b.len() / 2].to_vec()));
                assert!(!prefix.is_empty());
                let written = self
                    .inner
                    .pwrite(pos, prefix, Completion::new_write(|_| {}))?;
                assert!(written.succeeded());
            }
            return Err(self.failure("injected pwrite"));
        }
        self.inner.pwrite(pos, b, c)
    }
    fn sync(&self, c: Completion, kind: FileSyncType) -> turso_core::Result<Completion> {
        if self.take_fault(true, 0) {
            return Err(self.failure("injected sync"));
        }
        self.inner.sync(c, kind)
    }
    fn size(&self) -> turso_core::Result<u64> {
        self.inner.size()
    }
    fn truncate(&self, len: u64, c: Completion) -> turso_core::Result<Completion> {
        self.inner.truncate(len, c)
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
        let wal = path.ends_with("-wal");
        Ok(Arc::new(FaultFile {
            inner: self.inner.open_file(path, flags, direct)?,
            armed: self.armed.clone(),
            selected: if self.phase == "commit" {
                wal
            } else {
                path.ends_with("app.db")
            },
            wal,
            fault: self.fault.clone(),
        }))
    }
    fn remove_file(&self, path: &str) -> turso_core::Result<()> {
        self.inner.remove_file(path)
    }
}

fn indexed_fixture(path: &std::path::Path) {
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    q(&c, "PRAGMA synchronous=FULL");
    q(&c, "CREATE TABLE items");
    q(&c, "INSERT INTO items {id:items:a,n:1,body:'beforetoken',location:geo::point(0,0),v:vector32('[1,0]')}");
    q(&c, "INSERT INTO items {id:items:b,n:100,body:'controltoken',location:geo::point(20,0),v:vector32('[0.1,0.9]')}");
    q(&c, "CREATE UNIQUE INDEX item_n ON items(n)");
    q(
        &c,
        "CREATE SEARCH INDEX item_text ON items(body) USING FULLTEXT",
    );
    q(
        &c,
        "CREATE SEARCH INDEX item_geo ON items(location) USING SPATIAL",
    );
    q(
        &c,
        "CREATE SEARCH INDEX item_vec ON items(v) USING VECTOR WITH (metric='l2',dimensions=2)",
    );
    q(&c, "CREATE TABLE events(n INTEGER PRIMARY KEY)");
    q(&c, "INSERT INTO events VALUES(1)");
    q(&c, "PRAGMA wal_checkpoint(TRUNCATE)");
}
fn indexed_update(c: &crate::Connection, n: i64) {
    q(c, "BEGIN");
    q(c, &format!("UPDATE items SET n={n},body='aftertoken',location=geo::point(10,0),v=vector32('[0,1]') WHERE id=items:a"));
    q(c, &format!("INSERT INTO events VALUES({n})"));
}
fn record(key: &str) -> Value {
    Value::Record(crate::Record {
        table: "items".into(),
        key: crate::Key::String(key.into()),
    })
}
fn check_indexed_state(c: &crate::Connection) -> i64 {
    let Value::Integer(n) = q(c, "SELECT n FROM items WHERE id=items:a").rows[0][0] else {
        panic!("missing state marker");
    };
    assert_eq!(
        q(c, "SELECT count(*) FROM events").rows,
        vec![vec![Value::Integer(n)]]
    );
    let after = n >= 2;
    let (present, absent) = if after {
        ("aftertoken", "beforetoken")
    } else {
        ("beforetoken", "aftertoken")
    };
    assert_eq!(
        q(
            c,
            &format!("SELECT id FROM search::text('item_text','{present}',10)")
        )
        .rows,
        vec![vec![record("a")]]
    );
    assert!(q(
        c,
        &format!("SELECT id FROM search::text('item_text','{absent}',10)")
    )
    .rows
    .is_empty());
    let (near, far) = if after { (10, 0) } else { (0, 10) };
    assert_eq!(
        q(
            c,
            &format!("SELECT id FROM search::near('item_geo',geo::point({near},0),1)")
        )
        .rows,
        vec![vec![record("a")]]
    );
    assert!(q(
        c,
        &format!("SELECT id FROM search::near('item_geo',geo::point({far},0),1)")
    )
    .rows
    .is_empty());
    assert_eq!(
        q(
            c,
            "SELECT id FROM search::vector('item_vec',vector32('[0,1]'),1)"
        )
        .rows,
        vec![vec![record(if after { "a" } else { "b" })]]
    );
    c.check_collection_integrity("items", crate::IntegrityLimits::default())
        .unwrap();
    assert_eq!(
        q(c, "PRAGMA integrity_check").rows,
        vec![vec![Value::String("ok".into())]]
    );
    n
}

#[test]
fn returned_io_error_child() {
    let Ok(path) = std::env::var("ROMEDB_IO_ERROR_PATH") else {
        return;
    };
    let phase = std::env::var("ROMEDB_IO_ERROR_PHASE").unwrap();
    let fault = std::env::var("ROMEDB_IO_ERROR_FAULT").unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let io = Arc::new(FaultIo {
        inner: UnixIO::new().unwrap(),
        armed: armed.clone(),
        phase: phase.clone(),
        fault: fault.clone(),
    });
    let db = Database::open_with_io(&path, io).unwrap();
    let c = db.connect().unwrap();
    q(&c, "PRAGMA synchronous=FULL");
    q(&c, "PRAGMA wal_autocheckpoint=0");
    indexed_update(&c, 2);
    if phase == "checkpoint" {
        q(&c, "COMMIT");
    }
    armed.store(true, Ordering::SeqCst);
    let sql = if phase == "commit" {
        "COMMIT"
    } else {
        "PRAGMA wal_checkpoint(TRUNCATE)"
    };
    let result = c.execute(sql, &Parameters::new());
    assert!(!armed.load(Ordering::SeqCst), "selected fault must fire");
    if phase == "commit" {
        let error = result.expect_err("failed COMMIT I/O must never acknowledge success");
        assert!(
            error.to_string().contains("injected"),
            "unexpected failure: {error}"
        );
        eprintln!("{phase}/{fault}: correctly returned {error}");
    } else {
        // Pinned core op_checkpoint encodes every pager failure as busy=1,
        // including I/O errors, with NULL frame counts. A completed PRAGMA is
        // not a successful checkpoint: require this exact failed status here.
        let result = result.unwrap();
        assert_eq!(
            result.rows,
            vec![vec![Value::Integer(1), Value::Null, Value::Null]]
        );
        eprintln!(
            "{phase}/{fault}: correctly returned failed checkpoint status {:?}",
            result.rows
        );
    }
    // Do not let close/drop retry a failed operation and alter the evidence.
    std::process::exit(74);
}

#[test]
fn returned_commit_checkpoint_errors_preserve_v2_indexes() {
    for phase in ["commit", "checkpoint"] {
        for fault in ["enospc", "eio", "partial", "sync"] {
            let dir = std::env::temp_dir().join(format!(
                "romedb-io-error-{}-{phase}-{fault}",
                std::process::id()
            ));
            std::fs::create_dir(&dir).unwrap();
            let path = dir.join("app.db");
            indexed_fixture(&path);
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "recovery_io::returned_io_error_child",
                    "--nocapture",
                ])
                .env("ROMEDB_IO_ERROR_PATH", &path)
                .env("ROMEDB_IO_ERROR_PHASE", phase)
                .env("ROMEDB_IO_ERROR_FAULT", fault)
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("{phase}/{fault}: returned-error child timed out");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            assert_eq!(
                status.code(),
                Some(74),
                "{phase}/{fault}: public failure must return"
            );
            let recovered;
            {
                let db = Database::open(path.to_str().unwrap()).unwrap();
                let c = db.connect().unwrap();
                recovered = check_indexed_state(&c);
                assert!(
                    recovered == 1 || recovered == 2,
                    "{phase}/{fault}: complete old or new transaction required"
                );
                if phase == "checkpoint" {
                    assert_eq!(recovered, 2, "acknowledged COMMIT must survive");
                }
                eprintln!("{phase}/{fault}: recovered complete state {recovered}");
                // Prove both transaction rollback and new indexed writes still work.
                indexed_update(&c, recovered + 1);
                q(&c, "ROLLBACK");
                assert_eq!(check_indexed_state(&c), recovered);
                indexed_update(&c, recovered + 1);
                q(&c, "COMMIT");
                assert_eq!(check_indexed_state(&c), recovered + 1);
                assert_eq!(
                    q(&c, "PRAGMA wal_checkpoint(TRUNCATE)").rows,
                    vec![vec![
                        Value::Integer(0),
                        Value::Integer(0),
                        Value::Integer(0)
                    ]]
                );
            }
            {
                let db = Database::open(path.to_str().unwrap()).unwrap();
                let c = db.connect().unwrap();
                assert_eq!(check_indexed_state(&c), recovered + 1);
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
