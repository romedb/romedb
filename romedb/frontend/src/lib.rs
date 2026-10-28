//! Embedded RomeDB frontend over the pinned Turso engine.
mod ann;
mod array_predicate;
mod budget;
mod bundled;
mod grouped;
mod udf;
mod unnest;
pub use budget::ResultLimits;
mod array_index;
mod catalog;
mod check;
mod collections;
mod computed;
mod deferred;
mod expression;
mod fetch_clause;
mod field_rules;
mod scalar_index;
mod schema_rules;
mod search_filter;
pub use field_rules::FieldOptions;
mod analyzer;
mod fulltext;
mod mutation_result;
mod reindex;
pub use analyzer::FullTextOptions;
mod functions;
mod graph;
pub use graph::GraphLimits;
mod guard;
mod integrity;
pub use integrity::{IntegrityLimits, IntegrityReport};
mod interrupt;
mod wal_replication;
mod wire_json;
pub use wal_replication::WalPosition;
#[doc(hidden)]
pub use wire_json::decode_wire_json;
mod links;
pub use ann::VectorIndexOptions;
pub use interrupt::{CancellationToken, InterruptHandle};
mod meter;
pub use meter::{
    CreateWork, CreateWorkLimits, DdlWork, DdlWorkLimits, InfoWork, InfoWorkLimits, MeteredCreate,
    MeteredDdl, MeteredInfo, MeteredRead, MeteredSchema, MeteredWrite, ReadWork, ReadWorkLimits,
    SchemaWork, SchemaWorkLimits, WriteWork, WriteWorkLimits,
};
mod migration;
pub use migration::{Migration, MigrationReport};
mod mutation;
mod omit;
mod patch;
mod path;
mod profile;
mod projection_path;
mod relations;
pub use profile::{ProfiledQuery, QueryMetrics};
mod select;
mod service_catalog;
mod spatial;
mod timeout;
pub use service_catalog::{ServiceCatalog, ServiceReceipt};
mod request;
mod request_wire;
pub use request::{RequestBudget, RequestStatementKind};
pub use request_wire::{
    PreparedRequestStatement, RequestEnvelope, RequestError, RequestStatement, RequestWorkLimits,
    MAX_SEQUENCE as MAX_REQUEST_SEQUENCE,
};
mod transaction;
mod transfer;
pub use transfer::TransferFormat;
mod update;
mod value;
mod vectors;
mod write;
use serde::{Deserialize, Serialize};
use std::{num::NonZeroUsize, sync::Arc};
pub use transaction::{BatchExecution, ExecutionReport, TransactionState};
use turso_core::{
    Connection as EngineConnection, Database as EngineDatabase, Value as EngineValue,
};
pub use value::{Document, Key, Parameters, Record, Value};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Syntax(#[from] romeql_parser::Error),
    #[error("engine: {0}")]
    Engine(#[from] turso_core::LimboError),
    #[error("encoding: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("validation: {0}")]
    Validation(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("parameter missing: {0}")]
    Parameter(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("resource limit: {0}")]
    Limit(String),
    #[error("expected exactly one row, got {0}")]
    Cardinality(usize),
    #[error("migration {version} at byte {offset}: {source}")]
    Migration {
        version: i64,
        offset: usize,
        source: Box<Error>,
    },
    #[error("rollback failed after {cause}: {rollback}")]
    Rollback { cause: String, rollback: String },
}
impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Syntax(_) => "FDB_SYNTAX",
            Self::Engine(turso_core::LimboError::Interrupt) => "FDB_CANCELLED",
            Self::Engine(turso_core::LimboError::Busy) => "FDB_BUSY",
            Self::Engine(turso_core::LimboError::BusySnapshot) => "FDB_BUSY_SNAPSHOT",
            Self::Engine(
                turso_core::LimboError::WriteWriteConflict
                | turso_core::LimboError::CommitDependencyAborted,
            ) => "FDB_WRITE_CONFLICT",
            Self::Engine(
                turso_core::LimboError::Constraint(_)
                | turso_core::LimboError::ForeignKeyConstraint(_)
                | turso_core::LimboError::Raise(..),
            ) => "FDB_CONSTRAINT",
            Self::Engine(turso_core::LimboError::ExtensionError(message))
                if message.starts_with("__romedb_udf_limit:") =>
            {
                "FDB_LIMIT"
            }
            Self::Engine(turso_core::LimboError::ExtensionError(message))
                if message.starts_with("__romedb_udf_cancelled:") =>
            {
                "FDB_CANCELLED"
            }
            Self::Engine(turso_core::LimboError::ExtensionError(message))
                if message.starts_with("__romedb_udf_validation:") =>
            {
                "FDB_VALIDATION"
            }
            Self::Engine(_) => "FDB_ENGINE",
            Self::Encoding(_) | Self::Storage(_) => "FDB_STORAGE",
            Self::Validation(_) => "FDB_VALIDATION",
            Self::NotFound(_) => "FDB_NOT_FOUND",
            Self::AlreadyExists(_) => "FDB_ALREADY_EXISTS",
            Self::Unsupported(_) => "FDB_UNSUPPORTED",
            Self::Parameter(_) => "FDB_PARAMETER",
            Self::Limit(_) => "FDB_LIMIT",
            Self::Cardinality(_) => "FDB_CARDINALITY",
            Self::Migration { source, .. } if source.code() == "FDB_CANCELLED" => "FDB_CANCELLED",
            Self::Migration { .. } => "FDB_MIGRATION",
            Self::Rollback { .. } => "FDB_ROLLBACK",
        }
    }

    pub fn requires_exclusive_transaction(&self) -> bool {
        matches!(
            self,
            Self::Engine(turso_core::LimboError::TxError(message))
                if message.starts_with("DDL statements require an exclusive transaction")
        )
    }
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub affected: i64,
}
impl QueryResult {
    pub fn all(self) -> Vec<Vec<Value>> {
        self.rows
    }
    pub fn first(self) -> Option<Vec<Value>> {
        self.rows.into_iter().next()
    }
    pub fn exactly_one(mut self) -> Result<Vec<Value>> {
        if self.rows.len() != 1 {
            return Err(Error::Cardinality(self.rows.len()));
        }
        Ok(self.rows.remove(0))
    }
    fn documents(docs: Vec<Document>, affected: i64) -> Self {
        Self {
            columns: vec!["document".into()],
            rows: docs.into_iter().map(|d| vec![Value::Object(d)]).collect(),
            affected,
        }
    }
    fn command(affected: i64) -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            affected,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FieldType {
    String,
    Integer,
    Number,
    Boolean,
    Object,
    Array,
    Record(String),
    Vector(usize),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Field {
    pub path: Vec<String>,
    pub kind: FieldType,
    pub required: bool,
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IndexKind {
    #[default]
    Scalar,
    Array,
    Spatial,
    FullText,
    Vector,
}
impl IndexKind {
    fn is_scalar(&self) -> bool {
        *self == Self::Scalar
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Index {
    #[serde(default, skip_serializing_if = "IndexKind::is_scalar")]
    kind: IndexKind,
    name: String,
    path: Vec<String>,
    unique: bool,
    storage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fulltext: Option<fulltext::Config>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vector: Option<ann::Config>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scalar: Option<scalar_index::Config>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spatial: Option<spatial::Config>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Collection {
    #[serde(default = "catalog::legacy_version")]
    version: u32,
    name: String,
    storage: String,
    fields: Vec<Field>,
    indexes: Vec<Index>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    relations: Vec<relations::Relation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    field_policies: Vec<field_rules::Policy>,
    #[serde(default, skip_serializing_if = "field_rules::is_false")]
    strict: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    edge: Option<graph::Edge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    edge_dependents: Vec<String>,
}

pub struct Database {
    engine: Arc<EngineDatabase>,
    manual_wal: bool,
}
impl Database {
    pub fn open(path: &str) -> Result<Self> {
        Self::open_internal(path, false)
    }
    /// Open with caller-provided storage and ordinary automatic WAL management.
    ///
    /// The backend must implement the engine I/O contract, including file
    /// identity, locking, completion and durability semantics. Use this for
    /// embedded hosts with their own storage adapter. This method does not make
    /// an asynchronous backend synchronous or provide durability beyond that
    /// backend's guarantees.
    pub fn open_with_io(path: &str, io: Arc<dyn turso_core::IO>) -> Result<Self> {
        Self::open_internal_io(path, false, io)
    }
    /// Open a single-owner database whose caller manages WAL retention.
    ///
    /// Disables automatic checkpointing and WAL restart on every connection,
    /// including catalog initialization. Explicit checkpoint SQL remains available.
    /// The caller must bound WAL growth, coordinate all access, and publish remote
    /// commits before acknowledging writes. This method alone provides no remote
    /// durability. Never mix ordinary and manual-WAL opens of the same file.
    pub fn open_with_manual_wal(path: &str) -> Result<Self> {
        Self::open_internal(path, true)
    }
    fn open_internal(path: &str, manual_wal: bool) -> Result<Self> {
        let io = EngineDatabase::io_for_path(path)?;
        Self::open_internal_io(path, manual_wal, io)
    }
    /// Open a manually managed WAL database with caller-provided storage I/O.
    /// The caller must preserve the engine I/O contract and all single-owner WAL
    /// requirements of `open_with_manual_wal`. Useful for bounded or instrumented I/O.
    pub fn open_with_manual_wal_and_io(path: &str, io: Arc<dyn turso_core::IO>) -> Result<Self> {
        Self::open_internal_io(path, true, io)
    }
    fn open_internal_io(path: &str, manual_wal: bool, io: Arc<dyn turso_core::IO>) -> Result<Self> {
        let engine = EngineDatabase::open_file_with_flags(
            io,
            path,
            turso_core::OpenFlags::default(),
            turso_core::DatabaseOpts::new().with_index_method(true),
            None,
            Arc::new(turso_core::SqliteDialect),
        )?;
        Self::initialize(engine, manual_wal)
    }

    /// Open a caller-managed MVCC database with a custom commit durability hook.
    ///
    /// The caller must exclusively own this path, restore its complete published
    /// checkpoint/log pair before opening, and fence all access after uncertain
    /// publication. Never reopen a live path with a different storage adapter:
    /// the engine registry shares existing handles. This enables MVCC before
    /// catalog initialization, so the hook must also handle bootstrap writes.
    /// Existing WAL databases must be drained
    /// and migrated in isolation; this is not an online conversion API.
    /// Automatic checkpointing is disabled. The caller owns checkpoint publication
    /// and log retention, including the lifetime of snapshots and active readers.
    pub fn open_with_manual_mvcc_and_io(
        path: &str,
        io: Arc<dyn turso_core::IO>,
        storage: Arc<dyn turso_core::mvcc::persistent_storage::DurableStorage>,
    ) -> Result<Self> {
        storage.set_checkpoint_threshold(-1);
        let engine = EngineDatabase::open(
            io,
            path,
            turso_core::OpenOptions::new(Arc::new(turso_core::SqliteDialect))
                .db_opts(turso_core::DatabaseOpts::new().with_index_method(true))
                .durable_storage(storage),
        )?;
        let conn = engine.connect()?;
        conn.wal_auto_actions_disable();
        conn.set_portable_logical_changes_enabled(true);
        conn.execute("PRAGMA journal_mode=mvcc")?;
        conn.execute("PRAGMA mvcc_checkpoint_threshold=-1")?;
        Self::initialize(engine, true)
    }

    fn initialize(engine: Arc<EngineDatabase>, manual_wal: bool) -> Result<Self> {
        let conn = engine.connect()?;
        if manual_wal {
            conn.wal_auto_actions_disable();
        }
        conn.execute("CREATE TABLE IF NOT EXISTS __romedb_catalog (name TEXT PRIMARY KEY, metadata TEXT NOT NULL)")?;
        conn.execute(udf::DDL)?;
        Ok(Self { engine, manual_wal })
    }
    pub fn connect(&self) -> Result<Connection> {
        let engine = self.engine.connect()?;
        if self.manual_wal {
            engine.wal_auto_actions_disable();
        }
        let connection = Connection {
            engine,
            manual_wal: self.manual_wal,
            next_atomic_id: std::sync::atomic::AtomicU64::new(0),
            next_subquery_id: std::sync::atomic::AtomicU64::new(0),
            write_buffer_limits: None,
            ann_cache: Default::default(),
            work_meter: Default::default(),
            cancellation: Default::default(),
            meter_catalog_reads: Default::default(),
            meter_schema_changes: Default::default(),
            graph_limits: Default::default(),
            graph_counters: Default::default(),
            graph_plan: Default::default(),
            spatial_counters: Default::default(),
        };
        functions::register(&connection)?;
        connection.atomic(|| connection.validate_storage_schema())?;
        Ok(connection)
    }
}
/// Connections expose only checked frontend operations, never raw engine handles.
pub struct Connection {
    engine: Arc<EngineConnection>,
    manual_wal: bool,
    next_atomic_id: std::sync::atomic::AtomicU64,
    next_subquery_id: std::sync::atomic::AtomicU64,
    write_buffer_limits: Option<ResultLimits>,
    ann_cache: std::sync::Mutex<Option<ann::Cache>>,
    work_meter: std::sync::Mutex<Option<Arc<turso_core::execution_meter::ExecutionMeter>>>,
    cancellation: std::sync::Mutex<Option<Arc<interrupt::ActiveCancellation>>>,
    meter_catalog_reads: std::sync::atomic::AtomicBool,
    meter_schema_changes: std::sync::atomic::AtomicBool,
    graph_limits: std::sync::Mutex<GraphLimits>,
    graph_counters: std::sync::Mutex<graph::GraphCounters>,
    graph_plan: std::sync::Mutex<Vec<String>>,
    spatial_counters: std::sync::Mutex<spatial::Counters>,
}
fn retain_write_document(
    budget: &mut budget::ResultBudget,
    documents: &mut Vec<mutation_result::Snapshot>,
    document: mutation_result::Snapshot,
) -> Result<()> {
    document.charge(budget)?;
    documents.push(document);
    Ok(())
}
fn text(value: &str) -> EngineValue {
    EngineValue::Text(value.to_owned().into())
}
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn canonical(name: &str) -> Result<String> {
    if name.is_empty()
        || name.contains('\0')
        || guard::reserved_name(name)
        || name.to_ascii_lowercase().starts_with("sqlite_")
    {
        return Err(Error::Validation("invalid or reserved name".into()));
    }
    Ok(name.to_ascii_lowercase())
}
impl Connection {
    /// Attach a bounded host request identity to this connection's MVCC commits.
    ///
    /// The value is stored in the portable logical-log metadata under
    /// `romedb.commit_tag`. It does not alter SQL, authorize an operation, or
    /// deduplicate requests. Set it on each request connection before its commit;
    /// use `None` to clear it. Hosts must not use thread identity to associate
    /// logical-log frames with requests because group commit can change threads.
    pub fn set_commit_tag(&self, tag: Option<&str>) -> Result<()> {
        if tag.is_some_and(|tag| tag.is_empty() || tag.len() > 128 || tag.contains('\0')) {
            return Err(Error::Validation("invalid commit tag".into()));
        }
        self.engine.set_portable_logical_changes_enabled(true);
        self.engine
            .set_mvcc_log_meta("romedb.commit_tag".into(), tag.map(str::to_owned));
        Ok(())
    }

    fn prepare(&self, sql: impl AsRef<str>) -> turso_core::Result<turso_core::Statement> {
        parser_stack(|| self.engine.prepare(sql))
    }

    fn run(&self, sql: &str, params: &[EngineValue]) -> Result<Vec<Vec<EngineValue>>> {
        let mut statement = self.prepare(sql)?;
        for (i, value) in params.iter().enumerate() {
            statement.bind_at(
                NonZeroUsize::new(i + 1).expect("one-based parameter"),
                value.clone(),
            )?;
        }
        // Managed schema changes emit trusted CREATE/DROP statements for physical
        // storage/indexes. Charge their schema/VM work, never logical mutations.
        if self
            .meter_schema_changes
            .load(std::sync::atomic::Ordering::Relaxed)
            && (sql.starts_with("CREATE ") || sql.starts_with("DROP "))
        {
            return self.meter_schema_statement(&mut statement, collect_rows);
        }
        // INFO opts catalog reads into its infrastructure meter. Transaction
        // setup/cleanup must remain outside the exhausted execution budget.
        if self
            .meter_catalog_reads
            .load(std::sync::atomic::Ordering::Relaxed)
            && (sql.starts_with("SELECT ") || sql.starts_with("PRAGMA "))
        {
            self.meter_statement(&mut statement, collect_rows)
        } else {
            collect_rows(&mut statement)
        }
    }
    fn atomic<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.savepoint(f, |_| true)
    }
    fn interruptible_write<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        // Autocommit already has a full-transaction rollback boundary. Keep its
        // native commit/error behavior; only protect an existing caller scope.
        if self.engine.get_auto_commit() {
            return f();
        }
        // Native plain writes can omit the engine statement journal. Supply a
        // rollback boundary for interrupts without changing OR FAIL/ROLLBACK
        // and other native error dispositions.
        self.savepoint(f, |error| {
            matches!(error.code(), "FDB_CANCELLED" | "FDB_ROLLBACK")
        })
    }
    /// Run `f` in a uniquely named savepoint. The whole operation runs on a
    /// grown stack, so nested statement preparation does not allocate a new
    /// stack segment for each call.
    fn savepoint<T>(
        &self,
        f: impl FnOnce() -> Result<T>,
        rollback_on_error: impl FnOnce(&Error) -> bool,
    ) -> Result<T> {
        parser_stack(|| self.savepoint_on_stack(f, rollback_on_error))
    }
    fn savepoint_on_stack<T>(
        &self,
        f: impl FnOnce() -> Result<T>,
        rollback_on_error: impl FnOnce(&Error) -> bool,
    ) -> Result<T> {
        // A failed inner SAVEPOINT may already exist when interruption is
        // reported. Unique names let outer cleanup roll back its own frame.
        let id = self
            .next_atomic_id
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |value| value.checked_add(1),
            )
            .map_err(|_| Error::Limit("atomic operation identifiers exhausted".into()))?;
        let name = format!("__romedb_statement_{id}");
        if let Err(cause) = self.run(&format!("SAVEPOINT {name}"), &[]) {
            if self.engine.get_auto_commit() {
                return Err(cause);
            }
            // The callback has not run. Remove an opened frame, or accept the
            // pinned engine's exact missing-frame error if opening never happened.
            let rollback = self.cancellation_cleanup(|| {
                self.run(&format!("ROLLBACK TO {name}"), &[])
                    .and_then(|_| self.run(&format!("RELEASE {name}"), &[]))
            });
            return match rollback {
                Ok(_) => Err(cause),
                Err(Error::Engine(turso_core::LimboError::TxError(message)))
                    if message == format!("no such savepoint: {name}") =>
                {
                    Err(cause)
                }
                Err(rollback) => Err(Error::Rollback {
                    cause: cause.to_string(),
                    rollback: rollback.to_string(),
                }),
            };
        }
        let operation = f();
        let should_rollback = match &operation {
            Err(error) => rollback_on_error(error),
            Ok(_) => true, // A failed RELEASE must use the ordinary cleanup path.
        };
        let result = operation.and_then(|v| {
            self.check_cancellation()?;
            self.cancellation_cleanup(|| self.run(&format!("RELEASE {name}"), &[]))?;
            Ok(v)
        });
        match result {
            Ok(value) => Ok(value),
            Err(cause) => {
                if self.engine.get_auto_commit() {
                    return Err(cause);
                }
                let rollback = self.cancellation_cleanup(|| {
                    if should_rollback {
                        self.run(&format!("ROLLBACK TO {name}"), &[])
                            .and_then(|_| self.run(&format!("RELEASE {name}"), &[]))
                    } else {
                        self.run(&format!("RELEASE {name}"), &[])
                    }
                });
                match rollback {
                    Ok(_) => Err(cause),
                    Err(rollback) => Err(Error::Rollback {
                        cause: cause.to_string(),
                        rollback: rollback.to_string(),
                    }),
                }
            }
        }
    }
    fn catalog(&self, name: &str) -> Result<Collection> {
        let name = canonical(name)?;
        let rows = self.run(
            "SELECT metadata FROM __romedb_catalog WHERE name = ?1",
            &[text(&name)],
        )?;
        match rows.first().and_then(|r| r.first()) {
            Some(EngineValue::Text(t)) => catalog::decode(t.as_str(), &name),
            None => Err(Error::NotFound(name)),
            _ => Err(Error::Storage("invalid collection metadata".into())),
        }
    }
    // Call inside the write's atomic scope: failed writes also undo creation.
    fn collection_for_write(&self, name: &str) -> Result<Collection> {
        match self.catalog(name) {
            Ok(collection) => Ok(collection),
            Err(Error::NotFound(_)) => {
                self.create_collection(name, false)?;
                self.catalog(name)
            }
            Err(error) => Err(error),
        }
    }
    fn save_catalog(&self, collection: &Collection) -> Result<()> {
        let mut collection = collection.clone();
        collection.version = if collection
            .indexes
            .iter()
            .any(|index| index.spatial.is_some())
        {
            collection.version.max(spatial::CATALOG_VERSION)
        } else if collection.edge.is_some() || !collection.edge_dependents.is_empty() {
            collection.version.max(graph::CATALOG_VERSION)
        } else if collection.strict
            || !collection.field_policies.is_empty()
            || collection
                .indexes
                .iter()
                .any(|index| index.kind == IndexKind::Array)
            || collection
                .indexes
                .iter()
                .any(|index| index.scalar.is_some())
            || collection.indexes.iter().any(|index| {
                index
                    .fulltext
                    .as_ref()
                    .is_some_and(|config| config.is_custom())
            })
            || collection.indexes.iter().any(|index| {
                index
                    .vector
                    .as_ref()
                    .is_some_and(|config| config.compressed())
            })
        {
            collection.version.max(5)
        } else if collection.indexes.iter().any(|index| {
            index
                .fulltext
                .as_ref()
                .is_some_and(|config| config.storage_version == 2)
        }) {
            collection.version.max(4)
        } else if !collection.relations.is_empty()
            || collection
                .indexes
                .iter()
                .any(|index| index.kind != IndexKind::Scalar)
        {
            collection.version.max(3)
        } else {
            collection.version.max(catalog::version())
        };
        if collection
            .indexes
            .iter()
            .any(|index| array_index::steps_into_arrays(&index.path))
        {
            collection.version = collection
                .version
                .max(array_index::ARRAY_PATH_CATALOG_VERSION);
        }
        self.run(
            "UPDATE __romedb_catalog SET metadata = ?1 WHERE name = ?2",
            &[
                text(&serde_json::to_string(&collection)?),
                text(&collection.name),
            ],
        )?;
        Ok(())
    }
    pub fn create_collection(&self, name: &str, if_not_exists: bool) -> Result<()> {
        let name = canonical(name)?;
        self.atomic(|| {
            let exists = !self.run("SELECT name FROM sqlite_schema WHERE name = ?1 COLLATE NOCASE UNION ALL SELECT name FROM __romedb_catalog WHERE name = ?1", &[text(&name)])?.is_empty();
            if exists { return if if_not_exists { Ok(()) } else { Err(Error::AlreadyExists(name.clone())) }; }
            // Names are collision-free UTF-8 hex, independent of user quoting.
            let storage = format!("__romedb_c_{}", name.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>());
            self.run(&format!("CREATE TABLE {} (id BLOB PRIMARY KEY, doc BLOB NOT NULL)", quote(&storage)), &[])?;
            let collection = Collection { version:catalog::version(), name: name.clone(), storage, fields: Vec::new(), indexes: Vec::new(), relations: Vec::new(), field_policies: Vec::new(), strict: false, edge: None, edge_dependents: Vec::new() };
            self.run("INSERT INTO __romedb_catalog VALUES (?1, ?2)", &[text(&name), text(&serde_json::to_string(&collection)?)])?; Ok(())
        })
    }
    fn documents(&self, collection: &Collection) -> Result<Vec<Document>> {
        self.run_customer(
            &format!("SELECT doc FROM {}", quote(&collection.storage)),
            &[],
        )?
        .into_iter()
        .map(|row| decode_document(&row[0]))
        .collect()
    }
    pub fn get(&self, record: &Record) -> Result<Option<Document>> {
        let collection = self.catalog(&record.table)?;
        self.get_in(&collection, record)
    }
    fn get_in(&self, c: &Collection, record: &Record) -> Result<Option<Document>> {
        let id = normalized_id(record, &c.name)?;
        self.run_customer(
            &format!("SELECT doc FROM {} WHERE id = ?1", quote(&c.storage)),
            &[EngineValue::Blob(Value::Record(id).encode()?)],
        )?
        .first()
        .map(|r| decode_document(&r[0]))
        .transpose()
    }
    pub fn define_field(&self, table: &str, field: Field, overwrite: bool) -> Result<()> {
        self.define_field_with_options(table, field, FieldOptions::default(), overwrite)
    }
    pub fn define_field_with_options(
        &self,
        table: &str,
        field: Field,
        options: FieldOptions,
        overwrite: bool,
    ) -> Result<()> {
        validate_path(&field.path)?;
        if let FieldType::Vector(dims) = field.kind {
            vectors::validate_dimension(dims)?;
        }
        if field.path[0] == "id" {
            return Err(Error::Validation("id has a fixed type".into()));
        }
        self.graph_check_field(table, &field.path)?;
        if let FieldType::Record(target) = &field.kind {
            canonical(target)?;
        }
        self.check_definition(&field)?;
        field_rules::validate_options(&field, &options)?;
        self.atomic(|| {
            let mut c = self.catalog(table)?;
            let existing = c.fields.iter().position(|f| f.path == field.path);
            match (existing, overwrite) {
                (Some(i), true) => {
                    c.fields[i] = field.clone();
                }
                (None, false) => c.fields.push(field.clone()),
                (Some(_), false) => return Err(Error::AlreadyExists(field.path.join("."))),
                (None, true) => return Err(Error::NotFound(field.path.join("."))),
            }
            c.field_policies.retain(|policy| policy.path != field.path);
            if !options.is_empty() {
                c.field_policies.push(field_rules::Policy {
                    path: field.path.clone(),
                    options: options.clone(),
                });
                c.version = c.version.max(5);
            }
            field_rules::validate_catalog(&c)?;
            for index in &c.indexes {
                index.validate_vector_config(&c)?;
                for path in index.paths() {
                    catalog::compatible_index(&c, path, index.kind)?;
                }
            }
            for doc in self.documents(&c)? {
                self.validate_candidate(&c, &doc)?;
                self.validate_computed_fields(&c, &doc)?;
            }
            self.save_catalog(&c)
        })
    }
    pub fn create_index(
        &self,
        table: &str,
        name: &str,
        path: Vec<String>,
        unique: bool,
    ) -> Result<()> {
        self.create_index_if(table, name, path, unique, false)
    }
    pub fn create_index_if(
        &self,
        table: &str,
        name: &str,
        path: Vec<String>,
        unique: bool,
        if_not_exists: bool,
    ) -> Result<()> {
        self.create_index_kind(table, name, path, unique, if_not_exists, IndexKind::Scalar)
    }
    /// Build an index for two-dimensional points. Missing/null values are excluded from searches.
    pub fn create_spatial_index(
        &self,
        table: &str,
        name: &str,
        path: Vec<String>,
        if_not_exists: bool,
    ) -> Result<()> {
        self.create_index_kind(table, name, path, false, if_not_exists, IndexKind::Spatial)
    }
    /// Build an index of Polygon and MultiPolygon values for finding the
    /// stored regions that contain a point. Missing and null values are excluded.
    pub fn create_spatial_region_index(
        &self,
        table: &str,
        name: &str,
        path: Vec<String>,
        if_not_exists: bool,
    ) -> Result<()> {
        self.create_index_spatial(
            table,
            name,
            vec![path],
            false,
            if_not_exists,
            IndexKind::Spatial,
            Some(spatial::Config::regions()),
        )
    }
    fn create_index_kind(
        &self,
        table: &str,
        name: &str,
        path: Vec<String>,
        unique: bool,
        if_not_exists: bool,
        kind: IndexKind,
    ) -> Result<()> {
        self.create_index_paths(table, name, vec![path], unique, if_not_exists, kind)
    }
    fn create_index_paths(
        &self,
        table: &str,
        name: &str,
        paths: Vec<Vec<String>>,
        unique: bool,
        if_not_exists: bool,
        kind: IndexKind,
    ) -> Result<()> {
        let spatial = (kind == IndexKind::Spatial).then(spatial::Config::current);
        self.create_index_spatial(table, name, paths, unique, if_not_exists, kind, spatial)
    }
    #[allow(clippy::too_many_arguments)]
    fn create_index_spatial(
        &self,
        table: &str,
        name: &str,
        paths: Vec<Vec<String>>,
        unique: bool,
        if_not_exists: bool,
        kind: IndexKind,
        spatial: Option<spatial::Config>,
    ) -> Result<()> {
        if paths.is_empty() || paths.len() > 16 || (kind != IndexKind::Scalar && paths.len() != 1) {
            return Err(Error::Validation(
                "scalar indexes require 1..16 paths".into(),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for path in &paths {
            validate_path(path)?;
            if !seen.insert(path) {
                return Err(Error::Validation("duplicate index path".into()));
            }
            if array_index::steps_into_arrays(path) && kind != IndexKind::Array {
                return Err(Error::Validation(
                    "only array indexes accept * path steps".into(),
                ));
            }
            array_index::validate_array_path(path)?;
        }
        let path = paths[0].clone();
        let name = canonical(name)?;
        self.atomic(|| {
            let mut c = self.catalog(table)?;
            let existing = self.run(
                "SELECT type FROM sqlite_schema WHERE name=?1 COLLATE NOCASE",
                &[text(&name)],
            )?;
            if !existing.is_empty() {
                if matches!(kind, IndexKind::Spatial | IndexKind::Array)
                    && if_not_exists
                    && c.indexes.iter().any(|i| {
                        i.name == name
                            && i.kind == kind
                            && i.path == path
                            && i.region_index() == spatial.as_ref().is_some_and(|s| s.regions)
                    })
                {
                    return Ok(());
                }
                if kind == IndexKind::Scalar
                    && if_not_exists
                    && matches!(&existing[0][0],EngineValue::Text(t) if t.as_str()=="index")
                {
                    return Ok(());
                }
                return Err(Error::AlreadyExists(name.clone()));
            }
            if !self
                .run(
                    "SELECT name FROM __romedb_catalog WHERE name=?1",
                    &[text(&name)],
                )?
                .is_empty()
            {
                return Err(Error::AlreadyExists(name.clone()));
            }
            for path in &paths {
                catalog::compatible_index(&c, path, kind)?;
            }
            let storage = format!(
                "__romedb_i_{}",
                name.as_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            );
            let index = Index {
                kind,
                name: name.clone(),
                path: path.clone(),
                unique,
                storage,
                fulltext: None,
                vector: None,
                scalar: (paths.len() > 1).then(|| scalar_index::Config {
                    additional_paths: paths[1..].to_vec(),
                }),
                spatial,
            };
            index.validate_scalar_config()?;
            self.run(&index.table_ddl(), &[])?;
            self.run(&index.index_ddl(), &[])?;
            for object in index.extra_schema() {
                self.run(&object.ddl, &[])?;
            }
            for doc in self.documents(&c)? {
                self.insert_index(&index, &doc)?;
            }
            c.indexes.push(index);
            self.save_catalog(&c)
        })
    }
    fn insert_index(&self, index: &Index, doc: &Document) -> Result<()> {
        index.require_current_text_storage()?;
        if index.kind == IndexKind::Vector {
            return self.insert_vector_entry(index, doc);
        }
        if index.kind == IndexKind::Array {
            return self.insert_array_entries(index, doc);
        }
        if index.region_index() {
            return self.insert_region_entries(index, doc);
        }
        let mut values = index.document_keys(doc)?;
        let id = doc
            .get("id")
            .ok_or_else(|| Error::Storage("document has no id".into()))?;
        values.insert(1, EngineValue::Blob(id.encode()?));
        let slots = (1..=values.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        self.run_index_maintenance(
            &format!("INSERT INTO {} VALUES ({slots})", quote(&index.storage)),
            &values,
        )?;
        if index.kind == IndexKind::FullText {
            self.change_text_count(index, 1)?;
        }
        Ok(())
    }
    pub fn insert(&self, table: &str, doc: Document) -> Result<Document> {
        self.insert_with_replace(table, doc, false)
    }
    fn insert_with_replace(
        &self,
        table: &str,
        mut doc: Document,
        replace: bool,
    ) -> Result<Document> {
        self.atomic(|| {
            let c = self.collection_for_write(table)?;
            self.prepare_insert_document(&c, &mut doc)?;
            if replace {
                if let Some(Value::Record(record)) = doc.get("id") {
                    if let Some(previous) = self.get_in(&c, record)? {
                        field_rules::readonly(&c, &previous, &doc)?;
                        self.graph_check_edge_update(&c, &doc)?;
                        self.delete_document(&c, &previous)?;
                    }
                }
                self.delete_unique_conflicts(&c, &doc)?;
            }
            self.store_insert_document(&c, &doc)?;
            Ok(doc)
        })
    }
    /// Insert into a collection whose metadata the caller already read, inside
    /// a savepoint the caller owns and rolls back on any error.
    pub(crate) fn insert_in_transaction(
        &self,
        c: &Collection,
        mut doc: Document,
    ) -> Result<Document> {
        self.prepare_insert_document(c, &mut doc)?;
        self.store_insert_document(c, &doc)?;
        Ok(doc)
    }
    fn prepare_insert_document(&self, c: &Collection, doc: &mut Document) -> Result<()> {
        if !doc.contains_key("id") {
            let rows = self.run_customer("SELECT uuid7_str()", &[])?;
            let Some(EngineValue::Text(key)) = rows.first().and_then(|r| r.first()) else {
                return Err(Error::Storage("UUID generator returned non-text".into()));
            };
            doc.insert(
                "id".into(),
                Value::Record(Record {
                    table: c.name.clone(),
                    key: Key::String(key.as_str().into()),
                }),
            );
        }
        normalize_document_id(c, doc)?;
        if c.edge.is_some() {
            self.graph_normalize_edge(c, doc)?;
        }
        field_rules::defaults(c, doc)?;
        self.compute_fields(c, doc)?;
        self.validate_candidate(c, doc)?;
        Ok(())
    }
    fn store_insert_document(&self, c: &Collection, doc: &Document) -> Result<()> {
        self.graph_check_new_edge(c, doc)?;
        self.run_customer(
            &format!("INSERT INTO {} VALUES (?1, ?2)", quote(&c.storage)),
            &[
                EngineValue::Blob(doc["id"].encode()?),
                EngineValue::Blob(value::encode_document(doc)?),
            ],
        )?;
        for index in &c.indexes {
            self.insert_index(index, doc)?;
        }
        self.graph_link_edge(c, doc)
    }
    pub fn patch(&self, record: &Record, patch: Document) -> Result<Option<Document>> {
        if patch.contains_key("id") {
            return Err(Error::Validation("id is immutable".into()));
        }
        self.atomic(|| {
            let c = self.catalog(&record.table)?;
            let Some(mut doc) = self.get_in(&c, record)? else {
                return Ok(None);
            };
            doc.extend(patch);
            self.replace_document(&c, &mut doc)?;
            Ok(Some(doc))
        })
    }
    fn replace_document(&self, c: &Collection, doc: &mut Document) -> Result<()> {
        self.graph_restore_endpoints(c, doc)?;
        self.compute_fields(c, doc)?;
        self.validate_candidate(c, doc)?;
        self.write_validated_document(c, doc)
    }
    fn write_validated_document(&self, c: &Collection, doc: &Document) -> Result<()> {
        self.graph_check_edge_update(c, doc)?;
        let readonly = c
            .field_policies
            .iter()
            .any(|policy| policy.options.readonly);
        let before = if readonly || c.indexes.iter().any(Index::keyed_maintenance) {
            match doc.get("id") {
                Some(Value::Record(id)) => self.get_in(c, id)?,
                _ => None,
            }
        } else {
            None
        };
        if readonly {
            if let Some(before) = &before {
                field_rules::readonly(c, before, doc)?;
            }
        }
        let id = EngineValue::Blob(doc["id"].encode()?);
        self.run_customer(
            &format!("UPDATE {} SET doc = ?1 WHERE id = ?2", quote(&c.storage)),
            &[EngineValue::Blob(value::encode_document(doc)?), id.clone()],
        )?;
        for index in &c.indexes {
            if let (true, Some(before)) = (index.keyed_maintenance(), &before) {
                let old = index.document_keys(before)?;
                if old == index.document_keys(doc)? {
                    continue;
                }
                self.delete_keyed_entry(index, &id, old)?;
            } else {
                self.delete_index_entry(index, &id)?;
            }
            self.insert_index(index, doc)?;
        }
        Ok(())
    }
    /// Delete one scalar or spatial entry through the index on its key
    /// columns. Entry tables have no index on the ID column, so deleting by ID
    /// alone reads every entry. An entry that does not match its document's
    /// keys falls back to that complete deletion.
    fn delete_keyed_entry(
        &self,
        index: &Index,
        id: &EngineValue,
        keys: Vec<EngineValue>,
    ) -> Result<()> {
        let columns = index.key_columns();
        let columns = columns.split(',').collect::<Vec<_>>();
        if columns.len() != keys.len() {
            return Err(Error::Storage(format!(
                "invalid key arity for index {}",
                index.name
            )));
        }
        let condition = columns
            .iter()
            .enumerate()
            .map(|(i, column)| format!("{column} IS ?{}", i + 1))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut values = keys;
        values.push(id.clone());
        let deleted = self.run_index_maintenance(
            &format!(
                "DELETE FROM {} WHERE {condition} AND id = ?{} RETURNING id",
                quote(&index.storage),
                values.len()
            ),
            &values,
        )?;
        if deleted.len() != 1 {
            self.delete_index_entry(index, id)?;
        }
        Ok(())
    }
    // Caller owns the statement savepoint. A failure restores conflicting
    // documents along with the target and all managed indexes.
    fn replace_conflicting_document(&self, c: &Collection, doc: &mut Document) -> Result<()> {
        self.graph_restore_endpoints(c, doc)?;
        self.compute_fields(c, doc)?;
        self.validate_candidate(c, doc)?;
        self.delete_unique_conflicts(c, doc)?;
        self.write_validated_document(c, doc)
    }
    fn delete_unique_conflicts(&self, c: &Collection, doc: &Document) -> Result<()> {
        for index in c.indexes.iter().filter(|index| index.unique) {
            let keys = index.scalar_keys(doc)?;
            for conflict in self.lookup_scalar_keys(c, index, &keys)? {
                if conflict.get("id") != doc.get("id") {
                    self.delete_document(c, &conflict)?;
                }
            }
        }
        Ok(())
    }
    pub fn delete(&self, record: &Record) -> Result<Option<Document>> {
        self.atomic(|| {
            let c = self.catalog(&record.table)?;
            let Some(doc) = self.get_in(&c, record)? else {
                return Ok(None);
            };
            self.delete_document(&c, &doc)?;
            Ok(Some(doc))
        })
    }
    // Caller owns the operation savepoint and a snapshot read within it.
    fn delete_document(&self, c: &Collection, doc: &Document) -> Result<()> {
        self.graph_delete(c, doc)?;
        let id = EngineValue::Blob(doc["id"].encode()?);
        for index in &c.indexes {
            if index.keyed_maintenance() {
                self.delete_keyed_entry(index, &id, index.document_keys(doc)?)?;
            } else {
                self.delete_index_entry(index, &id)?;
            }
        }
        self.run_customer(
            &format!("DELETE FROM {} WHERE id = ?1", quote(&c.storage)),
            &[id],
        )?;
        Ok(())
    }
    pub fn lookup_index(&self, table: &str, name: &str, value: &Value) -> Result<Vec<Document>> {
        self.atomic(|| {
            let c = self.catalog(table)?;
            let index = c
                .indexes
                .iter()
                .find(|i| i.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| Error::NotFound(name.into()))?;
            if index.kind != IndexKind::Scalar || index.scalar.is_some() {
                return Err(Error::Validation(
                    "scalar lookup requires a single-path scalar index".into(),
                ));
            }
            let rows = self.run_customer(
                &format!(
                    "SELECT c.doc FROM {} AS i JOIN {} AS c ON c.id = i.id WHERE i.key = ?1",
                    quote(&index.storage),
                    quote(&c.storage)
                ),
                &[index_scalar(value)?],
            )?;
            rows.iter().map(|r| decode_document(&r[0])).collect()
        })
    }
    pub fn execute(&self, sql: &str, params: &Parameters) -> Result<QueryResult> {
        parser_stack(|| self.execute_inner(sql, params))
    }
    fn execute_inner(&self, sql: &str, params: &Parameters) -> Result<QueryResult> {
        self.execute_inner_with_result_limits(sql, params, None)
    }
    fn execute_inner_with_result_limits(
        &self,
        sql: &str,
        params: &Parameters,
        limits: Option<ResultLimits>,
    ) -> Result<QueryResult> {
        self.reset_spatial_counters();
        self.with_statement_timeout(sql, |sql| {
            if udf::has_calls(sql)?
                || analyzer::has_calls(sql)?
                || spatial::has_snapshot_sources(sql)?
            {
                return self.atomic(|| self.execute_snapshot(sql, params, limits));
            }
            self.execute_snapshot(sql, params, limits)
        })
    }
    fn execute_snapshot(
        &self,
        sql: &str,
        params: &Parameters,
        limits: Option<ResultLimits>,
    ) -> Result<QueryResult> {
        use romeql_parser::Statement;
        let object = |expr| match self.evaluate(expr, params, None)? {
            Value::Object(doc) => Ok(doc),
            _ => Err(Error::Validation("expected a typed document object".into())),
        };
        match romeql_parser::parse(sql)? {
            Statement::MutateDocument {
                table,
                target,
                mode,
                value,
                predicate,
                returning,
            } => self.atomic(|| {
                let collection = self.catalog(&table)?;
                let sources = if let Some(target) = target {
                    self.get(&target)?.into_iter().collect::<Vec<_>>()
                } else {
                    let suffix = predicate.map_or(String::new(), |p| format!(" WHERE {p}"));
                    let query = format!("SELECT * FROM {}{suffix}", quote(&table));
                    self.write_candidate_select(&query, params, false)?
                        .ok_or_else(|| Error::NotFound(table.clone()))?
                        .rows
                        .into_iter()
                        .map(|row| match row.into_iter().next() {
                            Some(Value::Object(document)) => Ok(document),
                            _ => Err(Error::Storage("expected candidate document".into())),
                        })
                        .collect::<Result<Vec<_>>>()?
                };
                let capture = mutation_result::needs_before(returning.as_deref().unwrap_or(""))?;
                let mut candidates = Vec::new();
                let mut budget = self.write_buffer_budget()?;
                for before in sources {
                    let value = self.evaluate(value.clone(), params, Some(&before))?;
                    let document = mutation::apply(&mode, &before, value)?;
                    let snapshot =
                        mutation_result::Snapshot::after(document, capture.then_some(before));
                    snapshot.charge(&mut budget)?;
                    candidates.push(snapshot);
                }
                let mut output_budget = self.write_buffer_budget()?;
                for document in &mut candidates {
                    self.replace_document(&collection, &mut document.document)?;
                    document.charge(&mut output_budget)?;
                }
                self.object_returning(&table, returning, candidates, params, limits)
            }),
            Statement::CreateFunction {
                name,
                parameters,
                returns,
                source,
                replace,
            } => {
                self.create_function(&name, parameters, &returns, &source, replace)?;
                Ok(QueryResult::command(0))
            }
            Statement::DropFunction { name, if_exists } => {
                self.drop_function(&name, if_exists)?;
                Ok(QueryResult::command(0))
            }
            Statement::DefineRelation {
                name,
                target,
                source,
                path,
            } => {
                self.define_relation(&name, &target, &source, path)?;
                Ok(QueryResult::command(0))
            }
            Statement::DropRelation { name, if_exists } => {
                self.drop_relation(&name, if_exists)?;
                Ok(QueryResult::command(0))
            }
            Statement::Upsert {
                table,
                target,
                value,
                returning,
            } => self.atomic(|| {
                let romeql_parser::Expr::Object(mut fields) = value else {
                    return Err(Error::Validation("UPSERT requires object".into()));
                };
                let record = if let Some(target) = target {
                    if fields.contains_key("id") {
                        return Err(Error::Validation("target UPSERT body must omit id".into()));
                    }
                    target
                } else {
                    let expr = fields.remove("id").ok_or_else(|| {
                        Error::Validation("UPSERT requires an explicit id".into())
                    })?;
                    let Value::Record(id) = self.evaluate(expr, params, None)? else {
                        return Err(Error::Validation("UPSERT id must be typed record".into()));
                    };
                    id
                };
                let record = normalized_id(&record, &canonical(&table)?)?;
                self.collection_for_write(&table)?;
                let before = self.get(&record)?.unwrap_or_default();
                let Value::Object(mut doc) =
                    self.evaluate(romeql_parser::Expr::Object(fields), params, Some(&before))?
                else {
                    unreachable!("object expression");
                };
                doc.insert("id".into(), Value::Record(record));
                let doc = self.upsert(&table, doc)?;
                let capture = mutation_result::needs_before(returning.as_deref().unwrap_or(""))?;
                let doc = mutation_result::Snapshot::after(
                    doc,
                    (capture && !before.is_empty()).then_some(before),
                );
                self.object_returning(&table, returning, vec![doc], params, limits)
            }),
            Statement::PatchWhere {
                table,
                value,
                predicate,
                returning,
            } => self.atomic(|| {
                let suffix = predicate.map_or(String::new(), |p| format!(" WHERE {p}"));
                let query = format!("SELECT * FROM {}{suffix}", quote(&table));
                let rows = self
                    .write_candidate_select(&query, params, false)?
                    .ok_or_else(|| Error::NotFound(table.clone()))?
                    .rows;
                let capture = mutation_result::needs_before(returning.as_deref().unwrap_or(""))?;
                let mut candidates = Vec::new();
                let mut candidate_budget = self.write_buffer_budget()?;
                for row in rows {
                    let Some(Value::Object(before)) = row.into_iter().next() else {
                        return Err(Error::Storage("expected candidate document".into()));
                    };
                    let Value::Object(patch) =
                        self.evaluate(value.clone(), params, Some(&before))?
                    else {
                        return Err(Error::Validation("expected object patch".into()));
                    };
                    let Some(Value::Record(id)) = before.get("id") else {
                        return Err(Error::Storage("expected typed id".into()));
                    };
                    candidate_budget.document(&patch)?;
                    candidate_budget.value(&Value::Record(id.clone()))?;
                    let id = id.clone();
                    let before = capture.then_some(before);
                    if let Some(before) = &before {
                        candidate_budget.document_fields(before)?;
                    }
                    candidates.push((id, patch, before));
                }
                let mut docs = Vec::new();
                let mut snapshot_budget = self.write_buffer_budget()?;
                for (id, patch, before) in candidates {
                    retain_write_document(
                        &mut snapshot_budget,
                        &mut docs,
                        mutation_result::Snapshot::after(
                            self.patch(&id, patch)?
                                .ok_or_else(|| Error::Storage("candidate disappeared".into()))?,
                            before,
                        ),
                    )?;
                }
                self.object_returning(&table, returning, docs, params, limits)
            }),
            Statement::RemoveField { table, path } => {
                self.remove_field(&table, &path)?;
                Ok(QueryResult::command(0))
            }
            Statement::Info { scope, name } => self.info(&scope, name.as_deref()),

            Statement::DefineSchema { table, strict } => {
                self.define_schema(&table, strict)?;
                Ok(QueryResult::command(0))
            }
            Statement::DefineField {
                table,
                path,
                kind,
                target,
                required,
                nullable,
                check,
                default,
                readonly,
                flexible,
                computed,
                overwrite,
            } => {
                let (element_type, element_nullable) = if kind == "array" {
                    field_rules::element_type(target.as_deref())?
                } else {
                    (None, false)
                };
                let kind = match (kind.as_str(), target) {
                    ("string", None) => FieldType::String,
                    ("integer", None) => FieldType::Integer,
                    ("number", None) => FieldType::Number,
                    ("boolean", None) => FieldType::Boolean,
                    ("object", None) => FieldType::Object,
                    ("array", _) => FieldType::Array,
                    ("record", Some(target)) => FieldType::Record(canonical(&target)?),
                    ("vector", Some(target)) => FieldType::Vector(
                        target
                            .parse()
                            .map_err(|_| Error::Validation("invalid vector dimension".into()))?,
                    ),
                    _ => return Err(Error::Unsupported("field type is not implemented".into())),
                };
                let default = default
                    .map(|expression| {
                        field_rules::constant(&expression)?;
                        self.evaluate(expression, params, None)
                    })
                    .transpose()?;
                self.define_field_with_options(
                    &table,
                    Field {
                        path,
                        kind,
                        required,
                        nullable,
                        check,
                    },
                    FieldOptions {
                        default,
                        computed,
                        readonly,
                        flexible,
                        element_type,
                        element_nullable,
                    },
                    overwrite,
                )?;
                Ok(QueryResult::command(0))
            }
            Statement::CreateVectorIndex {
                if_not_exists,
                table,
                name,
                path,
                dimensions,
                metric,
                quantization,
            } => {
                self.create_vector_index_with_options(
                    &table,
                    &name,
                    path,
                    VectorIndexOptions {
                        dimensions,
                        metric,
                        quantization: quantization.unwrap_or_else(|| "f32".into()),
                    },
                    if_not_exists,
                )?;
                Ok(QueryResult::command(0))
            }
            Statement::CreateFullTextIndex {
                if_not_exists,
                table,
                name,
                paths,
                tokenizer,
                min_gram,
                max_gram,
            } => {
                self.create_fulltext_index_with_options(
                    &table,
                    &name,
                    paths,
                    FullTextOptions {
                        tokenizer: tokenizer.unwrap_or_else(|| "default".into()),
                        min_gram,
                        max_gram,
                    },
                    if_not_exists,
                )?;
                Ok(QueryResult::command(0))
            }
            Statement::CreateArrayIndex {
                if_not_exists,
                table,
                name,
                path,
            } => {
                self.create_array_index(&table, &name, path, if_not_exists)?;
                Ok(QueryResult::command(0))
            }
            Statement::CreateSpatialIndex {
                if_not_exists,
                table,
                name,
                path,
                regions,
            } => {
                if regions {
                    self.create_spatial_region_index(&table, &name, path, if_not_exists)?;
                } else {
                    self.create_spatial_index(&table, &name, path, if_not_exists)?;
                }
                Ok(QueryResult::command(0))
            }
            Statement::CreateIndex {
                if_not_exists,
                table,
                name,
                path,
                additional_paths,
                unique,
                sql,
            } => match self.catalog(&table) {
                Ok(_) => {
                    let paths = std::iter::once(path).chain(additional_paths).collect();
                    self.create_compound_index(&table, &name, paths, unique, if_not_exists)?;
                    Ok(QueryResult::command(0))
                }
                Err(Error::NotFound(_)) => self.sql(&sql, params),
                Err(error) => Err(error),
            },
            Statement::CreateCollection {
                name,
                if_not_exists,
            } => {
                self.create_collection(&name, if_not_exists)?;
                Ok(QueryResult::command(0))
            }
            Statement::CreateEdgeTable {
                name,
                if_not_exists,
                source,
                target,
                cascade,
            } => {
                self.create_edge_table(&name, &source, &target, cascade, if_not_exists)?;
                Ok(QueryResult::command(0))
            }
            Statement::Relate {
                source,
                edge,
                id,
                target,
                value,
                returning,
            } => self.atomic(|| {
                let doc = self.relate_document(source, &edge, id, target, value, params)?;
                let doc = mutation_result::Snapshot::after(self.insert(&edge, doc)?, None);
                self.object_returning(&edge, returning, vec![doc], params, limits)
            }),
            Statement::Insert {
                table,
                value,
                returning,
            } => self.atomic(|| {
                let doc =
                    mutation_result::Snapshot::after(self.insert(&table, object(value)?)?, None);
                self.object_returning(&table, returning, vec![doc], params, limits)
            }),
            Statement::SelectRecordProjection { target, fields } => self
                .profile_record_projection(&target, &fields, params, limits)
                .map(|profile| profile.result),
            Statement::SelectRecord(record) => Ok(QueryResult::documents(
                self.get(&record)?.into_iter().collect(),
                0,
            )),
            Statement::Patch {
                target,
                value,
                returning,
            } => self.atomic(|| {
                let Some(before) = self.get(&target)? else {
                    return self.object_returning(
                        &target.table,
                        returning,
                        Vec::new(),
                        params,
                        limits,
                    );
                };
                let Value::Object(patch) = self.evaluate(value, params, Some(&before))? else {
                    return Err(Error::Validation("expected object patch".into()));
                };
                let capture = mutation_result::needs_before(returning.as_deref().unwrap_or(""))?;
                let doc = self
                    .patch(&target, patch)?
                    .map(|doc| mutation_result::Snapshot::after(doc, capture.then_some(before)));
                self.object_returning(
                    &target.table,
                    returning,
                    doc.into_iter().collect(),
                    params,
                    limits,
                )
            }),
            Statement::Delete { target, returning } => self.atomic(|| {
                let doc = self
                    .delete(&target)?
                    .map(mutation_result::Snapshot::deleted);
                self.object_returning(
                    &target.table,
                    returning,
                    doc.into_iter().collect(),
                    params,
                    limits,
                )
            }),
            Statement::Sql(sql) => {
                deferred::check(&sql)?;
                if write::is_data_write(self, &sql)? {
                    self.interruptible_write(|| self.sql(&sql, params))
                } else {
                    self.sql(&sql, params)
                }
            }
        }
    }
    pub(crate) fn guard_native_sql(&self, sql: &str) -> Result<()> {
        if matches!(
            select::parsed(sql),
            Ok(turso_parser::ast::Cmd::Stmt(
                turso_parser::ast::Stmt::CreateIndex { using: Some(_), .. }
            ))
        ) {
            return Err(Error::Unsupported(
                "native index methods require a managed search index".into(),
            ));
        }
        let tokens = guard::native_tokens(sql)?;
        let collections = self.run("SELECT name FROM __romedb_catalog", &[])?;
        for token in &tokens {
            // Reject unresolved managed references; value literals in covered AST
            // contexts have been removed from this guard-only token stream.
            if matches!(
                token.kind,
                romeql_parser::Kind::Word
                    | romeql_parser::Kind::Identifier
                    | romeql_parser::Kind::String
            ) {
                let name = token.text.to_ascii_lowercase();
                if guard::reserved_name(&name)
                    || name == "writable_schema"
                    || collections
                        .iter()
                        .any(|r| matches!(&r[0], EngineValue::Text(t) if t.as_str() == name))
                {
                    return Err(Error::Unsupported(
                        "managed collection SQL requires logical lowering".into(),
                    ));
                }
            }
        }
        Ok(())
    }
    fn sql(&self, sql: &str, params: &Parameters) -> Result<QueryResult> {
        if let Some(result) = self.catalog_statement(sql)? {
            return Ok(result);
        }
        if let Some(result) = self.collection_write(sql, params)? {
            return Ok(result);
        }
        if let Some(result) = self.collection_select(sql, params)? {
            return Ok(result);
        }
        self.native_profiled(sql, params)
            .map(|profile| profile.result)
    }
    fn native_profiled(&self, sql: &str, params: &Parameters) -> Result<ProfiledQuery> {
        self.native_profiled_with_limits(sql, params, None)
    }
    fn native_profiled_with_limits(
        &self,
        sql: &str,
        params: &Parameters,
        limits: Option<ResultLimits>,
    ) -> Result<ProfiledQuery> {
        self.guard_native_sql(sql)?;
        let mut stmt = self.prepare(sql)?;
        if !romeql_parser::tokenize(&sql[stmt.tail_offset()..])?.is_empty() {
            return Err(Error::Unsupported("execute accepts one statement".into()));
        }
        for (name, value) in params {
            let index = bind_index(&stmt, name).ok_or_else(|| Error::Parameter(name.clone()))?;
            stmt.bind_at(index, scalar(value)?)?;
        }
        let columns: Vec<String> = (0..stmt.num_columns())
            .map(|i| stmt.get_column_name(i).into_owned())
            .collect();
        let mut budget = budget::ResultBudget::new(limits, &columns)?;
        let mut rows = Vec::new();
        let mut failure = None;
        let execution = self.meter_statement(&mut stmt, |stmt| {
            parser_stack(|| {
                stmt.run_with_row_callback(|row| {
                    let output: Vec<_> = row.get_values().cloned().map(from_engine).collect();
                    if let Err(error) = budget.row(&output) {
                        failure = Some(error);
                        return Err(turso_core::LimboError::Interrupt);
                    }
                    rows.push(output);
                    Ok(())
                })
                .map_err(Error::from)
            })
        });
        if let Err(error @ Error::Rollback { .. }) = execution {
            return Err(error);
        }
        if let Some(error) = failure {
            return Err(error);
        }
        execution?;
        Ok(ProfiledQuery {
            result: QueryResult {
                columns,
                rows,
                affected: stmt.n_change(),
            },
            metrics: QueryMetrics::from_statement(&stmt),
        })
    }
}
fn bind_index(statement: &turso_core::Statement, name: &str) -> Option<NonZeroUsize> {
    statement.parameter_index(name).or_else(|| {
        let index = NonZeroUsize::new(name.strip_prefix('?')?.parse().ok()?)?;
        statement.parameters().has_index(index).then_some(index)
    })
}
fn from_engine(value: EngineValue) -> Value {
    use turso_core::Numeric;
    match value {
        EngineValue::Null => Value::Null,
        EngineValue::Text(t) => Value::String(t.as_str().into()),
        EngineValue::Blob(b) => Value::Binary(b),
        EngineValue::Numeric(Numeric::Integer(i)) => Value::Integer(i),
        EngineValue::Numeric(Numeric::Float(n)) => Value::Number(n.into()),
    }
}
fn scalar(value: &Value) -> Result<EngineValue> {
    value.validate()?;
    Ok(match value {
        Value::Null => EngineValue::Null,
        Value::Boolean(v) => EngineValue::Numeric(turso_core::Numeric::Integer(i64::from(*v))),
        Value::Integer(v) => EngineValue::Numeric(turso_core::Numeric::Integer(*v)),
        Value::Number(v) => EngineValue::Numeric(turso_core::Numeric::Float(
            turso_core::NonNan::new(*v).expect("validated finite number"),
        )),
        Value::String(v) => text(v),
        Value::Binary(v) => EngineValue::Blob(v.clone()),
        Value::Record(r) => {
            let mut r = r.clone();
            r.table = canonical(&r.table)?;
            EngineValue::Blob(Value::Record(r).encode()?)
        }
        _ => {
            return Err(Error::Validation(
                "expected scalar or record index value".into(),
            ))
        }
    })
}
fn index_scalar(value: &Value) -> Result<EngineValue> {
    // Binary data and record IDs share the engine BLOB storage class, but must
    // never alias each other even when user bytes imitate our record encoding.
    if matches!(value, Value::Binary(_)) {
        return Ok(EngineValue::Blob(value.encode()?));
    }
    scalar(value)
}
fn decode_document(value: &EngineValue) -> Result<Document> {
    if let EngineValue::Blob(bytes) = value {
        if let Value::Object(doc) = Value::decode(bytes)? {
            return Ok(doc);
        }
    }
    Err(Error::Storage("invalid stored document".into()))
}
fn normalized_id(record: &Record, table: &str) -> Result<Record> {
    value::validate_record(record)?;
    if canonical(&record.table)? != table {
        return Err(Error::Validation(
            "id targets a different collection".into(),
        ));
    }
    Ok(Record {
        table: table.into(),
        key: record.key.clone(),
    })
}
fn normalize_document_id(c: &Collection, doc: &mut Document) -> Result<()> {
    let Some(Value::Record(id)) = doc.get("id") else {
        return Err(Error::Validation("id must be a typed record".into()));
    };
    let id = normalized_id(id, &c.name)?;
    doc.insert("id".into(), Value::Record(id));
    Ok(())
}
fn validate_path(path: &[String]) -> Result<()> {
    if path.is_empty() || path.iter().any(String::is_empty) {
        return Err(Error::Validation("empty field path".into()));
    }
    Ok(())
}
fn path_value<'a>(doc: &'a Document, path: &[String]) -> Result<Option<&'a Value>> {
    let mut fields = doc;
    for (i, part) in path.iter().enumerate() {
        let value = fields.get(part);
        if i == path.len() - 1 || value.is_none() {
            return Ok(value);
        }
        match value {
            Some(Value::Object(object)) => fields = object,
            _ => return Err(Error::Validation(format!("non-object parent at {part}"))),
        }
    }
    Err(Error::Validation("empty field path".into()))
}
fn validate_document(c: &Collection, doc: &Document) -> Result<()> {
    value::validate_document_value(doc)?;
    for f in &c.fields {
        // A required nested child applies only when its parent object exists.
        if f.path.len() > 1 && path_value(doc, &f.path[..f.path.len() - 1])?.is_none() {
            continue;
        }
        let value = path_value(doc, &f.path)?;
        match value {
            None if !f.required => continue,
            Some(Value::Null) if f.nullable => continue,
            Some(value) if field_rules::matches_kind(&f.kind, value)? => continue,
            _ => {
                return Err(Error::Validation(format!(
                    "field {} failed {:?} validation",
                    f.path.join("."),
                    f.kind
                )))
            }
        }
    }
    field_rules::validate_arrays(c, doc)?;
    schema_rules::validate(c, doc)?;
    for index in &c.indexes {
        index.document_keys(doc)?;
    }
    Ok(())
}

// The pinned run_collect_rows helper conflates Interrupt with Busy. Its callback
// counterpart preserves those errors, so all frontend reads use this adapter.
fn collect_rows(statement: &mut turso_core::Statement) -> Result<Vec<Vec<EngineValue>>> {
    let mut rows = Vec::new();
    // Execution can reprepare after a concurrent schema change.
    parser_stack(|| {
        statement.run_with_row_callback(|row| {
            rows.push(row.get_values().cloned().collect());
            Ok(())
        })
    })?;
    Ok(rows)
}

// The pinned parser's recursion guard can require more than a caller's native
// stack in debug builds. Keep parsing/preparation on this thread while giving
// that guard room to return an error. Nested calls reuse the auxiliary stack.
fn parser_stack<T>(work: impl FnOnce() -> T) -> T {
    stacker::maybe_grow(16 * 1024 * 1024, 32 * 1024 * 1024, work)
}

#[cfg(test)]
mod stack_tests {
    use super::*;
    #[test]
    fn deep_native_statements_reprepare_on_a_small_caller_stack() {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                c.run("CREATE TABLE native(v INTEGER)", &[]).unwrap();
                c.run("INSERT INTO native VALUES (1)", &[]).unwrap();
                for (i, expr) in [
                    format!("{}1", "NOT ".repeat(80)),
                    format!(
                        "{}1{}",
                        "CASE WHEN 1 THEN ".repeat(80),
                        " ELSE 0 END".repeat(80)
                    ),
                ]
                .into_iter()
                .enumerate()
                {
                    let mut statement = c.prepare(format!("SELECT {expr} FROM native")).unwrap();
                    c.run(&format!("CREATE TABLE change_{i}(v INTEGER)"), &[])
                        .unwrap();
                    assert_eq!(
                        collect_rows(&mut statement).unwrap(),
                        vec![vec![EngineValue::Numeric(turso_core::Numeric::Integer(1))]]
                    );
                    assert!(statement.metrics().reprepares > 0);
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

#[cfg(all(test, unix))]
mod recovery_io;
