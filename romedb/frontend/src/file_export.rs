//! Copy an open MVCC database into a new, self-contained database file.
use crate::{CancellationToken, Database, Error, Result};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, AtomicU8, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};
use turso_core::Connection as EngineConnection;

/// A copy whose snapshot has started and whose file is still being written.
pub struct PendingFileExport {
    worker: std::thread::JoinHandle<Result<PathBuf>>,
    workspace: Workspace,
    destination: Option<PathBuf>,
}

/// A finished copy, open for reading.
pub struct FileExport {
    pub file: std::fs::File,
    pub bytes: u64,
}

impl Database {
    /// Copy this database into `destination`, a new file that is private to
    /// this user. Readers and writers continue during the copy.
    ///
    /// Returns once the copy's snapshot has started: the file holds every
    /// commit that finished before this call started, and no commit that
    /// started after it returned. A caller that keeps its own commits from
    /// running during this call gets an exact cut. Call `finish` to wait for
    /// the file.
    pub fn start_file_export(
        &self,
        destination: &Path,
        token: &CancellationToken,
    ) -> Result<PendingFileExport> {
        let destination = std::path::absolute(destination).map_err(storage)?;
        let (Some(directory), Some(_)) = (destination.parent(), destination.file_name()) else {
            return Err(Error::Validation(
                "export destination must name a file".into(),
            ));
        };
        refuse_existing(&destination)?;
        self.start_copy(directory, Some(destination.clone()), token)
    }

    /// Like `start_file_export`, but the copy gets no name: `finish` returns
    /// it open and nothing is left in `directory`.
    pub fn start_file_snapshot(
        &self,
        directory: &Path,
        token: &CancellationToken,
    ) -> Result<PendingFileExport> {
        self.start_copy(directory, None, token)
    }

    fn start_copy(
        &self,
        directory: &Path,
        destination: Option<PathBuf>,
        token: &CancellationToken,
    ) -> Result<PendingFileExport> {
        if !self.engine.mvcc_enabled() {
            return Err(Error::Unsupported(
                "file export needs an MVCC database; run romedb migrate-to-mvcc first".into(),
            ));
        }
        if token.is_cancelled() {
            return Err(Error::Engine(turso_core::LimboError::Interrupt));
        }
        let workspace = Workspace::create(directory)?;
        let engine = self.engine.clone();
        let manual_wal = self.manual_wal;
        let folder = workspace.path.clone();
        let token = token.clone();
        let snapshot = Arc::new(AtomicU8::new(WAITING));
        let (started, connection) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("romedb-file-export".into())
            .spawn({
                let snapshot = snapshot.clone();
                move || {
                    let database = Database { engine, manual_wal };
                    let connection = database.connect()?;
                    let _ = started.send(connection.engine.clone());
                    connection.with_cancellation(&token, || {
                        copy_with_retries(&connection, &folder, &snapshot, &token)
                    })
                }
            })
            .map_err(storage)?;
        if let Ok(connection) = connection.recv() {
            wait_for_snapshot(&connection, &worker, &snapshot);
        }
        Ok(PendingFileExport {
            worker,
            workspace,
            destination,
        })
    }
}

impl PendingFileExport {
    /// Wait for the copy, then put it at its destination or return it unnamed.
    pub fn finish(self) -> Result<FileExport> {
        let output = self
            .worker
            .join()
            .map_err(|_| Error::Storage("file export thread failed".into()))??;
        for sidecar in sidecars(&output) {
            match std::fs::metadata(&sidecar) {
                Ok(metadata) if metadata.len() != 0 => {
                    return Err(Error::Storage(
                        "exported database still has log data beside it".into(),
                    ))
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(storage(error)),
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o600))
                .map_err(storage)?;
        }
        let file = std::fs::File::open(&output).map_err(storage)?;
        file.sync_all().map_err(storage)?;
        let bytes = file.metadata().map_err(storage)?.len();
        if let Some(destination) = &self.destination {
            std::fs::hard_link(&output, destination).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    Error::AlreadyExists("export destination already exists".into())
                } else {
                    storage(error)
                }
            })?;
        }
        self.workspace.remove()?;
        Ok(FileExport { file, bytes })
    }
}

const WAITING: u8 = 0;
const SEEN: u8 = 1;
const RETRYING: u8 = 2;
const BUSY_RETRY_TIME: Duration = Duration::from_secs(5);

fn refuse_existing(destination: &Path) -> Result<()> {
    for path in std::iter::once(destination.to_path_buf()).chain(sidecars(destination)) {
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(Error::AlreadyExists(
                    "export destination or one of its database sidecars already exists".into(),
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage(error)),
        }
    }
    Ok(())
}

fn sidecars(path: &Path) -> Vec<PathBuf> {
    let mut sidecars: Vec<PathBuf> = ["-wal", "-shm", "-log", "-journal"]
        .into_iter()
        .map(|suffix| {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            PathBuf::from(name)
        })
        .collect();
    sidecars.push(path.with_extension("db-log"));
    sidecars
}

fn wait_for_snapshot(
    connection: &EngineConnection,
    worker: &std::thread::JoinHandle<Result<PathBuf>>,
    snapshot: &AtomicU8,
) {
    loop {
        if !connection.get_auto_commit()
            && connection.has_main_mvcc_tx_for_wal_session()
            && snapshot
                .compare_exchange(WAITING, SEEN, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            return;
        }
        if worker.is_finished() {
            return;
        }
        std::thread::sleep(Duration::from_micros(100));
    }
}

fn copy_with_retries(
    connection: &crate::Connection,
    folder: &Path,
    snapshot: &AtomicU8,
    token: &CancellationToken,
) -> Result<PathBuf> {
    let started = Instant::now();
    for attempt in 1.. {
        let output = folder.join(format!("export-{attempt}.romedb"));
        let text = output
            .to_str()
            .filter(|text| !text.contains('\''))
            .ok_or_else(|| {
                Error::Validation("export folder path must be UTF-8 without single quotes".into())
            })?;
        match connection.run(&format!("VACUUM INTO '{text}'"), &[]) {
            Ok(_) => return Ok(output),
            Err(_) if token.is_cancelled() => {
                return Err(Error::Engine(turso_core::LimboError::Interrupt))
            }
            Err(Error::Engine(turso_core::LimboError::Busy))
                if started.elapsed() < BUSY_RETRY_TIME
                    && snapshot
                        .compare_exchange(WAITING, RETRYING, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok() =>
            {
                std::thread::sleep(Duration::from_millis(10));
                snapshot.store(WAITING, Ordering::SeqCst);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("export attempts are unbounded until the retry time ends")
}

struct Workspace {
    path: PathBuf,
    parent: PathBuf,
}

impl Workspace {
    fn create(directory: &Path) -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        let path = directory.join(format!(
            ".romedb-export-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::NotFound("export directory does not exist".into())
            } else {
                storage(error)
            }
        })?;
        Ok(Self {
            path,
            parent: directory.to_path_buf(),
        })
    }

    fn remove(self) -> Result<()> {
        std::fs::remove_dir_all(&self.path).map_err(storage)?;
        #[cfg(unix)]
        std::fs::File::open(&self.parent)
            .and_then(|parent| parent.sync_all())
            .map_err(storage)?;
        Ok(())
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn storage(error: std::io::Error) -> Error {
    Error::Storage(error.to_string())
}
