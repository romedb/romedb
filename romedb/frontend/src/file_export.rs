//! Copy an open MVCC database into a new, self-contained database file.
use crate::Result;
use crate::{canonical, quote, text, CancellationToken, Connection, Database, EngineValue, Error};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, AtomicU8, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};
use turso_core::Connection as EngineConnection;

/// What a copy leaves out. The default copies the whole database.
#[derive(Clone, Debug, Default)]
pub struct FileExportOptions {
    /// These tables keep their schema and indexes but no rows. Their rows are
    /// never read, so they cannot reach the file. A receipt can hold row data,
    /// so the copy also gets a new identity, as with `new_identity`. The caller
    /// must keep its commits from running while the copy starts: an index
    /// committed in that moment would not be left out.
    pub empty_tables: Vec<String>,
    /// Leave out the database ID, the sequence and the receipts. The copy is a
    /// new database: its sequence starts at 0, and a service gives it an ID the
    /// first time it opens the file.
    pub new_identity: bool,
}

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
        options: &FileExportOptions,
        token: &CancellationToken,
    ) -> Result<PendingFileExport> {
        let destination = std::path::absolute(destination).map_err(storage)?;
        let (Some(directory), Some(_)) = (destination.parent(), destination.file_name()) else {
            return Err(Error::Validation(
                "export destination must name a file".into(),
            ));
        };
        refuse_existing(&destination)?;
        self.start_copy(directory, Some(destination.clone()), options, token)
    }

    /// Like `start_file_export`, but the copy gets no name: `finish` returns
    /// it open and nothing is left in `directory`.
    pub fn start_file_snapshot(
        &self,
        directory: &Path,
        options: &FileExportOptions,
        token: &CancellationToken,
    ) -> Result<PendingFileExport> {
        self.start_copy(directory, None, options, token)
    }

    fn start_copy(
        &self,
        directory: &Path,
        destination: Option<PathBuf>,
        options: &FileExportOptions,
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
        let plan = if options.empty_tables.is_empty() && !options.new_identity {
            CopyPlan::default()
        } else {
            self.connect()?.copy_plan(options)?
        };
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
                    connection
                        .engine
                        .set_vacuum_into_tables_without_rows(plan.tables_without_rows);
                    let _ = started.send(connection.engine.clone());
                    let output = connection.with_cancellation(&token, || {
                        copy_with_retries(&connection, &folder, &snapshot, &token)
                    })?;
                    rebuild_indexes(&output, &plan.indexes_to_rebuild, &token)?;
                    Ok(output)
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

/// Which tables a copy leaves empty, and which indexes it then builds again.
#[derive(Default)]
struct CopyPlan {
    tables_without_rows: BTreeSet<String>,
    indexes_to_rebuild: Vec<String>,
}

impl Connection {
    fn copy_plan(&self, options: &FileExportOptions) -> Result<CopyPlan> {
        let mut plan = CopyPlan::default();
        let emptied = options
            .empty_tables
            .iter()
            .map(|name| canonical(name).map_err(|_| unknown_table(name)))
            .collect::<Result<BTreeSet<_>>>()?;
        for name in &emptied {
            self.plan_empty_table(name, &emptied, &mut plan)?;
        }
        let existing = self.table_names()?;
        if let Some(missing) = plan
            .tables_without_rows
            .iter()
            .find(|table| !existing.contains(*table))
        {
            return Err(Error::Storage(format!(
                "file export cannot find the storage table {missing}"
            )));
        }
        let left_empty = IDENTITY_TABLES
            .into_iter()
            .chain((!options.empty_tables.is_empty()).then_some(CHANGE_LOG_TABLE));
        plan.tables_without_rows.extend(
            left_empty
                .filter(|table| existing.contains(*table))
                .map(str::to_owned),
        );
        Ok(plan)
    }

    fn plan_empty_table(
        &self,
        name: &str,
        emptied: &BTreeSet<String>,
        plan: &mut CopyPlan,
    ) -> Result<()> {
        let collection = match self.catalog(name) {
            Ok(collection) => collection,
            Err(Error::NotFound(_)) => return self.plan_empty_native_table(name, plan),
            Err(error) => return Err(error),
        };
        if let Some(edges) = collection
            .edge_dependents
            .iter()
            .find(|edges| !emptied.contains(*edges))
        {
            return Err(Error::Validation(format!(
                "empty the edge table {edges} together with {name}"
            )));
        }
        plan.tables_without_rows.insert(collection.storage.clone());
        if let Some(edge) = &collection.edge {
            plan.tables_without_rows.insert(edge.adjacency.clone());
        }
        for index in &collection.indexes {
            plan.tables_without_rows.extend(index.storage_tables());
            plan.indexes_to_rebuild.push(index.name.clone());
        }
        Ok(())
    }

    fn plan_empty_native_table(&self, name: &str, plan: &mut CopyPlan) -> Result<()> {
        let rows = self.run(
            "SELECT name FROM main.sqlite_schema WHERE type='table' AND rootpage<>0 AND lower(name)=?1",
            &[text(name)],
        )?;
        let [row] = rows.as_slice() else {
            return Err(unknown_table(name));
        };
        let table = text_column(row)?;
        let methods = self.run(
            "SELECT name FROM main.sqlite_schema WHERE type='index' AND rootpage=0 AND tbl_name=?1",
            &[text(&table)],
        )?;
        if !methods.is_empty() {
            return Err(Error::Unsupported(format!(
                "table {table} has an index method that keeps rows in storage of its own"
            )));
        }
        plan.tables_without_rows.insert(table);
        Ok(())
    }

    fn table_names(&self) -> Result<BTreeSet<String>> {
        self.run(
            "SELECT name FROM main.sqlite_schema WHERE type='table'",
            &[],
        )?
        .iter()
        .map(|row| text_column(row))
        .collect()
    }
}

/// Building an index over an empty collection writes the state of an empty
/// index. Some index kinds need that as a row: a vector index keeps its graph in
/// one, and the copy left that row out.
fn rebuild_indexes(output: &Path, indexes: &[String], token: &CancellationToken) -> Result<()> {
    if indexes.is_empty() {
        return Ok(());
    }
    let path = output
        .to_str()
        .ok_or_else(|| Error::Validation("export folder path must be UTF-8".into()))?;
    let copy = Database::open_mvcc(path)?;
    let connection = copy.connect_without_schema_check()?;
    connection.with_cancellation(token, || {
        connection.atomic(|| {
            for index in indexes {
                connection.rebuild_index(index)?;
            }
            connection.validate_storage_schema()
        })?;
        connection.run("PRAGMA wal_checkpoint(TRUNCATE)", &[])?;
        Ok(())
    })
}

fn text_column(row: &[EngineValue]) -> Result<String> {
    match row {
        [EngineValue::Text(text), ..] => Ok(text.as_str().to_owned()),
        _ => Err(Error::Storage("schema name is not text".into())),
    }
}

fn unknown_table(name: &str) -> Error {
    Error::Validation(format!("no table named {} can be emptied", quote(name)))
}

const IDENTITY_TABLES: [&str; 2] = ["__romedb_service_state", "__romedb_service_receipts"];
/// With change capture on, this table holds rows from before and after each change.
const CHANGE_LOG_TABLE: &str = "turso_cdc";
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

#[cfg(test)]
mod tests {
    use crate::{Database, Parameters};
    use std::collections::BTreeSet;

    fn tables(c: &crate::Connection) -> BTreeSet<String> {
        c.table_names().unwrap()
    }

    #[test]
    fn storage_tables_lists_every_table_that_dropping_an_index_removes() {
        let database = Database::open(":memory:").unwrap();
        let c = database.connect().unwrap();
        c.execute("PRAGMA journal_mode=mvcc", &Parameters::new())
            .unwrap();
        for sql in [
            "CREATE TABLE docs",
            "CREATE INDEX scalar ON docs(n)",
            "CREATE UNIQUE INDEX compound ON docs(n,m)",
            "CREATE SEARCH INDEX array ON docs(team.*.role) USING ARRAY",
            "CREATE SEARCH INDEX text ON docs(body) USING FULLTEXT",
            "CREATE SEARCH INDEX near ON docs(v) USING VECTOR WITH (metric='l2',dimensions=2)",
            "CREATE SEARCH INDEX point ON docs(place) USING SPATIAL",
        ] {
            c.execute(sql, &Parameters::new()).expect(sql);
        }
        c.create_spatial_region_index("docs", "region", vec!["area".into()], false)
            .unwrap();
        let collection = c.catalog("docs").unwrap();
        assert_eq!(collection.indexes.len(), 7);
        for index in &collection.indexes {
            let before = tables(&c);
            let listed: BTreeSet<String> = index.storage_tables().into_iter().collect();
            assert!(listed.is_subset(&before), "{}: {listed:?}", index.name);
            assert!(c.drop_managed_index(&index.name).unwrap(), "{}", index.name);
            let removed: BTreeSet<String> = before.difference(&tables(&c)).cloned().collect();
            assert_eq!(listed, removed, "{}", index.name);
        }
    }
}
