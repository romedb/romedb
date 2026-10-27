//! A cancellation-only handle that does not keep a database connection alive.
use crate::{Connection, ExecutionReport, Parameters, QueryResult, Result};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
/// A sticky cancellation request scoped to executions that explicitly use it.
/// Clones share the request; cancellation does not retain a connection.
#[derive(Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    deadline: Option<std::time::Instant>,
}
impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }
    /// Create a token that also requests cancellation once a monotonic deadline
    /// is reached. Expiry is observed at the same cooperative boundaries as
    /// manual cancellation, not by a background timer or hard preemption.
    pub fn with_deadline(deadline: std::time::Instant) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: Some(deadline),
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
            || self
                .deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
    }
}

pub(crate) struct ActiveCancellation {
    tokens: Vec<CancellationToken>,
    delivered: Arc<AtomicBool>,
    suspended: Arc<AtomicUsize>,
}
impl ActiveCancellation {
    fn install(self: &Arc<Self>, connection: &turso_core::Connection) {
        let active = self.clone();
        connection.set_progress_handler(1, Some(Box::new(move || active.interrupt_requested())));
    }
    fn interrupt_requested(&self) -> bool {
        self.suspended.load(Ordering::SeqCst) == 0
            && self.tokens.iter().any(CancellationToken::is_cancelled)
            && !self.delivered.swap(true, Ordering::SeqCst)
    }
}
struct ProgressGuard<'a> {
    connection: &'a Connection,
    previous: Option<Arc<ActiveCancellation>>,
}
impl Drop for ProgressGuard<'_> {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            previous.install(&self.connection.engine);
        } else {
            self.connection.engine.set_progress_handler(0, None);
        }
        *self
            .connection
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = self.previous.take();
    }
}

#[derive(Clone)]
pub struct InterruptHandle {
    connection: Weak<turso_core::Connection>,
}
impl InterruptHandle {
    /// Request interruption of active engine statements. Returns false if the
    /// connection has been dropped. True means live, not confirmed cancellation.
    /// Idle requests do not poison subsequent statements.
    pub fn interrupt(&self) -> bool {
        let Some(connection) = self.connection.upgrade() else {
            return false;
        };
        connection.interrupt();
        true
    }
}
impl Connection {
    /// Execute with cooperative cancellation at engine progress boundaries.
    /// A pre-cancelled token rejects before parsing or writes. Cancellation is
    /// best effort; completion can win a race with cancel(). Serialize calls on
    /// this connection. Compilation and non-engine work have no latency bound.
    pub fn execute_cancellable(
        &self,
        sql: &str,
        params: &Parameters,
        token: &CancellationToken,
    ) -> Result<QueryResult> {
        self.with_cancellation(token, || self.execute(sql, params))
    }

    /// Apply the atomic write-result policy with cooperative cancellation.
    /// Completed-result accounting has no fixed cancellation latency. A cancelled
    /// write uses the existing statement/savepoint recovery contract.
    pub fn write_with_result_limits_cancellable(
        &self,
        sql: &str,
        params: &Parameters,
        limits: crate::ResultLimits,
        token: &CancellationToken,
    ) -> Result<QueryResult> {
        self.with_cancellation(token, || self.write_with_result_limits(sql, params, limits))
    }

    /// Profile a SELECT using the same cooperative cancellation contract.
    pub fn profile_select_cancellable(
        &self,
        sql: &str,
        params: &Parameters,
        token: &CancellationToken,
    ) -> Result<crate::ProfiledQuery> {
        self.with_cancellation(token, || self.profile_select(sql, params))
    }

    /// Profile a bounded SELECT with cooperative engine-boundary cancellation.
    pub fn profile_select_with_limits_cancellable(
        &self,
        sql: &str,
        params: &Parameters,
        limits: crate::ResultLimits,
        token: &CancellationToken,
    ) -> Result<crate::ProfiledQuery> {
        self.with_cancellation(token, || {
            self.profile_select_with_limits(sql, params, limits)
        })
    }

    /// Execute a bounded SELECT with cooperative engine-boundary cancellation.
    pub fn select_with_limits_cancellable(
        &self,
        sql: &str,
        params: &Parameters,
        limits: crate::ResultLimits,
        token: &CancellationToken,
    ) -> Result<QueryResult> {
        self.profile_select_with_limits_cancellable(sql, params, limits, token)
            .map(|profile| profile.result)
    }

    /// Audit collection content with cooperative engine-boundary cancellation.
    pub fn check_collection_integrity_cancellable(
        &self,
        table: &str,
        limits: crate::IntegrityLimits,
        token: &CancellationToken,
    ) -> Result<crate::IntegrityReport> {
        self.with_cancellation(token, || self.check_collection_integrity(table, limits))
    }

    /// Export a complete transfer payload with cooperative cancellation.
    /// Serialization is not interrupted; no partial payload is returned.
    pub fn export_documents_cancellable(
        &self,
        table: &str,
        format: crate::TransferFormat,
        token: &CancellationToken,
    ) -> Result<String> {
        self.with_cancellation(token, || self.export_documents(table, format))
    }

    /// Import atomically with cooperative cancellation. Parsing has no fixed
    /// cancellation latency; interrupted writes use the import's rollback path.
    pub fn import_documents_cancellable(
        &self,
        table: &str,
        input: &str,
        format: crate::TransferFormat,
        token: &CancellationToken,
    ) -> Result<usize> {
        self.with_cancellation(token, || self.import_documents(table, input, format))
    }

    /// Run pending migrations with cooperative cancellation. Their schema,
    /// data and history changes share the existing atomic migration scope.
    pub fn migrate_cancellable(
        &self,
        migrations: &[crate::Migration],
        token: &CancellationToken,
    ) -> Result<crate::MigrationReport> {
        self.with_cancellation(token, || self.migrate(migrations))
    }

    pub(crate) fn check_cancellation(&self) -> Result<()> {
        let active = self
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if active.is_some_and(|active| active.interrupt_requested()) {
            Err(crate::Error::Engine(turso_core::LimboError::Interrupt))
        } else {
            Ok(())
        }
    }

    pub(crate) fn cancellation_cleanup<T>(&self, operation: impl FnOnce() -> T) -> T {
        struct Resume(Option<Arc<ActiveCancellation>>);
        impl Drop for Resume {
            fn drop(&mut self) {
                if let Some(active) = &self.0 {
                    active.suspended.fetch_sub(1, Ordering::SeqCst);
                }
            }
        }
        let active = self
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(active) = &active {
            active.suspended.fetch_add(1, Ordering::SeqCst);
        }
        let _resume = Resume(active);
        operation()
    }

    pub(crate) fn with_cancellation<T>(
        &self,
        token: &CancellationToken,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let previous = self
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if token.is_cancelled() {
            if let Some(previous) = &previous {
                previous.delivered.store(true, Ordering::SeqCst);
            }
            return Err(crate::Error::Engine(turso_core::LimboError::Interrupt));
        }
        let mut tokens = previous
            .as_ref()
            .map_or_else(Vec::new, |active| active.tokens.clone());
        tokens.push(token.clone());
        let active = Arc::new(ActiveCancellation {
            tokens,
            suspended: previous.as_ref().map_or_else(
                || Arc::new(AtomicUsize::new(0)),
                |active| active.suspended.clone(),
            ),
            delivered: previous.as_ref().map_or_else(
                || Arc::new(AtomicBool::new(false)),
                |active| active.delivered.clone(),
            ),
        });
        active.install(&self.engine);
        *self.cancellation.lock().unwrap_or_else(|e| e.into_inner()) = Some(active);
        let _guard = ProgressGuard {
            connection: self,
            previous,
        };
        operation()
    }

    /// Cancellable execution with transaction observations on both outcomes.
    pub fn execute_report_cancellable(
        &self,
        sql: &str,
        params: &Parameters,
        token: &CancellationToken,
    ) -> ExecutionReport {
        let transaction_before = self.transaction_state();
        let result = self.execute_cancellable(sql, params, token);
        ExecutionReport {
            result,
            transaction_before,
            transaction_after: self.transaction_state(),
        }
    }

