//! Proposed finite boundary cases, kept outside the release suite until review.
use romedb::{Database, Parameters, Value};
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc, Mutex,
};
use turso_core::io::{
    clock::{Clock, MonotonicInstant, WallClockInstant},
    FileSyncType, UnixIO,
};
use turso_core::{Buffer, Completion, CompletionError, File, OpenFlags, IO};

#[derive(Default)]
struct State {
    events: Mutex<Vec<&'static str>>,
    // 1: returned error; 2: errored completion; 3: deferred errored completion.
    fault: AtomicU8,
    pending: Mutex<Option<Completion>>,
}
struct TestIo {
    inner: UnixIO,
    state: Arc<State>,
}
struct TestFile {
    inner: Arc<dyn File>,
    state: Arc<State>,
    wal: bool,
}
fn injected() -> CompletionError {
    CompletionError::IOError(
        std::io::Error::from_raw_os_error(5).kind(),
        "injected WAL barrier",
    )
}
impl File for TestFile {
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
        self.state
            .events
            .lock()
            .unwrap()
            .push(if self.wal { "wal-write" } else { "db-write" });
        self.inner.pwrite(pos, b, c)
    }
    fn sync(&self, c: Completion, kind: FileSyncType) -> turso_core::Result<Completion> {
        if self.wal {
            match self.state.fault.swap(0, Ordering::SeqCst) {
                0 => (),
                1 => return Err(turso_core::LimboError::CompletionError(injected())),
                2 => {
                    c.error(injected());
                    return Ok(c);
                }
                3 => {
                    *self.state.pending.lock().unwrap() = Some(c.clone());
                    return Ok(c);
                }
                other => panic!("unknown fault {other}"),
            }
        }
        let result = self.inner.sync(c, kind)?;
        self.state
            .events
            .lock()
            .unwrap()
            .push(if self.wal { "wal-sync" } else { "db-sync" });
        Ok(result)
    }
    fn size(&self) -> turso_core::Result<u64> {
        self.inner.size()
    }
    fn truncate(&self, len: u64, c: Completion) -> turso_core::Result<Completion> {
        self.inner.truncate(len, c)
    }
}
impl Clock for TestIo {
    fn current_time_monotonic(&self) -> MonotonicInstant {
        self.inner.current_time_monotonic()
    }
    fn current_time_wall_clock(&self) -> WallClockInstant {
        self.inner.current_time_wall_clock()
    }
}
impl IO for TestIo {
    fn open_file(
        &self,
        path: &str,
        flags: OpenFlags,
        direct: bool,
    ) -> turso_core::Result<Arc<dyn File>> {
        Ok(Arc::new(TestFile {
            inner: self.inner.open_file(path, flags, direct)?,
            state: self.state.clone(),
            wal: path.ends_with("-wal"),
        }))
    }
    fn remove_file(&self, path: &str) -> turso_core::Result<()> {
        self.inner.remove_file(path)
    }
    fn step(&self) -> turso_core::Result<()> {
        if let Some(c) = self.state.pending.lock().unwrap().take() {
            c.error(injected());
        }
        self.inner.step()
    }
}
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).unwrap()
}
fn successful_checkpoint(c: &romedb::Connection) {
    assert_eq!(
        q(c, "PRAGMA wal_checkpoint(TRUNCATE)").rows,
        vec![vec![Value::Integer(0); 3]]
    );
}
fn fixture(path: &std::path::Path, state: Arc<State>) -> (Database, romedb::Connection) {
    let db = Database::open_with_io(
        path.to_str().unwrap(),
        Arc::new(TestIo {
            inner: UnixIO::new().unwrap(),
            state,
        }),
    )
    .unwrap();
    let c = db.connect().unwrap();
    q(&c, "PRAGMA synchronous=FULL");
    q(&c, "CREATE TABLE items(id INTEGER PRIMARY KEY,n INTEGER)");
    q(&c, "INSERT INTO items VALUES(1,1),(2,1)");
    successful_checkpoint(&c);
    (db, c)
}
fn assert_fault_delivered_without_backfill(state: &State, fault: u8) {
    assert_eq!(
        state.fault.load(Ordering::SeqCst),
        0,
        "fault {fault} must be reached"
    );
    assert!(
        state.pending.lock().unwrap().is_none(),
        "fault {fault} must complete"
    );
    assert!(
        !state.events.lock().unwrap().contains(&"db-write"),
        "fault {fault}: database must not be modified after a failed WAL barrier"
    );
}
fn assert_synced_before_backfill(state: &State, context: &str) {
    let events = state.events.lock().unwrap();
    let first_write = events
        .iter()
        .position(|event| *event == "db-write")
        .unwrap_or_else(|| panic!("{context}: checkpoint must backfill: {events:?}"));
    let sync = events[..first_write]
        .iter()
        .rposition(|event| *event == "wal-sync")
        .unwrap_or_else(|| panic!("{context}: WAL sync must precede backfill: {events:?}"));
    if let Some(write) = events[..first_write]
        .iter()
        .rposition(|event| *event == "wal-write")
    {
        assert!(
            sync > write,
            "{context}: WAL sync must cover current frames: {events:?}"
        );
    }
}
fn core_rows(c: &Arc<turso_core::Connection>, sql: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    c.prepare(sql)
        .unwrap()
        .run_with_row_callback(|row| {
            rows.push(row.get_values().map(|value| format!("{value}")).collect());
            Ok(())
        })
        .unwrap();
    rows
}
fn core_fixture(
    path: &std::path::Path,
    state: Arc<State>,
) -> (Arc<turso_core::Database>, Arc<turso_core::Connection>) {
    let db = turso_core::Database::open_file_with_flags(
        Arc::new(TestIo {
            inner: UnixIO::new().unwrap(),
            state,
        }),
        path.to_str().unwrap(),
        OpenFlags::default(),
        turso_core::DatabaseOpts::new(),
        None,
    )
    .unwrap();
    let c = db.connect().unwrap();
    c.execute("PRAGMA synchronous=FULL").unwrap();
    c.execute("CREATE TABLE items(id INTEGER PRIMARY KEY,n INTEGER)")
        .unwrap();
    c.execute("INSERT INTO items VALUES(1,1),(2,1)").unwrap();
    c.checkpoint(turso_core::CheckpointMode::Truncate {
        upper_bound_inclusive: None,
    })
    .unwrap();
    (db, c)
}

