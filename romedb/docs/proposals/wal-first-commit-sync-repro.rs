// Read-only engine API probe: records whether a FULL commit syncs its WAL writes.
use std::sync::{Arc, Mutex};
use turso_core::io::clock::{MonotonicInstant, WallClockInstant};
use turso_core::io::{FileId, FileSyncType};
use turso_core::{Buffer, Clock, Completion, File, MemoryIO, OpenFlags, IO};
struct TracedIo {
    inner: MemoryIO,
    events: Arc<Mutex<Vec<&'static str>>>,
}
struct TracedFile {
    inner: Arc<dyn File>,
    wal: bool,
    events: Arc<Mutex<Vec<&'static str>>>,
}
impl Clock for TracedIo {
    fn current_time_monotonic(&self) -> MonotonicInstant {
        self.inner.current_time_monotonic()
    }
    fn current_time_wall_clock(&self) -> WallClockInstant {
        self.inner.current_time_wall_clock()
    }
}
impl IO for TracedIo {
    fn open_file(
        &self,
        path: &str,
        flags: OpenFlags,
        direct: bool,
    ) -> turso_core::Result<Arc<dyn File>> {
        Ok(Arc::new(TracedFile {
            inner: self.inner.open_file(path, flags, direct)?,
            wal: path.ends_with("-wal"),
            events: self.events.clone(),
        }))
    }
    fn remove_file(&self, path: &str) -> turso_core::Result<()> {
        self.inner.remove_file(path)
    }
    fn file_id(&self, path: &str) -> turso_core::Result<FileId> {
        self.inner.file_id(path)
    }
}
impl File for TracedFile {
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
        if self.wal {
            self.events.lock().unwrap().push("write");
        }
        self.inner.pwrite(pos, b, c)
    }
    fn sync(&self, c: Completion, kind: FileSyncType) -> turso_core::Result<Completion> {
        if self.wal {
            self.events.lock().unwrap().push("sync");
        }
        self.inner.sync(c, kind)
    }
    fn size(&self) -> turso_core::Result<u64> {
        self.inner.size()
    }
    fn truncate(&self, length: u64, c: Completion) -> turso_core::Result<Completion> {
        self.inner.truncate(length, c)
    }
}
fn main() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let io = Arc::new(TracedIo {
        inner: MemoryIO::new(),
        events: events.clone(),
    });
    {
        let db = turso_core::Database::open_file(io.clone(), "probe.db").unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE items(n INTEGER)").unwrap();
        c.execute("INSERT INTO items VALUES(1)").unwrap();
        c.execute("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    }
    let db = turso_core::Database::open_file(io, "probe.db").unwrap();
    let c = db.connect().unwrap();
    c.execute("PRAGMA synchronous=FULL").unwrap();
    c.execute("PRAGMA wal_autocheckpoint=0").unwrap();
    let mut failures = 0;
    for value in 2..=4 {
        events.lock().unwrap().clear();
        c.execute(format!("INSERT INTO items VALUES({value})"))
            .unwrap();
        let trace = events.lock().unwrap().clone();
        println!(
            "commit {value}, mode {:?}, WAL events: {trace:?}",
            c.get_sync_mode()
        );
        if trace.last() != Some(&"sync") {
            failures += 1;
        }
    }
    assert_eq!(
        failures, 0,
        "FULL must sync after the commit's final WAL write"
    );
}