    pub fn interrupt_handle(&self) -> InterruptHandle {
        InterruptHandle {
            connection: Arc::downgrade(&self.engine),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Database, Parameters, Value};
    use std::sync::atomic::{AtomicBool, Ordering};
    #[test]
    fn cancelled_plain_native_writes_undo_rows_and_preserve_caller_work() {
        for (outer, finish) in [
            (None, None),
            (Some("BEGIN"), Some("ROLLBACK")),
            (Some("BEGIN"), Some("COMMIT")),
            (Some("SAVEPOINT caller"), Some("ROLLBACK")),
            (Some("SAVEPOINT caller"), Some("RELEASE caller")),
        ] {
            for via_handle in [false, true] {
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                let q = |sql: &str| c.execute(sql, &Parameters::new()).unwrap();
                q("CREATE TABLE source(n INTEGER)");
                q("INSERT INTO source VALUES(1),(2),(3),(4)");
                q("CREATE TABLE sink(n INTEGER)");
                q("CREATE TABLE prior(n INTEGER)");
                if let Some(sql) = outer {
                    q(sql);
                    q("INSERT INTO prior VALUES(9)");
                }
                // last_insert_rowid changes during each Insert opcode. Observing
                // its atomic value avoids adding an SQL function or trigger that
                // changes the native compiler's statement-journal decisions.
                assert_ne!(c.engine.last_insert_rowid(), 2);
                let engine = Arc::downgrade(&c.engine);
                let fired = Arc::new(AtomicBool::new(false));
                let observed = fired.clone();
                c.engine.set_progress_handler(
                    1,
                    Some(Box::new(move || {
                        let engine = engine.upgrade().unwrap();
                        if engine.last_insert_rowid() == 2 && !observed.swap(true, Ordering::SeqCst)
                        {
                            if via_handle {
                                engine.interrupt();
                                false
                            } else {
                                true
                            }
                        } else {
                            false
                        }
                    })),
                );
                let error = c
                    .execute("INSERT INTO sink SELECT n FROM source", &Parameters::new())
                    .unwrap_err();
                c.engine.set_progress_handler(0, None);
                assert!(fired.load(Ordering::SeqCst));
                assert_eq!(
                    error.code(),
                    "FDB_CANCELLED",
                    "outer={outer:?}, handle={via_handle}: {error:?}"
                );
                assert!(
                    q("SELECT * FROM sink").rows.is_empty(),
                    "outer={outer:?}, handle={via_handle}"
                );
                assert_eq!(
                    q("SELECT * FROM prior").rows.len(),
                    usize::from(outer.is_some())
                );
                assert_eq!(
                    c.transaction_state(),
                    if outer.is_some() {
                        crate::TransactionState::Active
                    } else {
                        crate::TransactionState::Autocommit
                    }
                );
                q("INSERT INTO sink SELECT n FROM source");
                assert_eq!(
                    q("SELECT n FROM sink ORDER BY n").rows,
                    (1..=4).map(|n| vec![Value::Integer(n)]).collect::<Vec<_>>()
                );
                if let Some(finish) = finish {
                    // Visibility before COMMIT is not enough: cancellation must
                    // leave earlier caller work committable after statement undo.
                    q(finish);
                    assert_eq!(c.transaction_state(), crate::TransactionState::Autocommit);
                    if finish == "ROLLBACK" {
                        assert!(q("SELECT * FROM sink").rows.is_empty());
                        assert!(q("SELECT * FROM prior").rows.is_empty());
                    } else {
                        assert_eq!(q("SELECT * FROM sink").rows.len(), 4);
                        assert_eq!(q("SELECT * FROM prior").rows, vec![vec![Value::Integer(9)]]);
                    }
                }
            }
        }
    }

    #[test]
    fn cancellation_tokens_reject_before_writes_and_do_not_cancel_later_work() {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(&c, "BEGIN");
        q(&c, "INSERT INTO docs {id:docs:prior,n:9}");
        let prior = q(&c, "SELECT id,n FROM docs").rows;
        let token = CancellationToken::new();
        let other = token.clone();
        std::thread::spawn(move || other.cancel()).join().unwrap();
        assert!(token.is_cancelled());
        assert_eq!(
            c.write_with_result_limits_cancellable(
                "INSERT INTO docs {n:1} RETURNING n",
                &Parameters::new(),
                crate::ResultLimits {
                    max_rows: 1,
                    max_payload_bytes: 9
                },
                &token
            )
            .unwrap_err()
            .code(),
            "FDB_CANCELLED"
        );
        assert_eq!(q(&c, "SELECT id,n FROM docs").rows, prior);
        assert_eq!(c.transaction_state(), crate::TransactionState::Active);
        let report =
            c.execute_report_cancellable("INSERT INTO docs {n:1}", &Parameters::new(), &token);
        assert_eq!(report.result.unwrap_err().code(), "FDB_CANCELLED");
        assert_eq!(report.transaction_before, crate::TransactionState::Active);
        assert_eq!(report.transaction_after, crate::TransactionState::Active);
        assert_eq!(q(&c, "SELECT id,n FROM docs").rows, prior);
        let fresh = CancellationToken::new();
        c.execute_cancellable("INSERT INTO docs {n:2}", &Parameters::new(), &fresh)
            .unwrap();
        fresh.cancel();
        q(&c, "INSERT INTO docs {n:3}");
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .documents,
            3
        );
        q(&c, "ROLLBACK");
    }