#[test]
fn failed_blocking_checkpoint_releases_state_and_retries_barrier() {
    for fault in [1, 2, 3] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blocking.db");
        let state = Arc::new(State::default());
        let (db, c) = core_fixture(&path, state.clone());
        c.execute("PRAGMA synchronous=NORMAL").unwrap();
        c.execute("UPDATE items SET n=2").unwrap();
        state.events.lock().unwrap().clear();
        state.fault.store(fault, Ordering::SeqCst);
        let error = c
            .checkpoint(turso_core::CheckpointMode::Truncate {
                upper_bound_inclusive: None,
            })
            .expect_err("direct blocking checkpoint must report the injected error");
        assert!(
            error.to_string().contains("injected WAL barrier"),
            "{error}"
        );
        assert_fault_delivered_without_backfill(&state, fault);
        state.events.lock().unwrap().clear();
        c.checkpoint(turso_core::CheckpointMode::Truncate {
            upper_bound_inclusive: None,
        })
        .unwrap();
        assert_synced_before_backfill(&state, &format!("blocking retry fault {fault}"));
        assert_eq!(
            core_rows(&c, "SELECT n FROM items ORDER BY id"),
            vec![vec!["2".to_string()]; 2]
        );
        c.execute("UPDATE items SET n=3").unwrap();
        c.checkpoint(turso_core::CheckpointMode::Truncate {
            upper_bound_inclusive: None,
        })
        .unwrap();
        assert_eq!(
            core_rows(&c, "PRAGMA integrity_check"),
            vec![vec!["ok".to_string()]]
        );
        drop(c);
        drop(db);
        let reopened = turso_core::Database::open_file(
            Arc::new(UnixIO::new().unwrap()),
            path.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(
            core_rows(
                &reopened.connect().unwrap(),
                "SELECT n FROM items ORDER BY id"
            ),
            vec![vec!["3".to_string()]; 2]
        );
    }
}

