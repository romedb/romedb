//! Standalone proposed regression; not registered in the release test suite.
use romedb::{Database, Parameters};
use std::sync::{Arc, Mutex};
use turso_core::io::{
    clock::{Clock, MonotonicInstant, WallClockInstant},
    FileSyncType, UnixIO,
};
use turso_core::{Buffer, Completion, File, OpenFlags, IO};

type Trace = Arc<Mutex<Vec<String>>>;
struct TraceIo {
    inner: UnixIO,
    events: Trace,
}
struct TraceFile {
    inner: Arc<dyn File>,
    kind: &'static str,
    events: Trace,
}
impl File for TraceFile {
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
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:write:{pos}:{}", self.kind, b.len()));
        self.inner.pwrite(pos, b, c)
    }
    fn sync(&self, c: Completion, kind: FileSyncType) -> turso_core::Result<Completion> {
        let result = self.inner.sync(c, kind)?;
        assert!(result.succeeded());
        self.events
            .lock()
            .unwrap()
            .push(format!("{}:sync:ok", self.kind));
        Ok(result)
    }
    fn size(&self) -> turso_core::Result<u64> {
        self.inner.size()
    }
    fn truncate(&self, len: u64, c: Completion) -> turso_core::Result<Completion> {
        self.inner.truncate(len, c)
    }
}
impl Clock for TraceIo {
    fn current_time_monotonic(&self) -> MonotonicInstant {
        self.inner.current_time_monotonic()
    }
    fn current_time_wall_clock(&self) -> WallClockInstant {
        self.inner.current_time_wall_clock()
    }
}
impl IO for TraceIo {
    fn open_file(
        &self,
        path: &str,
        flags: OpenFlags,
        direct: bool,
    ) -> turso_core::Result<Arc<dyn File>> {
        Ok(Arc::new(TraceFile {
            inner: self.inner.open_file(path, flags, direct)?,
            kind: if path.ends_with("-wal") { "wal" } else { "db" },
            events: self.events.clone(),
        }))
    }
    fn remove_file(&self, path: &str) -> turso_core::Result<()> {
        self.inner.remove_file(path)
    }
}
fn main() {
    let path =
        std::env::temp_dir().join(format!("romedb-normal-barrier-{}.db", std::process::id()));
    let events: Trace = Arc::new(Mutex::new(Vec::new()));
    let db = Database::open_with_io(
        path.to_str().unwrap(),
        Arc::new(TraceIo {
            inner: UnixIO::new().unwrap(),
            events: events.clone(),
        }),
    )
    .unwrap();
    let c = db.connect().unwrap();
    let q = |sql: &str| c.execute(sql, &Parameters::new()).unwrap();
    q("PRAGMA synchronous=FULL");
    q("CREATE TABLE items(id INTEGER PRIMARY KEY,n INTEGER,payload TEXT)");
    q("INSERT INTO items VALUES(1,1,printf('%08000d',1)),(2,1,printf('%08000d',2))");
    q("PRAGMA wal_checkpoint(TRUNCATE)");
    q("PRAGMA synchronous=NORMAL");
    events.lock().unwrap().clear();
    q("BEGIN");
    q("UPDATE items SET n=2");
    q("COMMIT");
    eprintln!("NORMAL commit I/O: {:?}", events.lock().unwrap());
    events.lock().unwrap().clear();
    let result = q("PRAGMA wal_checkpoint(TRUNCATE)");
    let checkpoint = events.lock().unwrap().clone();
    eprintln!("checkpoint status: {:?}", result.rows);
    eprintln!("checkpoint I/O: {checkpoint:?}");
    let first_db_write = checkpoint
        .iter()
        .position(|event| event.starts_with("db:write:"))
        .expect("fixture must backfill pages");
    assert!(
        checkpoint[..first_db_write]
            .iter()
            .any(|event| event == "wal:sync:ok"),
        "checkpoint wrote database pages before syncing the NORMAL transaction's WAL frames"
    );
    drop(c);
    drop(db);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