    #[test]
    fn cancellation_tokens_interrupt_sources_and_preserve_retry_and_transaction_state() {
        use std::sync::{atomic::AtomicUsize, Mutex};
        use turso_ext::{scalar, ResultCode, Value as ExtValue};
        static TOKEN: Mutex<Option<CancellationToken>> = Mutex::new(None);
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        #[scalar(name = "cancel_token_tick")]
        fn cancel_token_tick(args: &[ExtValue]) -> ExtValue {
            if CALLS.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                TOKEN.lock().unwrap().as_ref().unwrap().cancel();
            }
            ExtValue::from_integer(args[0].to_integer().unwrap())
        }
        for insert in [false, true] {
            for (outer, mode) in [
                (false, 0),
                (true, 0),
                (false, 1),
                (true, 1),
                (false, 2),
                (true, 2),
                (false, 3),
                (true, 3),
            ] {
                if mode == 3 && !insert {
                    continue;
                }
                let batch = mode == 1;
                let bounded = mode >= 2;
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                unsafe {
                    let api = c.engine._build_turso_ext();
                    let code = (api.register_scalar_function)(
                        api.ctx,
                        c"cancel_token_tick".as_ptr(),
                        1,
                        false,
                        0,
                        cancel_token_tick,
                        None,
                        None,
                    );
                    c.engine._free_extension_ctx(api);
                    assert_eq!(code, ResultCode::OK);
                }
                q(&c, "CREATE TABLE docs");
                q(&c, "INSERT INTO docs(n) VALUES (1),(2),(3)");
                q(&c, "CREATE TABLE target");
                q(&c, "CREATE UNIQUE INDEX target_n ON target(n)");
                if outer {
                    q(&c, "BEGIN");
                    q(&c, "INSERT INTO target {id:target:prior,n:9}");
                }
                let prior = q(&c, "SELECT id,n FROM target").rows;
                let token = CancellationToken::new();
                *TOKEN.lock().unwrap() = Some(token.clone());
                CALLS.store(0, Ordering::SeqCst);
                let sql = if mode == 3 {
                    "INSERT INTO target(n) SELECT n FROM docs RETURNING cancel_token_tick(n) AS n"
                        .to_owned()
                } else {
                    format!(
                        "{}SELECT cancel_token_tick(n) AS n FROM docs",
                        if insert { "INSERT INTO target(n) " } else { "" }
                    )
                };
                let limits = crate::ResultLimits {
                    max_rows: 3,
                    max_payload_bytes: 1000,
                };
                let report = if bounded {
                    let transaction_before = c.transaction_state();
                    let result = if insert {
                        c.write_with_result_limits_cancellable(
                            &sql,
                            &Parameters::new(),
                            limits,
                            &token,
                        )
                    } else {
                        c.select_with_limits_cancellable(&sql, &Parameters::new(), limits, &token)
                    };
                    ExecutionReport {
                        result,
                        transaction_before,
                        transaction_after: c.transaction_state(),
                    }
                } else if batch {
                    let script = format!("SELECT 42; {sql}; DELETE FROM target;");
                    let mut entries = c.execute_batch_cancellable(&script, &token).unwrap();
                    assert_eq!(entries.len(), 2);
                    assert!(entries[0].execution.result.is_ok());
                    assert_eq!(entries[1].offset, "SELECT 42; ".len());
                    entries.pop().unwrap().execution
                } else {
                    c.execute_report_cancellable(&sql, &Parameters::new(), &token)
                };
                assert_eq!(report.result.unwrap_err().code(), "FDB_CANCELLED");
                assert_eq!(
                    report.transaction_after,
                    if outer {
                        crate::TransactionState::Active
                    } else {
                        crate::TransactionState::Autocommit
                    }
                );
                assert_eq!(CALLS.load(Ordering::SeqCst), 2);
                assert_eq!(q(&c, "SELECT id,n FROM target").rows, prior);
                assert_eq!(
                    c.check_collection_integrity("target", Default::default())
                        .unwrap()
                        .documents,
                    u64::from(outer)
                );
                let retry = if bounded {
                    let fresh = CancellationToken::new();
                    if insert {
                        c.write_with_result_limits_cancellable(
                            &sql,
                            &Parameters::new(),
                            limits,
                            &fresh,
                        )
                        .unwrap()
                    } else {
                        c.select_with_limits_cancellable(&sql, &Parameters::new(), limits, &fresh)
                            .unwrap()
                    }
                } else {
                    q(&c, &sql)
                };
                if insert {
                    assert_eq!(retry.affected, 3);
                } else {
                    assert_eq!(
                        retry.rows,
                        vec![
                            vec![Value::Integer(1)],
                            vec![Value::Integer(2)],
                            vec![Value::Integer(3)]
                        ]
                    );
                }
                assert_eq!(
                    c.check_collection_integrity("target", Default::default())
                        .unwrap()
                        .documents,
                    u64::from(outer) + if insert { 3 } else { 0 }
                );
                if outer {
                    q(&c, "ROLLBACK");
                    assert_eq!(
                        c.check_collection_integrity("target", Default::default())
                            .unwrap()
                            .documents,
                        0
                    );
                }
                *TOKEN.lock().unwrap() = None;
            }
        }
    }

    #[test]
    fn transfer_cancellation_preserves_atomic_import_and_prior_work() {
        for format in [crate::TransferFormat::Json, crate::TransferFormat::Ndjson] {
            for outer in [false, true] {
                let source = Database::open(":memory:").unwrap();
                let source = source.connect().unwrap();
                q(&source, "CREATE TABLE docs");
                q(&source, "INSERT INTO docs(n) VALUES (1),(2),(3)");
                let data = source.export_documents("docs", format).unwrap();
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                q(&c, "CREATE TABLE docs");
                q(&c, "CREATE UNIQUE INDEX docs_n ON docs(n)");
                if outer {
                    q(&c, "BEGIN");
                }
                q(&c, "INSERT INTO docs {id:docs:prior,n:9}");
                let state = c.transaction_state();
                let prior = q(&c, "SELECT id,n FROM docs").rows;
                let fired = arm_after_write(&c);
                let result = c.import_documents("docs", &data, format);
                c.engine.set_progress_handler(0, None);
                assert!(fired.load(Ordering::SeqCst));
                assert_eq!(result.unwrap_err().code(), "FDB_CANCELLED");
                assert_eq!(c.transaction_state(), state);
                assert_eq!(q(&c, "SELECT id,n FROM docs").rows, prior);
                assert_eq!(
                    c.check_collection_integrity("docs", Default::default())
                        .unwrap()
                        .documents,
                    1
                );
                let cancelled = CancellationToken::new();
                cancelled.cancel();
                assert_eq!(
                    c.import_documents_cancellable("docs", "invalid", format, &cancelled)
                        .unwrap_err()
                        .code(),
                    "FDB_CANCELLED"
                );
                assert_eq!(
                    c.export_documents_cancellable("docs", format, &cancelled)
                        .unwrap_err()
                        .code(),
                    "FDB_CANCELLED"
                );
                c.engine.set_progress_handler(1, Some(Box::new(|| true)));
                let export = c.export_documents("docs", format);
                c.engine.set_progress_handler(0, None);
                // This deliberately persistent handler also interrupts cleanup.
                // Public CancellationToken delivery is one-shot; an active outer
                // transaction must report that cleanup could not be verified.
                assert_eq!(
                    export.unwrap_err().code(),
                    if outer {
                        "FDB_ROLLBACK"
                    } else {
                        "FDB_CANCELLED"
                    }
                );
                assert_eq!(c.transaction_state(), state);
                let fresh = CancellationToken::new();
                assert_eq!(
                    c.import_documents_cancellable("docs", &data, format, &fresh)
                        .unwrap(),
                    3
                );
                let complete = c
                    .export_documents_cancellable("docs", format, &fresh)
                    .unwrap();
                assert_eq!(complete, c.export_documents("docs", format).unwrap());
                assert_eq!(
                    c.check_collection_integrity("docs", Default::default())
                        .unwrap()
                        .documents,
                    4
                );
                fresh.cancel();
                assert_eq!(
                    q(&c, "SELECT count(*) FROM docs").rows,
                    vec![vec![Value::Integer(4)]]
                );
                if outer {
                    q(&c, "ROLLBACK");
                    assert_eq!(
                        c.check_collection_integrity("docs", Default::default())
                            .unwrap()
                            .documents,
                        0
                    );
                }
            }
        }
    }

    #[test]
    fn token_cancellation_at_nested_release_preserves_atomic_outcomes() {
        for stop in 1..=32 {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            q(&c, "CREATE TABLE native(n INTEGER)");
            q(&c, "BEGIN");
            q(&c, "INSERT INTO native VALUES(0)");
            let token = CancellationToken::new();
            let release_started = Arc::new(AtomicBool::new(false));
            let result = c.with_cancellation(&token, || {
                let active = c.cancellation.lock().unwrap().clone().unwrap();
                let cancel = token.clone();
                let started = release_started.clone();
                let ticks = AtomicUsize::new(0);
                c.engine.set_progress_handler(
                    1,
                    Some(Box::new(move || {
                        if started.load(Ordering::SeqCst)
                            && ticks.fetch_add(1, Ordering::SeqCst) + 1 == stop
                        {
                            cancel.cancel();
                        }
                        active.interrupt_requested()
                    })),
                );
                c.atomic(|| {
                    c.run("INSERT INTO native VALUES(1)", &[])?;
                    c.atomic(|| {
                        c.run("INSERT INTO native VALUES(2)", &[])?;
                        release_started.store(true, Ordering::SeqCst);
                        Ok(())
                    })?;
                    c.run("INSERT INTO native VALUES(3)", &[])?;
                    Ok(())
                })
            });
            let rows = q(&c, "SELECT n FROM native ORDER BY n").rows;
            match result {
                Ok(()) => assert_eq!(
                    rows,
                    (0..=3).map(|n| vec![Value::Integer(n)]).collect::<Vec<_>>(),
                    "stop={stop}"
                ),
                Err(error) => {
                    assert_eq!(error.code(), "FDB_CANCELLED", "stop={stop}: {error}");
                    assert_eq!(rows, vec![vec![Value::Integer(0)]], "stop={stop}");
                }
            }
            assert_eq!(c.transaction_state(), crate::TransactionState::Active);
            q(&c, "COMMIT");
        }
    }

    #[test]
    fn token_cancelled_before_release_restores_prior_work() {
        for outer in [false, true] {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            q(&c, "CREATE TABLE native(n INTEGER)");
            if outer {
                q(&c, "BEGIN");
            }
            q(&c, "INSERT INTO native VALUES(0)");
            let state = c.transaction_state();
            let token = CancellationToken::new();
            let error = c
                .with_cancellation(&token, || {
                    c.atomic(|| {
                        c.run("INSERT INTO native VALUES(1)", &[])?;
                        token.cancel();
                        Ok(())
                    })
                })
                .unwrap_err();
            assert_eq!(error.code(), "FDB_CANCELLED");
            assert_eq!(c.transaction_state(), state);
            assert_eq!(
                q(&c, "SELECT n FROM native").rows,
                vec![vec![Value::Integer(0)]]
            );
            if outer {
                q(&c, "COMMIT");
            }
        }
    }

    #[test]
    fn nested_atomic_cancellation_never_reports_partial_success_as_cancelled() {
        use std::sync::atomic::AtomicUsize;
        for stop in 1..=48 {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            q(&c, "CREATE TABLE native(n INTEGER)");
            q(&c, "BEGIN");
            q(&c, "INSERT INTO native VALUES(0)");
            let baseline = c.engine.total_changes();
            let engine = Arc::downgrade(&c.engine);
            let ticks = Arc::new(AtomicUsize::new(0));
            let seen = ticks.clone();
            c.engine.set_progress_handler(
                1,
                Some(Box::new(move || {
                    engine
                        .upgrade()
                        .is_some_and(|engine| engine.total_changes() > baseline)
                        && seen.fetch_add(1, Ordering::SeqCst) + 1 == stop
                })),
            );
            let result = c.atomic(|| {
                c.run("INSERT INTO native VALUES(1)", &[])?;
                c.atomic(|| c.run("INSERT INTO native VALUES(2)", &[]))?;
                Ok(())
            });
            c.engine.set_progress_handler(0, None);
            if stop == 4 {
                assert!(matches!(&result, Err(error) if error.code() == "FDB_CANCELLED"));
            }
            let rows = q(&c, "SELECT n FROM native ORDER BY n").rows;
            let before = vec![vec![Value::Integer(0)]];
            let complete = vec![
                vec![Value::Integer(0)],
                vec![Value::Integer(1)],
                vec![Value::Integer(2)],
            ];
            match result {
                Ok(()) => assert_eq!(rows, complete, "stop={stop}"),
                Err(error) if error.code() == "FDB_CANCELLED" => {
                    assert_eq!(rows, before, "stop={stop}")
                }
                Err(crate::Error::Rollback { .. }) => assert!(
                    rows == before || rows == complete,
                    "partial rows at stop={stop}: {rows:?}"
                ),
                Err(error) => panic!("stop={stop}: {error}"),
            }
            assert_eq!(c.transaction_state(), crate::TransactionState::Active);
            q(&c, "ROLLBACK");
        }
    }

    #[test]
    fn cancelled_atomic_open_preserves_transaction_state() {
        use std::sync::atomic::AtomicUsize;
        for outer in [false, true] {
            let mut rejected_opens = 0;
            for stop in 1..=16 {
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                q(&c, "CREATE TABLE native(n INTEGER)");
                if outer {
                    q(&c, "BEGIN");
                    q(&c, "INSERT INTO native VALUES(9)");
                }
                let state = c.transaction_state();
                let id = c.next_atomic_id.load(Ordering::Relaxed);
                let ticks = AtomicUsize::new(0);
                c.engine.set_progress_handler(
                    1,
                    Some(Box::new(move || {
                        ticks.fetch_add(1, Ordering::SeqCst) + 1 == stop
                    })),
                );
                let called = std::cell::Cell::new(false);
                let result = c.atomic(|| {
                    called.set(true);
                    Ok(())
                });
                c.engine.set_progress_handler(0, None);
                if called.get() {
                    break;
                }
                rejected_opens += 1;
                assert_eq!(result.unwrap_err().code(), "FDB_CANCELLED");
                assert_eq!(c.transaction_state(), state, "outer={outer}, stop={stop}");
                assert!(
                    c.run(&format!("RELEASE __romedb_statement_{id}"), &[])
                        .is_err(),
                    "orphan savepoint: outer={outer}, stop={stop}"
                );
                assert_eq!(
                    q(&c, "SELECT count(*) FROM native").rows,
                    vec![vec![Value::Integer(i64::from(outer))]]
                );
                c.atomic(|| c.run("INSERT INTO native VALUES(1)", &[]))
                    .unwrap();
                assert_eq!(
                    q(&c, "SELECT count(*) FROM native").rows,
                    vec![vec![Value::Integer(i64::from(outer) + 1)]]
                );
                if outer {
                    q(&c, "ROLLBACK");
                    assert!(q(&c, "SELECT n FROM native").rows.is_empty());
                }
            }
            // Dispatch fusion changes the number of interruptible VM boundaries.
            assert!(rejected_opens > 0);
        }
    }

    #[test]
    fn atomic_release_cancellation_preserves_complete_or_restored_write_sets() {
        use std::sync::atomic::AtomicUsize;
        let mut interrupted = 0;
        for outer in [false, true] {
            for stop in 1..=16 {
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                q(&c, "CREATE TABLE native(n INTEGER)");
                if outer {
                    q(&c, "BEGIN");
                }
                q(&c, "INSERT INTO native VALUES(0)");
                let state = c.transaction_state();
                let baseline = c.engine.total_changes();
                let engine = Arc::downgrade(&c.engine);
                let ticks = Arc::new(AtomicUsize::new(0));
                let seen = ticks.clone();
                c.engine.set_progress_handler(
                    1,
                    Some(Box::new(move || {
                        engine
                            .upgrade()
                            .is_some_and(|c| c.total_changes() >= baseline + 2)
                            && seen.fetch_add(1, Ordering::SeqCst) + 1 == stop
                    })),
                );
                let result = c.atomic(|| c.run("INSERT INTO native VALUES(1),(2)", &[]));
                c.engine.set_progress_handler(0, None);
                let delivered = ticks.load(Ordering::SeqCst) >= stop;
                let rows = q(&c, "SELECT n FROM native ORDER BY n").rows;
                let before = vec![vec![Value::Integer(0)]];
                let complete = vec![
                    vec![Value::Integer(0)],
                    vec![Value::Integer(1)],
                    vec![Value::Integer(2)],
                ];
                assert!(
                    rows == before || rows == complete,
                    "outer={outer}, stop={stop}: {rows:?}"
                );
                assert_eq!(c.transaction_state(), state, "outer={outer}, stop={stop}");
                match result {
                    Err(error) => {
                        assert!(delivered);
                        interrupted += 1;
                        match error.code() {
                            "FDB_CANCELLED" => assert_eq!(rows, before),
                            "FDB_ROLLBACK" => assert_eq!(rows, complete),
                            code => panic!("unexpected {code}: {error}"),
                        }
                    }
                    Ok(_) => {
                        assert!(!delivered, "outer={outer}, stop={stop}");
                        assert_eq!(rows, complete);
                    }
                }
                if outer {
                    q(&c, "ROLLBACK");
                    assert!(q(&c, "SELECT n FROM native").rows.is_empty());
                }
            }
        }
        assert!(interrupted > 0);
    }

    fn arm_after_write(connection: &Connection) -> Arc<AtomicBool> {
        let baseline = connection.engine.total_changes();
        let engine = Arc::downgrade(&connection.engine);
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        connection.engine.set_progress_handler(
            1,
            Some(Box::new(move || {
                engine
                    .upgrade()
                    .is_some_and(|c| c.total_changes() > baseline)
                    && !flag.swap(true, Ordering::SeqCst)
            })),
        );
        fired
    }
    fn q(connection: &Connection, sql: &str) -> crate::QueryResult {
        connection.execute(sql, &Parameters::new()).expect(sql)
    }
    #[test]
    fn interrupted_collection_mutations_restore_documents_and_indexes() {
        for (statement, outer) in [
            "UPDATE docs SET value=value+100",
            "UPDATE OR REPLACE docs SET value=value+1",
            "UPDATE OR REPLACE docs SET value=docs.value+1 FROM (SELECT 1 AS k) source",
            "UPDATE OR IGNORE docs SET value=value+100",
            "UPDATE OR FAIL docs SET value=value+100",
            "UPDATE docs AS d SET value=value+100 WHERE 1 IN (SELECT 1 WHERE d.value>0)",
            "UPDATE docs SET value=value+(SELECT 100)",
            "UPDATE docs AS d SET value=value+(SELECT 100 WHERE d.value>0)",
            "DELETE FROM docs AS d WHERE EXISTS(SELECT 1 WHERE d.value>0)",
            "WITH chosen AS (SELECT 100 AS delta) UPDATE docs SET value=value+(SELECT delta FROM chosen)",
            "WITH chosen AS (SELECT value FROM docs) UPDATE docs SET value=value+100 WHERE value IN (SELECT value FROM chosen)",
            "DELETE FROM docs WHERE value>0",
            "WITH chosen AS (SELECT value FROM docs) DELETE FROM docs WHERE value IN (SELECT value FROM chosen)",
            "INSERT INTO docs (value) SELECT value+100 FROM docs",
            "INSERT OR ABORT INTO docs(value) VALUES(101),(102)",
            "INSERT OR ABORT INTO docs(value) SELECT value+100 FROM docs",
            "INSERT OR IGNORE INTO docs(value) VALUES(101),(102)",
            "INSERT OR IGNORE INTO docs(value) SELECT value+100 FROM docs",
            "INSERT OR REPLACE INTO docs(value) VALUES(1),(2)",
            "INSERT OR REPLACE INTO docs(value) SELECT value FROM docs",
            "INSERT OR REPLACE INTO docs(id,value) VALUES(docs:1,101)",
            "INSERT OR REPLACE INTO docs(id,value) SELECT id,value+100 FROM docs",
            "INSERT OR FAIL INTO docs(value) VALUES(101),(102)",
            "INSERT OR FAIL INTO docs(value) SELECT value+100 FROM docs",
            "INSERT OR ROLLBACK INTO docs(value) VALUES(101),(102)",
            "INSERT OR ROLLBACK INTO docs(value) SELECT value+100 FROM docs",
            "WITH a AS (SELECT value+100 AS value FROM docs) INSERT INTO docs (value) SELECT value FROM a",
            "INSERT INTO docs (value) SELECT a.value FROM (SELECT value+100 AS value FROM docs) a",
            "INSERT INTO docs (value) SELECT value+100 FROM docs UNION ALL SELECT value+200 FROM docs",
            "WITH a AS MATERIALIZED (SELECT value FROM docs) INSERT INTO docs (value) SELECT value+100 FROM a UNION ALL SELECT value+200 FROM a",
            "INSERT INTO docs (value) SELECT value+100 FROM docs UNION SELECT value+100 FROM docs",
            "INSERT INTO docs (value) SELECT value+100 FROM docs INTERSECT SELECT value+100 FROM docs",
            "INSERT INTO docs (value) SELECT value+100 FROM docs EXCEPT SELECT value+200 FROM docs",
            "INSERT INTO docs (value) VALUES (101),(102) UNION VALUES (102),(103)",
        ]
        .into_iter()
        .flat_map(|statement| [false, true].map(|outer| (statement, outer)))
        {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            q(&c, "CREATE TABLE docs");
            q(&c, "CREATE UNIQUE INDEX values_idx ON docs(value)");
            for i in 1..=3 {
                q(&c, &format!("INSERT INTO docs {{id:docs:{i},value:{i}}}"));
            }
            let before = q(&c, "SELECT * FROM docs ORDER BY id").rows;
            q(&c, "CREATE TABLE prior(value INTEGER)");
            if outer {
                q(&c, "BEGIN");
                q(&c, "INSERT INTO prior VALUES (1)");
            }
            let fired = arm_after_write(&c);
            let report = c.execute_report(statement, &Parameters::new());
            c.engine.set_progress_handler(0, None);
            assert_eq!(
                report.transaction_after,
                if outer {
                    crate::TransactionState::Active
                } else {
                    crate::TransactionState::Autocommit
                },
                "{statement}, outer={outer}"
            );
            assert_eq!(
                q(&c, "SELECT * FROM prior").rows.len(),
                usize::from(outer),
                "prior work: {statement}"
            );
            assert!(
                fired.load(Ordering::SeqCst),
                "no partial write reached: {statement}"
            );
            assert_eq!(
                report.result.unwrap_err().code(),
                "FDB_CANCELLED",
                "{statement}"
            );
            assert_eq!(
                q(&c, "SELECT * FROM docs ORDER BY id").rows,
                before,
                "{statement}"
            );
            for i in 1..=3 {
                assert_eq!(
                    c.lookup_index("docs", "values_idx", &Value::Integer(i))
                        .unwrap()
                        .len(),
                    1
                );
                assert!(c
                    .lookup_index("docs", "values_idx", &Value::Integer(i + 100))
                    .unwrap()
                    .is_empty());
            }
            assert_eq!(c.check_collection_integrity("docs", Default::default()).unwrap().documents, 3);
            q(&c, "UPDATE docs SET value=value+10");
            assert_eq!(
                c.lookup_index("docs", "values_idx", &Value::Integer(11))
                    .unwrap()
                    .len(),
                1
            );
            if outer {
                q(&c, "ROLLBACK");
                assert!(q(&c, "SELECT * FROM prior").rows.is_empty());
                assert_eq!(q(&c, "SELECT * FROM docs ORDER BY id").rows, before);
            }
        }
    }
    #[test]
    fn interrupted_spatial_index_build_can_retry_without_orphans() {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for i in 1..=3 {
            q(
                &c,
                &format!("INSERT INTO places {{location:geo::point({i},0)}}"),
            );
        }
        let fired = arm_after_write(&c);
        let result = c.create_spatial_index("places", "locations", vec!["location".into()], false);
        c.engine.set_progress_handler(0, None);
        assert!(fired.load(Ordering::SeqCst));
        assert_eq!(result.unwrap_err().code(), "FDB_CANCELLED");
        assert!(c.catalog("places").unwrap().indexes.is_empty());
        assert!(c
            .run(
                "SELECT name FROM sqlite_schema WHERE name='locations' OR name LIKE '__romedb_i_%'",
                &[]
            )
            .unwrap()
            .is_empty());
        c.create_spatial_index("places", "locations", vec!["location".into()], false)
            .unwrap();
        assert_eq!(
            q(
                &c,
                "SELECT * FROM search::near('locations',geo::point(1,0),0)"
            )
            .rows
            .len(),
            1
        );
        c.check_collection_integrity("places", Default::default())
            .unwrap();
    }

    #[test]
    fn interrupted_text_build_and_write_preserve_indexes_and_prior_work() {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for i in 1..=3 {
            q(
                &c,
                &format!("INSERT INTO docs {{id:type::record('docs',{i}),title:'original'}}"),
            );
        }
        let fired = arm_after_write(&c);
        let result = c.create_fulltext_index("docs", "titles", vec![vec!["title".into()]], false);
        c.engine.set_progress_handler(0, None);
        assert!(fired.load(Ordering::SeqCst));
        assert_eq!(result.unwrap_err().code(), "FDB_CANCELLED");
        assert!(c.catalog("docs").unwrap().indexes.is_empty());
        drop(db.connect().unwrap());
        c.create_fulltext_index("docs", "titles", vec![vec!["title".into()]], false)
            .unwrap();
        let sql =
            "SELECT id,score FROM search::text('titles','original',10) ORDER BY score DESC,id";
        let before = q(&c, sql).rows;
        q(&c, "BEGIN");
        q(&c, "INSERT INTO prior {value:1}");
        let fired = arm_after_write(&c);
        let result = c.execute("UPDATE docs SET title='changed'", &Parameters::new());
        c.engine.set_progress_handler(0, None);
        assert!(fired.load(Ordering::SeqCst));
        assert_eq!(result.unwrap_err().code(), "FDB_CANCELLED");
        assert_eq!(q(&c, sql).rows, before);
        assert_eq!(q(&c, "SELECT * FROM prior").rows.len(), 1);
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
        q(&c, "UPDATE docs SET title='changed'");
        assert!(q(&c, sql).rows.is_empty());
        q(&c, "ROLLBACK");
        assert_eq!(q(&c, sql).rows, before);
    }

    #[test]
    fn interrupted_index_build_removes_metadata_and_physical_storage() {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        for i in 1..=3 {
            q(&c, &format!("INSERT INTO docs {{value:{i}}}"));
        }
        let fired = arm_after_write(&c);
        let result = c.create_index("docs", "build_idx", vec!["value".into()], true);
        c.engine.set_progress_handler(0, None);
        assert!(fired.load(Ordering::SeqCst));
        assert_eq!(result.unwrap_err().code(), "FDB_CANCELLED");
        assert!(c.catalog("docs").unwrap().indexes.is_empty());
        let names = c
            .run(
                "SELECT name FROM sqlite_schema WHERE name='build_idx' OR name LIKE '__romedb_i_%'",
                &[],
            )
            .unwrap();
        assert!(names.is_empty(), "orphaned index storage: {names:?}");
        q(&c, "CREATE UNIQUE INDEX build_idx ON docs(value)");
        for i in 1..=3 {
            assert_eq!(
                c.lookup_index("docs", "build_idx", &Value::Integer(i))
                    .unwrap()
                    .len(),
                1
            );
        }
    }
    #[test]
    fn interrupted_catalog_mutations_restore_schema_and_prior_transaction_work() {
        for statement in [
            "DROP INDEX values_idx",
            "DROP TABLE docs",
            "DEFINE FIELD OVERWRITE value ON docs TYPE number REQUIRED",
        ] {
            for outer in [false, true] {
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                q(&c, "CREATE TABLE docs");
                q(&c, "DEFINE FIELD value ON docs TYPE integer REQUIRED");
                q(&c, "CREATE UNIQUE INDEX values_idx ON docs(value)");
                q(&c, "INSERT INTO docs {id:docs:a,value:1}");
                q(&c, "CREATE TABLE prior(value INTEGER)");
                let info = q(&c, "INFO FOR TABLE docs").rows;
                let schema = c
                    .run("SELECT name,sql FROM sqlite_schema ORDER BY name", &[])
                    .unwrap();
                if outer {
                    q(&c, "BEGIN");
                    q(&c, "INSERT INTO prior VALUES (1)");
                }
                let fired = arm_after_write(&c);
                let report = c.execute_report(statement, &Parameters::new());
                c.engine.set_progress_handler(0, None);
                assert!(fired.load(Ordering::SeqCst), "{statement}");
                assert_eq!(
                    report.result.unwrap_err().code(),
                    "FDB_CANCELLED",
                    "{statement}"
                );
                assert_eq!(
                    report.transaction_after,
                    if outer {
                        crate::TransactionState::Active
                    } else {
                        crate::TransactionState::Autocommit
                    }
                );
                assert_eq!(q(&c, "INFO FOR TABLE docs").rows, info, "{statement}");
                assert_eq!(
                    c.run("SELECT name,sql FROM sqlite_schema ORDER BY name", &[])
                        .unwrap(),
                    schema,
                    "{statement}"
                );
                assert_eq!(
                    c.lookup_index("docs", "values_idx", &Value::Integer(1))
                        .unwrap()
                        .len(),
                    1
                );
                assert_eq!(q(&c, "SELECT * FROM prior").rows.len(), usize::from(outer));
                assert!(c
                    .execute("INSERT INTO docs {value:1.5}", &Parameters::new())
                    .is_err());
                q(&c, statement);
                if outer {
                    q(&c, "ROLLBACK");
                    assert_eq!(q(&c, "INFO FOR TABLE docs").rows, info);
                    assert_eq!(
                        c.lookup_index("docs", "values_idx", &Value::Integer(1))
                            .unwrap()
                            .len(),
                        1
                    );
                    assert!(q(&c, "SELECT * FROM prior").rows.is_empty());
                }
            }
        }
    }
    #[test]
    fn cancelled_index_replacement_restores_old_index_on_outer_rollback() {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(&c, "INSERT INTO docs {value:1,other:10}");
        q(&c, "INSERT INTO docs {value:2,other:20}");
        q(&c, "CREATE UNIQUE INDEX values_idx ON docs(value)");
        let before = q(&c, "INFO FOR TABLE docs").rows;
        q(&c, "BEGIN");
        q(&c, "DROP INDEX values_idx");
        let fired = arm_after_write(&c);
        let report = c.execute_report(
            "CREATE UNIQUE INDEX values_idx ON docs(other)",
            &Parameters::new(),
        );
        c.engine.set_progress_handler(0, None);
        assert!(fired.load(Ordering::SeqCst));
        assert_eq!(report.result.unwrap_err().code(), "FDB_CANCELLED");
        assert_eq!(report.transaction_after, crate::TransactionState::Active);
        assert!(c.catalog("docs").unwrap().indexes.is_empty());
        q(&c, "ROLLBACK");
        assert_eq!(q(&c, "INFO FOR TABLE docs").rows, before);
        assert_eq!(
            c.lookup_index("docs", "values_idx", &Value::Integer(1))
                .unwrap()
                .len(),
            1
        );
        assert!(c
            .lookup_index("docs", "values_idx", &Value::Integer(10))
            .unwrap()
            .is_empty());
        q(&c, "BEGIN");
        q(&c, "DROP INDEX values_idx");
        q(&c, "CREATE UNIQUE INDEX values_idx ON docs(other)");
        q(&c, "COMMIT");
        assert_eq!(
            c.lookup_index("docs", "values_idx", &Value::Integer(10))
                .unwrap()
                .len(),
            1
        );
        assert!(c
            .lookup_index("docs", "values_idx", &Value::Integer(1))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn interrupted_cte_lowering_and_reads_do_not_fall_back_or_run_writes() {
        use std::sync::atomic::AtomicUsize;
        let statements = [
            "WITH l AS (SELECT * FROM labels), a AS (SELECT value FROM docs) SELECT a.value,l.value FROM a JOIN l ON a.value=l.value",
            "WITH l AS (SELECT * FROM labels), a AS (SELECT value FROM docs) INSERT INTO copied (value) SELECT a.value FROM a JOIN l ON a.value=l.value",
        ];
        for statement in statements {
            for after in [1, 5, 20] {
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                q(&c, "CREATE TABLE docs");
                q(&c, "CREATE TABLE copied");
                q(&c, "CREATE UNIQUE INDEX copied_value ON copied(value)");
                q(&c, "CREATE TABLE labels(value INTEGER)");
                q(&c, "CREATE TABLE prior(value INTEGER)");
                for value in 1..=3 {
                    q(&c, &format!("INSERT INTO docs {{value:{value}}}"));
                    q(&c, &format!("INSERT INTO labels VALUES ({value})"));
                }
                q(&c, "BEGIN");
                q(&c, "INSERT INTO prior VALUES (99)");
                let steps = Arc::new(AtomicUsize::new(0));
                let count = steps.clone();
                c.engine.set_progress_handler(
                    1,
                    Some(Box::new(move || {
                        count.fetch_add(1, Ordering::SeqCst) + 1 == after
                    })),
                );
                let report = c.execute_report(statement, &Parameters::new());
                c.engine.set_progress_handler(0, None);
                assert!(
                    steps.load(Ordering::SeqCst) >= after,
                    "interruption point not reached"
                );
                assert_eq!(
                    report.result.unwrap_err().code(),
                    "FDB_CANCELLED",
                    "step {after}: {statement}"
                );
                assert_eq!(report.transaction_after, crate::TransactionState::Active);
                assert_eq!(
                    q(&c, "SELECT * FROM prior").rows,
                    vec![vec![Value::Integer(99)]]
                );
                assert!(q(&c, "SELECT * FROM copied").rows.is_empty());
                for value in 1..=3 {
                    assert!(c
                        .lookup_index("copied", "copied_value", &Value::Integer(value))
                        .unwrap()
                        .is_empty());
                }
                assert_eq!(
                    q(&c, "SELECT count(*) FROM docs").rows,
                    vec![vec![Value::Integer(3)]]
                );
                q(&c, statement);
                q(&c, "ROLLBACK");
                assert!(q(&c, "SELECT * FROM copied").rows.is_empty());
                assert!(q(&c, "SELECT * FROM prior").rows.is_empty());
            }
        }
    }

    #[test]
    fn native_target_subquery_cancellation_matches_native_transaction_disposition() {
        check_native_target_subquery_cancellation(false);
    }

    #[test]
    fn native_target_after_write_cancellation_requires_interrupt_propagation() {
        check_native_target_subquery_cancellation(true);
    }

    fn check_native_target_subquery_cancellation(after_write: bool) {
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        use std::sync::atomic::AtomicUsize;
        use turso_ext::{scalar, ResultCode, Value as ExtValue};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        static WRITES: AtomicUsize = AtomicUsize::new(0);
        #[scalar(name = "native_write_tick")]
        fn native_write_tick(_: &[ExtValue]) -> ExtValue {
            WRITES.fetch_add(1, Ordering::SeqCst);
            ExtValue::from_integer(0)
        }
        #[scalar(name = "native_subquery_tick")]
        fn native_subquery_tick(args: &[ExtValue]) -> ExtValue {
            CALLS.fetch_add(1, Ordering::SeqCst);
            ExtValue::from_integer(args[0].to_integer().expect("integer source"))
        }
        for predicate in [
            "value IN (SELECT native_subquery_tick(value)+native_subquery_tick(0) FROM docs)",
            "value NOT IN (SELECT native_subquery_tick(value)+native_subquery_tick(10) FROM docs)",
            "EXISTS (SELECT value FROM docs WHERE native_subquery_tick(value)+native_subquery_tick(0)=3)",
            "value <= (SELECT max(native_subquery_tick(value)+native_subquery_tick(0)) FROM docs)",
        ] {
            for after in if after_write { &[0][..] } else { &[2, 4][..] } {
                let after = *after;
                for outer in [false, true] {
                    let mut observed = Vec::new();
                    for logical in [false, true] {
                        let db = Database::open(":memory:").unwrap();
                        let c = db.connect().unwrap();
                        unsafe {
                            let api = c.engine._build_turso_ext();
                            let code = (api.register_scalar_function)(api.ctx, c"native_subquery_tick".as_ptr(), 1, false, 0, native_subquery_tick, None, None);
                            assert_eq!(code, ResultCode::OK);
                            let code = (api.register_scalar_function)(api.ctx, c"native_write_tick".as_ptr(), 0, false, 0, native_write_tick, None, None);
                            c.engine._free_extension_ctx(api);
                            assert_eq!(code, ResultCode::OK);
                        }
                        q(&c, "CREATE TABLE docs");
                        q(&c, "INSERT INTO docs(value) VALUES (1),(2),(3)");
                        q(&c, "CREATE TABLE source_native(value INTEGER)");
                        q(&c, "INSERT INTO source_native VALUES (1),(2),(3)");
                        q(&c, "CREATE TABLE target(value INTEGER UNIQUE)");
                        q(&c, "CREATE TABLE effects(value INTEGER)");
                        q(&c, "CREATE TRIGGER target_effect AFTER INSERT ON target BEGIN INSERT INTO effects VALUES (new.value); SELECT native_write_tick(); END");
                        q(&c, "CREATE TABLE prior(value INTEGER)");
                        if outer { q(&c, "BEGIN"); q(&c, "INSERT INTO prior VALUES (99)"); }
                        let sql = format!("INSERT INTO target SELECT value FROM docs WHERE {predicate}");
                        let sql = if logical { sql } else { sql.replace("FROM docs", "FROM source_native") };
                        CALLS.store(0, Ordering::SeqCst);
                        WRITES.store(0, Ordering::SeqCst);
                        let fired = Arc::new(AtomicBool::new(false));
                        let flag = fired.clone();
                        c.engine.set_progress_handler(1, Some(Box::new(move || (if after == 0 { WRITES.load(Ordering::SeqCst) > 0 } else { CALLS.load(Ordering::SeqCst) >= after }) && !flag.swap(true, Ordering::SeqCst))));
                        let report = c.execute_report(&sql, &Parameters::new());
                        c.engine.set_progress_handler(0, None);
                        assert!(fired.load(Ordering::SeqCst), "{sql}");
                        if after == 0 {
                            assert_eq!(WRITES.load(Ordering::SeqCst), 1);
                        } else {
                            assert_eq!(CALLS.load(Ordering::SeqCst), after);
                            assert_eq!(WRITES.load(Ordering::SeqCst), 0);
                        }
                        let error = report.result.unwrap_err();
                        assert_eq!(error.code(), "FDB_CANCELLED", "{sql}");
                        assert!(q(&c, "SELECT * FROM target").rows.is_empty());
                        assert!(q(&c, "SELECT * FROM effects").rows.is_empty());
                        observed.push((report.transaction_after, q(&c, "SELECT * FROM prior").rows));
                        assert_eq!(c.check_collection_integrity("docs", Default::default()).unwrap().documents, 3);
                        assert_eq!(q(&c, &sql).affected, 3);
                        assert_eq!(q(&c, "SELECT value FROM target ORDER BY value").rows, vec![vec![Value::Integer(1)], vec![Value::Integer(2)], vec![Value::Integer(3)]]);
                        assert_eq!(q(&c, "SELECT value FROM effects ORDER BY value").rows, q(&c, "SELECT value FROM target ORDER BY value").rows);
                        if c.transaction_state() == crate::TransactionState::Active {
                            q(&c, "ROLLBACK");
                            assert!(q(&c, "SELECT * FROM target").rows.is_empty());
                        assert!(q(&c, "SELECT * FROM effects").rows.is_empty());
                        }
                    }
                    assert_eq!(observed[0], observed[1], "{predicate}, after={after}, outer={outer}");
                }
            }
        }
    }

    #[test]
    fn interrupted_compound_and_subquery_sources_discard_rows_and_allow_exact_retry() {
        use std::sync::atomic::AtomicUsize;
        use turso_ext::{scalar, ResultCode, Value as ExtValue};
        static ROWS: AtomicUsize = AtomicUsize::new(0);
        #[scalar(name = "union_source_tick")]
        fn union_source_tick(args: &[ExtValue]) -> ExtValue {
            ROWS.fetch_add(1, Ordering::SeqCst);
            ExtValue::from_integer(args[0].to_integer().expect("integer source"))
        }
        for (operator, insert) in [
            "UNION ALL",
            "UNION",
            "INTERSECT",
            "EXCEPT",
            "IN",
            "NOT IN",
            "NATIVE IN",
            "NATIVE NOT IN",
            "EXISTS",
            "SCALAR",
            "LIMIT",
            "OFFSET",
            "UNION LIMIT",
            "UNION OFFSET",
        ]
        .into_iter()
        .flat_map(|operator| [false, true].map(|insert| (operator, insert)))
        {
            let expected = match operator {
                "INTERSECT" => vec![],
                "EXCEPT" | "IN" | "NOT IN" | "EXISTS" | "SCALAR" | "LIMIT" | "OFFSET"
                | "UNION LIMIT" | "UNION OFFSET" | "NATIVE IN" | "NATIVE NOT IN" => vec![1, 2, 3],
                _ => vec![1, 2, 3, 11, 12, 13],
            };
            for after in [2, 4] {
                for outer in [false, true] {
                    let db = Database::open(":memory:").unwrap();
                    let c = db.connect().unwrap();
                    unsafe {
                        let api = c.engine._build_turso_ext();
                        let code = (api.register_scalar_function)(
                            api.ctx,
                            c"union_source_tick".as_ptr(),
                            1,
                            false,
                            0,
                            union_source_tick,
                            None,
                            None,
                        );
                        c.engine._free_extension_ctx(api);
                        assert_eq!(code, ResultCode::OK);
                    }
                    q(&c, "CREATE TABLE native_source(value INTEGER)");
                    q(&c, "INSERT INTO native_source VALUES (1),(2),(3)");
                    q(&c, "CREATE TABLE docs");
                    q(&c, "CREATE TABLE copied");
                    q(&c, "CREATE UNIQUE INDEX copied_value ON copied(value)");
                    q(&c, "CREATE TABLE prior(value INTEGER)");
                    for value in 1..=3 {
                        q(&c, &format!("INSERT INTO docs {{value:{value}}}"));
                    }
                    if outer {
                        q(&c, "BEGIN");
                        q(&c, "INSERT INTO prior VALUES (99)");
                    }
                    let prefix = if insert {
                        "INSERT INTO copied(value) "
                    } else {
                        ""
                    };
                    let source = match operator {
                        "NATIVE IN" => "SELECT value FROM docs WHERE value IN (SELECT union_source_tick(value)+union_source_tick(0) FROM native_source)".to_owned(),
                        "NATIVE NOT IN" => "SELECT value FROM docs WHERE value NOT IN (SELECT union_source_tick(value)+union_source_tick(10) FROM native_source)".to_owned(),
                        "IN" => "SELECT value FROM docs WHERE value IN (SELECT union_source_tick(value)+union_source_tick(0) FROM docs)".to_owned(),
                        "NOT IN" => "SELECT value FROM docs WHERE value NOT IN (SELECT union_source_tick(value)+union_source_tick(10) FROM docs)".to_owned(),
                        "EXISTS" => "SELECT value FROM docs WHERE EXISTS (SELECT value FROM docs WHERE union_source_tick(value)+union_source_tick(0)=3)".to_owned(),
                        "SCALAR" => "SELECT value FROM docs WHERE value <= (SELECT max(union_source_tick(value)+union_source_tick(0)) FROM docs)".to_owned(),
                        "LIMIT" => "SELECT value FROM docs LIMIT (SELECT max(union_source_tick(value)+union_source_tick(0)) FROM docs)".to_owned(),
                        "OFFSET" => "SELECT value FROM docs LIMIT -1 OFFSET (SELECT sum(union_source_tick(value)+union_source_tick(0)-value) FROM docs)".to_owned(),
                        "UNION LIMIT" => "SELECT value FROM docs UNION SELECT value FROM docs LIMIT (SELECT max(union_source_tick(value)+union_source_tick(0)) FROM docs)".to_owned(),
                        "UNION OFFSET" => "SELECT value FROM docs UNION SELECT value FROM docs LIMIT -1 OFFSET (SELECT sum(union_source_tick(value)+union_source_tick(0)-value) FROM docs)".to_owned(),
                        _ => format!("SELECT union_source_tick(value) AS value FROM docs {operator} SELECT union_source_tick(value+10) FROM docs"),
                    };
                    let statement = format!("{prefix}{source}");
                    ROWS.store(0, Ordering::SeqCst);
                    let fired = Arc::new(AtomicBool::new(false));
                    let flag = fired.clone();
                    c.engine.set_progress_handler(
                        1,
                        Some(Box::new(move || {
                            ROWS.load(Ordering::SeqCst) >= after
                                && !flag.swap(true, Ordering::SeqCst)
                        })),
                    );
                    let report = c.execute_report(&statement, &Parameters::new());
                    c.engine.set_progress_handler(0, None);
                    assert!(
                        fired.load(Ordering::SeqCst),
                        "{operator}, source point {after}, insert={insert}, outer={outer}, calls={}, result={:?}", ROWS.load(Ordering::SeqCst), report.result
                    );
                    assert_eq!(ROWS.load(Ordering::SeqCst), after);
                    assert_eq!(
                        report.result.unwrap_err().code(),
                        "FDB_CANCELLED",
                        "{statement}"
                    );
                    assert_eq!(
                        report.transaction_after,
                        if outer {
                            crate::TransactionState::Active
                        } else {
                            crate::TransactionState::Autocommit
                        }
                    );
                    assert_eq!(q(&c, "SELECT * FROM prior").rows.len(), usize::from(outer));
                    assert_eq!(
                        c.check_collection_integrity("copied", Default::default())
                            .unwrap()
                            .documents,
                        0
                    );
                    assert_eq!(
                        c.check_collection_integrity("docs", Default::default())
                            .unwrap()
                            .documents,
                        3
                    );
                    let retry = q(&c, &statement);
                    if insert {
                        assert_eq!(retry.affected, expected.len() as i64);
                        assert_eq!(
                            c.check_collection_integrity("copied", Default::default())
                                .unwrap()
                                .documents,
                            expected.len() as u64
                        );
                    } else {
                        assert_eq!(retry.rows.len(), expected.len());
                    }
                    let rows = if insert {
                        q(&c, "SELECT value FROM copied").rows
                    } else {
                        retry.rows
                    };
                    let mut values = rows
                        .into_iter()
                        .map(|row| match row.as_slice() {
                            [Value::Integer(value)] => *value,
                            _ => panic!("unexpected retry row: {row:?}"),
                        })
                        .collect::<Vec<_>>();
                    values.sort_unstable();
                    assert_eq!(values, expected, "{statement}");
                    if outer {
                        q(&c, "ROLLBACK");
                        assert_eq!(
                            c.check_collection_integrity("copied", Default::default())
                                .unwrap()
                                .documents,
                            0
                        );
                        assert!(q(&c, "SELECT * FROM prior").rows.is_empty());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod deadline_returning_tests {
    use super::*;
    use std::sync::{atomic::AtomicUsize, Mutex};
    use std::time::{Duration, Instant};
    use turso_ext::{scalar, ResultCode, Value as ExtValue};
    static TOKEN: Mutex<Option<CancellationToken>> = Mutex::new(None);
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    #[scalar(name = "deadline_returning_tick")]
    fn deadline_returning_tick(args: &[ExtValue]) -> ExtValue {
        CALLS.fetch_add(1, Ordering::SeqCst);
        let token = TOKEN.lock().unwrap().as_ref().unwrap().clone();
        while !token.is_cancelled() {
            std::thread::sleep(Duration::from_millis(1));
        }
        ExtValue::from_integer(args[0].to_integer().unwrap())
    }

    #[test]
    fn expiry_during_returning_restores_mutations_and_indexes() {
        for (collection, outer) in [(false, false), (false, true), (true, false), (true, true)] {
            let db = crate::Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            let p = Parameters::new();
            unsafe {
                let api = c.engine._build_turso_ext();
                let code = (api.register_scalar_function)(
                    api.ctx,
                    c"deadline_returning_tick".as_ptr(),
                    1,
                    false,
                    0,
                    deadline_returning_tick,
                    None,
                    None,
                );
                c.engine._free_extension_ctx(api);
                assert_eq!(code, ResultCode::OK);
            }
            c.execute(
                if collection {
                    "CREATE TABLE docs"
                } else {
                    "CREATE TABLE docs(n INTEGER)"
                },
                &p,
            )
            .unwrap();
            c.execute("CREATE UNIQUE INDEX docs_n ON docs(n)", &p)
                .unwrap();
            c.execute("INSERT INTO docs(n) VALUES(1)", &p).unwrap();
            if outer {
                c.execute("BEGIN", &p).unwrap();
                c.execute("INSERT INTO docs(n) VALUES(2)", &p).unwrap();
            }
            let before = c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows;
            let token = CancellationToken::with_deadline(Instant::now() + Duration::from_secs(1));
            *TOKEN.lock().unwrap() = Some(token.clone());
            CALLS.store(0, Ordering::SeqCst);
            let error = c
                .write_with_result_limits_cancellable(
                    "UPDATE docs SET n=n+10 RETURNING deadline_returning_tick(n)",
                    &p,
                    crate::ResultLimits {
                        max_rows: 10,
                        max_payload_bytes: 1000,
                    },
                    &token,
                )
                .unwrap_err();
            *TOKEN.lock().unwrap() = None;
            assert_eq!(error.code(), "FDB_CANCELLED");
            assert_eq!(CALLS.load(Ordering::SeqCst), 1);
            assert_eq!(
                c.transaction_state(),
                if outer {
                    crate::TransactionState::Active
                } else {
                    crate::TransactionState::Autocommit
                }
            );
            assert_eq!(
                c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows,
                before
            );
            if collection {
                assert_eq!(
                    c.check_collection_integrity("docs", Default::default())
                        .unwrap()
                        .documents,
                    before.len() as u64
                );
            }
            let fresh = CancellationToken::with_deadline(Instant::now() + Duration::from_secs(60));
            c.execute_cancellable("UPDATE docs SET n=n+10", &p, &fresh)
                .unwrap();
            if outer {
                c.execute("ROLLBACK", &p).unwrap();
            }
            assert_eq!(
                c.execute("SELECT n FROM docs WHERE n=1", &p)
                    .unwrap()
                    .rows
                    .len(),
                usize::from(outer)
            );
        }
    }
}