#[test]
fn failed_auto_checkpoint_preserves_commit_and_allows_barrier_retry() {
    for fault in [1, 2, 3] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("automatic.db");
        let state = Arc::new(State::default());
        let (db, c) = fixture(&path, state.clone());
        q(&c, "PRAGMA synchronous=NORMAL");
        // Initialize this WAL generation before arming the fault: the tested
        // sync must be the auto-checkpoint barrier, not WAL header creation.
        q(&c, "UPDATE items SET n=2");
        state.events.lock().unwrap().clear();
        state.fault.store(fault, Ordering::SeqCst);
        // This pin has a fixed threshold of 1000 frames and does not implement
        // PRAGMA wal_autocheckpoint. Bound the small, one-page commits and
        // require an actual injected checkpoint barrier before continuing.
        let mut expected = 2;
        for _ in 0..1100 {
            let result = c.execute("UPDATE items SET n=n+1", &Parameters::new());
            expected += 1;
            if state.fault.load(Ordering::SeqCst) == 0 {
                result.expect("automatic checkpoint failure must preserve the published commit");
                break;
            }
            result.expect("writes before the injected barrier must succeed");
        }
        assert_fault_delivered_without_backfill(&state, fault);
        assert!(
            state.events.lock().unwrap().contains(&"wal-write"),
            "fault must follow transaction writes"
        );
        // The transaction committed before the checkpoint started. Do not
        // replay this write after the checkpoint error or increment it twice.
        assert_eq!(
            q(&c, "SELECT n FROM items ORDER BY id").rows,
            vec![vec![Value::Integer(expected)]; 2]
        );
        state.events.lock().unwrap().clear();
        // Unbackfilled frames still exceed the threshold. One new write must
        // retry the automatic checkpoint, without replaying the prior write.
        q(&c, "UPDATE items SET n=n+1");
        expected += 1;
        assert_synced_before_backfill(&state, &format!("automatic checkpoint retry fault {fault}"));
        successful_checkpoint(&c);
        q(&c, "UPDATE items SET n=n+1");
        expected += 1;
        successful_checkpoint(&c);
        assert_eq!(
            q(&c, "SELECT n FROM items ORDER BY id").rows,
            vec![vec![Value::Integer(expected)]; 2]
        );
        assert_eq!(
            q(&c, "PRAGMA integrity_check").rows,
            vec![vec![Value::String("ok".into())]]
        );
        drop(c);
        drop(db);
        let reopened = Database::open(path.to_str().unwrap()).unwrap();
        assert_eq!(
            q(
                &reopened.connect().unwrap(),
                "SELECT n FROM items ORDER BY id"
            )
            .rows,
            vec![vec![Value::Integer(expected)]; 2]
        );
    }
}
#[test]
fn failed_wal_barrier_blocks_backfill_and_allows_checkpoint_retry() {
    for fault in [1, 2, 3] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.db");
        let state = Arc::new(State::default());
        let (db, c) = fixture(&path, state.clone());
        q(&c, "PRAGMA synchronous=NORMAL");
        q(&c, "UPDATE items SET n=2");
        state.events.lock().unwrap().clear();
        state.fault.store(fault, Ordering::SeqCst);
        let failed = c.execute("PRAGMA wal_checkpoint(TRUNCATE)", &Parameters::new());
        assert_eq!(
            state.fault.load(Ordering::SeqCst),
            0,
            "fault must be reached"
        );
        assert!(
            state.pending.lock().unwrap().is_none(),
            "deferred completion must be delivered"
        );
        match failed {
            Err(error) => assert!(
                error.to_string().contains("injected WAL barrier"),
                "{error}"
            ),
            Ok(result) => assert_eq!(
                result.rows,
                vec![vec![Value::Integer(1), Value::Null, Value::Null]]
            ),
        }
        assert!(
            !state.events.lock().unwrap().contains(&"db-write"),
            "fault {fault}: database must not be modified after a failed WAL barrier"
        );
        state.events.lock().unwrap().clear();
        successful_checkpoint(&c);
        let retry = state.events.lock().unwrap().clone();
        let first_write = retry
            .iter()
            .position(|event| *event == "db-write")
            .expect("retry must backfill");
        assert!(retry[..first_write].contains(&"wal-sync"),
            "fault {fault}: checkpoint retry must complete a fresh WAL sync before backfill: {retry:?}");
        assert_eq!(
            q(&c, "SELECT n FROM items ORDER BY id").rows,
            vec![vec![Value::Integer(2)]; 2]
        );
        q(&c, "UPDATE items SET n=3");
        successful_checkpoint(&c);
        assert_eq!(
            q(&c, "PRAGMA integrity_check").rows,
            vec![vec![Value::String("ok".into())]]
        );
        drop(c);
        drop(db);
        let reopened = Database::open(path.to_str().unwrap()).unwrap();
        assert_eq!(
            q(
                &reopened.connect().unwrap(),
                "SELECT n FROM items ORDER BY id"
            )
            .rows,
            vec![vec![Value::Integer(3)]; 2]
        );
    }
}
#[test]
fn off_checkpoint_skips_wal_barrier() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let (_db, c) = fixture(&dir.path().join("app.db"), state.clone());
    q(&c, "PRAGMA synchronous=OFF");
    q(&c, "UPDATE items SET n=2");
    state.events.lock().unwrap().clear();
    let result = q(&c, "PRAGMA wal_checkpoint(PASSIVE)");
    assert_eq!(result.rows[0][0], Value::Integer(0));
    let events = state.events.lock().unwrap();
    assert!(events.contains(&"db-write"));
    assert!(!events.contains(&"wal-sync"));
}
#[test]
fn empty_checkpoint_skips_wal_barrier() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(State::default());
    let (_db, c) = fixture(&dir.path().join("app.db"), state.clone());
    q(&c, "PRAGMA synchronous=NORMAL");
    state.events.lock().unwrap().clear();
    let result = q(&c, "PRAGMA wal_checkpoint(PASSIVE)");
    assert_eq!(result.rows[0][0], Value::Integer(0));
    let events = state.events.lock().unwrap();
    assert!(!events.contains(&"db-write"));
    assert!(!events.contains(&"wal-sync"));
}
