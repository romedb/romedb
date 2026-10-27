//! Retained work for checked customer statements.
use crate::{Connection, Error, Parameters, QueryResult, Result, ResultLimits};
use serde::Serialize;
use std::sync::Arc;
use turso_core::execution_meter::{ExecutionLimits, ExecutionMeter};

#[derive(Debug, Clone, Copy, Default)]
pub struct ReadWorkLimits {
    pub max_rows_read: Option<u64>,
    pub max_vm_steps: Option<u64>,
}

/// Instrumented customer-statement work, including failed execution. Physical
/// engine counter coverage still applies; this is not a logical-write counter.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ReadWork {
    pub rows_read: u64,
    pub vm_steps: u64,
    pub read_budget_exhausted: bool,
    pub vm_budget_exhausted: bool,
}

#[derive(Debug)]
pub struct MeteredRead {
    pub outcome: Result<QueryResult>,
    pub work: ReadWork,
}

/// Infrastructure work for logical schema inspection, not customer row reads.
#[derive(Debug, Clone, Copy, Default)]
pub struct InfoWorkLimits {
    pub max_catalog_rows_read: Option<u64>,
    pub max_vm_steps: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct InfoWork {
    pub catalog_rows_read: u64,
    pub vm_steps: u64,
    pub catalog_budget_exhausted: bool,
    pub vm_budget_exhausted: bool,
}

#[derive(Debug)]
pub struct MeteredInfo {
    pub outcome: Result<QueryResult>,
    pub work: InfoWork,
}

/// Limits shared by customer reads and row mutations within one checked write.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteWorkLimits {
    pub max_rows_read: Option<u64>,
    pub max_row_mutations: Option<u64>,
    pub max_vm_steps: Option<u64>,
}

/// Attempted work survives rollback. `row_mutations` is never a committed-write
/// receipt: the caller must resolve the enclosing transaction separately.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct WriteWork {
    pub rows_read: u64,
    pub row_mutations: u64,
    pub vm_steps: u64,
    pub read_budget_exhausted: bool,
    pub mutation_budget_exhausted: bool,
    pub vm_budget_exhausted: bool,
}

#[derive(Debug)]
pub struct MeteredWrite {
    pub outcome: Result<QueryResult>,
    pub work: WriteWork,
}

/// Checked table/collection and managed index creation. Frontend catalog
/// lookups/updates and transaction cleanup are excluded; native schema work is
/// retained separately. General search/extension coverage is still unqualified.
#[derive(Debug, Clone, Copy, Default)]
pub struct CreateWorkLimits {
    pub max_rows_read: Option<u64>,
    pub max_schema_rows_read: Option<u64>,
    pub max_row_mutations: Option<u64>,
    pub max_vm_steps: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CreateWork {
    pub rows_read: u64,
    pub schema_rows_read: u64,
    pub row_mutations: u64,
    pub vm_steps: u64,
    pub read_budget_exhausted: bool,
    pub schema_budget_exhausted: bool,
    pub mutation_budget_exhausted: bool,
    pub vm_budget_exhausted: bool,
}

#[derive(Debug)]
pub struct MeteredCreate {
    pub outcome: Result<QueryResult>,
    pub work: CreateWork,
}

/// Logical field, relation and function declarations use the same retained
/// counter shape as creation. Catalog I/O and JavaScript compilation are excluded.
pub type SchemaWorkLimits = CreateWorkLimits;
pub type SchemaWork = CreateWork;
pub type MeteredSchema = MeteredCreate;

enum SchemaStatement {
    Logical,
    Native,
    DropTable(String),
}

pub type DdlWorkLimits = CreateWorkLimits;
pub type DdlWork = CreateWork;
pub type MeteredDdl = MeteredCreate;

struct Scope<'a>(&'a Connection);
impl Drop for Scope<'_> {
    fn drop(&mut self) {
        // Statement locals unwind before this scope, including after a panic.
        let _ = self.0.engine.set_execution_meter(None);
        *self.0.work_meter.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.0
            .meter_schema_changes
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.0
            .meter_catalog_reads
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Connection {
    /// Execute checked table/collection, scalar and managed search-index creation.
    /// `None` means a different statement family: nothing has executed, and the
    /// caller must not interpret it as measured zero work. Invalid syntax returns
    /// a measured error with zero dispatched work. Native engine schema visits
    /// have an independent limit; logical frontend catalog access is excluded.
    /// CTAS mutation attempts require a separately verified transaction commit
    /// before being reported as committed writes. Serialize connection access.
    pub fn create_metered(
        &self,
        sql: &str,
        params: &Parameters,
        result_limits: ResultLimits,
        work_limits: CreateWorkLimits,
    ) -> Option<MeteredCreate> {
        self.with_statement_timeout(sql, |sql| {
            let syntax = crate::parser_stack(|| -> Result<Option<SchemaStatement>> {
                use romeql_parser::Statement;
                use turso_parser::ast::{Cmd, Stmt};
                Ok(match romeql_parser::parse(sql)? {
                    Statement::CreateCollection { .. }
                    | Statement::CreateEdgeTable { .. }
                    | Statement::CreateIndex { .. }
                    | Statement::CreateSpatialIndex { .. }
                    | Statement::CreateArrayIndex { .. }
                    | Statement::CreateFullTextIndex { .. }
                    | Statement::CreateVectorIndex { .. } => Some(SchemaStatement::Logical),
                    Statement::Sql(sql) => match crate::select::parsed(&sql)? {
                        Cmd::Stmt(
                            Stmt::CreateTable {
                                temporary: false, ..
                            }
                            | Stmt::CreateIndex { using: None, .. },
                        ) => Some(SchemaStatement::Native),
                        _ => None,
                    },
                    _ => None,
                })
            });
            Ok(self.execute_metered_schema(sql, params, result_limits, work_limits, syntax))
        })
        .unwrap_or_else(|error| {
            self.execute_metered_schema(sql, params, result_limits, work_limits, Err(error))
        })
    }

    /// Execute logical field definition/removal, relation definition/removal and
    /// function creation/replacement/removal. Field validation reads existing
    /// customer documents; metadata writes never become logical row mutations.
    /// Unsupported families return None without executing. Frontend catalog I/O
    /// and JavaScript compilation are outside the engine VM/read counters.
    pub fn schema_metered(
        &self,
        sql: &str,
        params: &Parameters,
        result_limits: ResultLimits,
        work_limits: SchemaWorkLimits,
    ) -> Option<MeteredSchema> {
        self.with_statement_timeout(sql, |sql| {
            let syntax = crate::parser_stack(|| -> Result<Option<SchemaStatement>> {
                use romeql_parser::Statement;
                Ok(match romeql_parser::parse(sql)? {
                    Statement::DefineSchema { .. }
                    | Statement::DefineField { .. }
                    | Statement::RemoveField { .. }
                    | Statement::DefineRelation { .. }
                    | Statement::DropRelation { .. }
                    | Statement::CreateFunction { .. }
                    | Statement::DropFunction { .. } => Some(SchemaStatement::Logical),
                    Statement::Sql(sql) => match crate::select::parsed(&sql)? {
                        turso_parser::ast::Cmd::Stmt(turso_parser::ast::Stmt::Reindex {
                            name: Some(name),
                        }) if name
                            .db_name
                            .as_ref()
                            .is_none_or(|db| db.as_str().eq_ignore_ascii_case("main"))
                            && self.collections()?.iter().any(|collection| {
                                collection.indexes.iter().any(|index| {
                                    index.name.eq_ignore_ascii_case(name.name.as_str())
                                })
                            }) =>
                        {
                            Some(SchemaStatement::Logical)
                        }
                        _ => None,
                    },
                    _ => None,
                })
            });
            Ok(self.execute_metered_schema(sql, params, result_limits, work_limits, syntax))
        })
        .unwrap_or_else(|error| {
            self.execute_metered_schema(sql, params, result_limits, work_limits, Err(error))
        })
    }

    /// Execute ordinary main-schema CREATE/DROP VIEW, DROP TABLE/INDEX and
    /// ALTER TABLE statements. Materialized views remain outside this adapter.
    /// Table deletion counts its rows inside the same atomic scope, charging the
    /// real counting reads. It rejects an insufficient mutation budget before
    /// destruction and records bulk mutations only after DROP completes. Index
    /// and catalog writes are excluded. Virtual/attached schemas return None.
    pub fn ddl_metered(
        &self,
        sql: &str,
        params: &Parameters,
        result_limits: ResultLimits,
        work_limits: DdlWorkLimits,
    ) -> Option<MeteredDdl> {
        self.with_statement_timeout(sql, |sql| {
        let syntax = crate::parser_stack(|| -> Result<Option<SchemaStatement>> {
            use turso_parser::ast::{Cmd, Stmt};
            let romeql_parser::Statement::Sql(native) = romeql_parser::parse(sql)? else {
                return Ok(None);
            };
            Ok(match crate::select::parsed(&native)? {
                Cmd::Stmt(Stmt::DropTable { tbl_name, .. })
                    if self.ddl_targets_main(&tbl_name)? =>
                {
                    if self.virtual_drop_target(tbl_name.name.as_str())? {
                        return Ok(None);
                    }
                    Some(SchemaStatement::DropTable(
                        tbl_name.name.as_str().to_owned(),
                    ))
                }
                Cmd::Stmt(Stmt::CreateView {
                    temporary: false,
                    view_name,
                    ..
                }) if view_name
                    .db_name
                    .as_ref()
                    .is_none_or(|db| db.as_str().eq_ignore_ascii_case("main")) =>
                {
                    Some(SchemaStatement::Native)
                }
                Cmd::Stmt(Stmt::DropView { view_name, .. })
                    if self.ddl_targets_main(&view_name)? =>
                {
                    let rows = self.run("SELECT sql FROM main.sqlite_schema WHERE name=?1 COLLATE NOCASE AND type='view'", &[crate::text(view_name.name.as_str())])?;
                    if let Some(crate::EngineValue::Text(sql)) =
                        rows.first().and_then(|row| row.first())
                    {
                        if matches!(
                            crate::select::parsed(sql.as_str())?,
                            Cmd::Stmt(Stmt::CreateMaterializedView { .. })
                        ) {
                            return Ok(None);
                        }
                    }
                    Some(SchemaStatement::Native)
                }
                Cmd::Stmt(Stmt::DropIndex { idx_name, .. })
                    if self.ddl_targets_main(&idx_name)? =>
                {
                    Some(SchemaStatement::Logical)
                }
                Cmd::Stmt(Stmt::AlterTable(table)) if self.ddl_targets_main(&table.name)? => {
                    if self.virtual_drop_target(table.name.name.as_str())? {
                        return Ok(None);
                    }
                    Some(SchemaStatement::Native)
                }
                _ => None,
            })
        });
            Ok(self.execute_metered_schema(sql, params, result_limits, work_limits, syntax))
        }).unwrap_or_else(|error| self.execute_metered_schema(sql, params, result_limits, work_limits, Err(error)))
    }

    /// Execute EXPLAIN or EXPLAIN QUERY PLAN without executing the explained
    /// statement. Retains engine work and applies result bounds. Parsing and
    /// planning are not VM steps; this is not a planning CPU/memory budget.
    pub fn explain_metered(
        &self,
        sql: &str,
        params: &Parameters,
        result_limits: ResultLimits,
        work_limits: DdlWorkLimits,
    ) -> Option<MeteredDdl> {
        self.with_statement_timeout(sql, |sql| {
            let syntax = crate::parser_stack(|| -> Result<Option<SchemaStatement>> {
                use turso_parser::ast::Cmd;
                let romeql_parser::Statement::Sql(native) = romeql_parser::parse(sql)? else {
                    return Ok(None);
                };
                // Match the normal SELECT lowering's namespace/record syntax before
                // classifying; parsing raw RomeQL would reject managed search plans.
                let expanded = crate::select::expand_paths(&crate::select::expand_records(
                    Some(self),
                    &native,
                )?)?;
                if !matches!(
                    crate::select::parsed(&expanded)?,
                    Cmd::Explain(_) | Cmd::ExplainQueryPlan { .. }
                ) {
                    return Ok(None);
                }
                // ANN lowering currently materializes candidates before preparing
                // the outer SQL. Its graph/storage work is not qualified by this
                // adapter, so never report that path as a zero-read explanation.
                if romeql_parser::tokenize(&expanded)?.iter().any(|token| {
                    token.kind == romeql_parser::Kind::Word && token.text == "__romedb_vector"
                }) {
                    return Err(Error::Unsupported(
                        "metered vector EXPLAIN is not supported".into(),
                    ));
                }
                Ok(Some(SchemaStatement::Native))
            });
            Ok(self.execute_metered_schema(sql, params, result_limits, work_limits, syntax))
        })
        .unwrap_or_else(|error| {
            self.execute_metered_schema(sql, params, result_limits, work_limits, Err(error))
        })
    }

    fn ddl_targets_main(&self, name: &turso_parser::ast::QualifiedName) -> Result<bool> {
        if let Some(database) = &name.db_name {
            return Ok(database.as_str().eq_ignore_ascii_case("main"));
        }
        if self
            .engine
            .list_all_databases()
            .iter()
            .any(|(id, _, _)| *id == turso_core::TEMP_DB_ID)
            && !self
                .run(
                    "SELECT name FROM temp.sqlite_schema WHERE name=?1 COLLATE NOCASE",
                    &[crate::text(name.name.as_str())],
                )?
                .is_empty()
        {
            return Ok(false);
        }
        // Unqualified attached targets are outside this adapter. Existing main
        // objects still take precedence over attached schemas.
        Ok(self.engine.list_attached_databases().is_empty()
            || !self
                .run(
                    "SELECT name FROM main.sqlite_schema WHERE name=?1 COLLATE NOCASE",
                    &[crate::text(name.name.as_str())],
                )?
                .is_empty())
    }

    fn virtual_drop_target(&self, table: &str) -> Result<bool> {
        let rows = self.run(
            "SELECT sql FROM main.sqlite_schema WHERE name=?1 COLLATE NOCASE AND type='table'",
            &[crate::text(table)],
        )?;
        let Some(crate::EngineValue::Text(sql)) = rows.first().and_then(|row| row.first()) else {
            return Ok(false);
        };
        Ok(matches!(
            crate::select::parsed(sql.as_str())?,
            turso_parser::ast::Cmd::Stmt(turso_parser::ast::Stmt::CreateVirtualTable { .. })
        ))
    }

    fn drop_table_row_count(&self, table: &str) -> Result<u64> {
        // Validate reserved names, but preserve native identifier spelling.
        // Native identifiers use ASCII case folding, unlike logical names.
        crate::canonical(table)?;
        let physical = match self.catalog(table) {
            Ok(collection) => collection.storage,
            Err(Error::NotFound(_)) => {
                self.guard_native_sql(&format!("DROP TABLE {}", crate::quote(table)))?;
                if self.virtual_drop_target(table)? {
                    return Err(Error::Unsupported(
                        "virtual table drop accounting is unavailable".into(),
                    ));
                }
                if self.run("SELECT name FROM main.sqlite_schema WHERE name=?1 COLLATE NOCASE AND type='table'", &[crate::text(table)])?.is_empty() {
                    // Let the original statement decide IF EXISTS and type errors.
                    return Ok(0);
                }
                table.to_owned()
            }
            Err(error) => return Err(error),
        };
        let rows = self.run_customer(
            &format!(
                "SELECT count(*) FROM main.{} NOT INDEXED",
                crate::quote(&physical)
            ),
            &[],
        )?;
        match rows.first().and_then(|row| row.first()) {
            Some(crate::EngineValue::Numeric(turso_core::Numeric::Integer(n))) if *n >= 0 => {
                Ok(*n as u64)
            }
            _ => Err(Error::Storage("invalid table row count".into())),
        }
    }

    fn execute_metered_schema(
        &self,
        sql: &str,
        params: &Parameters,
        result_limits: ResultLimits,
        work_limits: CreateWorkLimits,
        syntax: Result<Option<SchemaStatement>>,
    ) -> Option<MeteredCreate> {
        if matches!(syntax, Ok(None)) {
            return None;
        }
        let mut meter = Arc::new(ExecutionMeter::with_schema_read_limit(
            ExecutionLimits {
                max_rows_read: work_limits.max_rows_read,
                max_row_mutations: work_limits.max_row_mutations,
                max_vm_steps: work_limits.max_vm_steps,
            },
            work_limits.max_schema_rows_read,
        ));
        let mut preflight_work = None;
        let mut dropped_rows = 0u64;
        let mut drop_budget_exhausted = false;
        let outcome = crate::parser_stack(|| {
            let syntax = syntax?.expect("supported schema statement");
            if !matches!(syntax, SchemaStatement::Native) {
                if let Some(name) = params.keys().next() {
                    return Err(Error::Parameter(format!("unused binding {name}")));
                }
            }
            {
                let mut slot = self.work_meter.lock().unwrap_or_else(|e| e.into_inner());
                if slot.is_some() {
                    return Err(Error::Unsupported("nested metered execution".into()));
                }
                *slot = Some(meter.clone());
            }
            let _scope = Scope(self);
            self.meter_schema_changes
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.atomic(|| {
                let removing = if let SchemaStatement::DropTable(table) = &syntax {
                    self.drop_table_row_count(table)?
                } else {
                    0
                };
                if work_limits
                    .max_row_mutations
                    .is_some_and(|limit| removing > limit)
                {
                    drop_budget_exhausted = true;
                    return Err(Error::Engine(turso_core::LimboError::Interrupt));
                }
                if matches!(syntax, SchemaStatement::DropTable(_)) {
                    let before = meter.snapshot();
                    let remaining =
                        |limit: Option<u64>, used| limit.map(|n| n.saturating_sub(used));
                    meter = Arc::new(ExecutionMeter::with_schema_read_limit(
                        ExecutionLimits {
                            max_rows_read: remaining(work_limits.max_rows_read, before.rows_read),
                            max_vm_steps: remaining(work_limits.max_vm_steps, before.vm_steps),
                            // Reserve the table's own rows before native FK actions.
                            max_row_mutations: remaining(work_limits.max_row_mutations, removing),
                        },
                        remaining(work_limits.max_schema_rows_read, before.schema_rows_read),
                    ));
                    preflight_work = Some(before);
                    *self.work_meter.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(meter.clone());
                }
                let result =
                    self.execute_inner_with_result_limits(sql, params, Some(result_limits))?;
                dropped_rows = removing;
                // A future native side effect must not exceed the total budget
                // when combined with the completed bulk deletion.
                if work_limits.max_row_mutations.is_some_and(|limit| {
                    meter.snapshot().row_mutations.saturating_add(removing) > limit
                }) {
                    drop_budget_exhausted = true;
                    return Err(Error::Engine(turso_core::LimboError::Interrupt));
                }
                let mut budget =
                    crate::budget::ResultBudget::new(Some(result_limits), &result.columns)?;
                for row in &result.rows {
                    budget.row(row)?;
                }
                Ok(result)
            })
        });
        let mut work = meter.snapshot();
        if let Some(before) = preflight_work {
            work.rows_read = work.rows_read.saturating_add(before.rows_read);
            work.schema_rows_read = work
                .schema_rows_read
                .saturating_add(before.schema_rows_read);
            work.vm_steps = work.vm_steps.saturating_add(before.vm_steps);
            // Preflight only counts rows. It has no mutation events, and a budget
            // failure returns before the execution meter can replace it.
            debug_assert_eq!(before.row_mutations, 0);
        }
        Some(MeteredCreate {
            outcome,
            work: CreateWork {
                rows_read: work.rows_read,
                schema_rows_read: work.schema_rows_read,
                row_mutations: work.row_mutations.saturating_add(dropped_rows),
                vm_steps: work.vm_steps,
                read_budget_exhausted: work.read_budget_exhausted,
                schema_budget_exhausted: work.schema_budget_exhausted,
                mutation_budget_exhausted: work.mutation_budget_exhausted || drop_budget_exhausted,
                vm_budget_exhausted: work.vm_budget_exhausted,
            },
        })
    }

    pub(crate) fn meter_schema_statement<T>(
        &self,
        statement: &mut turso_core::Statement,
        execute: impl FnOnce(&mut turso_core::Statement) -> Result<T>,
    ) -> Result<T> {
        self.meter_statement_attributed(statement, true, execute)
    }

    /// Execute only logical INFO statements. Customer data is never read or
    /// mutated; catalog/PRAGMA reads and VM work are retained separately, even
    /// after failure. Transaction setup and cleanup do not inherit the budget.
    /// Result limits validate the assembled INFO value; they do not bound its
    /// intermediate decoding allocations. Serialize operations on the connection.
    pub fn info_metered(
        &self,
        sql: &str,
        params: &Parameters,
        result_limits: ResultLimits,
        work_limits: InfoWorkLimits,
    ) -> MeteredInfo {
        let meter = Arc::new(ExecutionMeter::with_limits(ExecutionLimits {
            max_rows_read: work_limits.max_catalog_rows_read,
            max_row_mutations: Some(0),
            max_vm_steps: work_limits.max_vm_steps,
        }));
        let outcome = self.with_statement_timeout(sql, |sql| {
            crate::parser_stack(|| {
                if let Some(name) = params.keys().next() {
                    return Err(Error::Parameter(format!("unused binding {name}")));
                }
                let romeql_parser::Statement::Info { scope, name } = romeql_parser::parse(sql)?
                else {
                    return Err(Error::Unsupported(
                        "metered INFO requires logical schema inspection".into(),
                    ));
                };
                {
                    let mut slot = self.work_meter.lock().unwrap_or_else(|e| e.into_inner());
                    if slot.is_some() {
                        return Err(Error::Unsupported("nested metered execution".into()));
                    }
                    *slot = Some(meter.clone());
                }
                let _scope = Scope(self);
                self.meter_catalog_reads
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                let result = self.info(&scope, name.as_deref())?;
                let mut budget =
                    crate::budget::ResultBudget::new(Some(result_limits), &result.columns)?;
                for row in &result.rows {
                    budget.row(row)?;
                }
                Ok(result)
            })
        });
        let snapshot = meter.snapshot();
        MeteredInfo {
            outcome,
            work: InfoWork {
                catalog_rows_read: snapshot.rows_read,
                vm_steps: snapshot.vm_steps,
                catalog_budget_exhausted: snapshot.read_budget_exhausted,
                vm_budget_exhausted: snapshot.vm_budget_exhausted,
            },
        }
    }

    /// Execute a checked SELECT with retained work on success and failure.
    /// Includes primary SQL/lowered statements and linked target statements;
    /// frontend catalog lookups and transaction cleanup are excluded. Uses the
    /// same accepted SELECT syntax as `profile_select_with_limits`, plus a bare
    /// record SELECT.
    ///
    /// Serialize all operations on this connection. A read limit permits at most
    /// one crossing instrumented visit, which is retained. Parsing, planning,
    /// frontend evaluation and arbitrary extension work are not bounded by VM
    /// steps. This API does not certify complete billing coverage of every index
    /// method or temporary/spilled execution path.
    pub fn select_metered(
        &self,
        sql: &str,
        params: &Parameters,
        result_limits: ResultLimits,
        work_limits: ReadWorkLimits,
    ) -> MeteredRead {
        let meter = Arc::new(ExecutionMeter::with_limits(ExecutionLimits {
            max_rows_read: work_limits.max_rows_read,
            max_row_mutations: None,
            max_vm_steps: work_limits.max_vm_steps,
        }));
        let outcome = self.with_statement_timeout(sql, |sql| {
            crate::parser_stack(|| {
                {
                    let mut slot = self.work_meter.lock().unwrap_or_else(|e| e.into_inner());
                    if slot.is_some() {
                        return Err(Error::Unsupported("nested metered execution".into()));
                    }
                    *slot = Some(meter.clone());
                }
                let _scope = Scope(self);
                match romeql_parser::parse(sql)? {
                    romeql_parser::Statement::SelectRecord(target) => {
                        self.metered_record(&target, params, result_limits)
                    }
                    _ => self
                        .profile_select_with_limits(sql, params, result_limits)
                        .map(|profile| profile.result),
                }
            })
        });
        let snapshot = meter.snapshot();
        MeteredRead {
            outcome,
            work: ReadWork {
                rows_read: snapshot.rows_read,
                vm_steps: snapshot.vm_steps,
                read_budget_exhausted: snapshot.read_budget_exhausted,
                vm_budget_exhausted: snapshot.vm_budget_exhausted,
            },
        }
    }

    /// Execute a checked data write with retained work and atomic result limits.
    /// Shares one meter across source reads and primary document mutations;
    /// catalog and rollback are excluded. Managed index SQL contributes read/VM
    /// work but does not add logical mutations.
    /// Retained mutations include rolled-back attempts. Successful execution in
    /// a caller transaction is still pending that transaction's final outcome.
    /// Virtual/search/materialized coverage
    /// require further qualification before this can serve as billing evidence.
    pub fn write_metered(
        &self,
        sql: &str,
        params: &Parameters,
        result_limits: ResultLimits,
        work_limits: WriteWorkLimits,
    ) -> MeteredWrite {
        let meter = Arc::new(ExecutionMeter::with_limits(ExecutionLimits {
            max_rows_read: work_limits.max_rows_read,
            max_row_mutations: work_limits.max_row_mutations,
            max_vm_steps: work_limits.max_vm_steps,
        }));
        let outcome = crate::parser_stack(|| {
            {
                let mut slot = self.work_meter.lock().unwrap_or_else(|e| e.into_inner());
                if slot.is_some() {
                    return Err(Error::Unsupported("nested metered execution".into()));
                }
                *slot = Some(meter.clone());
            }
            let _scope = Scope(self);
            self.write_with_result_limits(sql, params, result_limits)
        });
        let snapshot = meter.snapshot();
        MeteredWrite {
            outcome,
            work: WriteWork {
                rows_read: snapshot.rows_read,
                row_mutations: snapshot.row_mutations,
                vm_steps: snapshot.vm_steps,
                read_budget_exhausted: snapshot.read_budget_exhausted,
                mutation_budget_exhausted: snapshot.mutation_budget_exhausted,
                vm_budget_exhausted: snapshot.vm_budget_exhausted,
            },
        }
    }

    pub(crate) fn run_customer(
        &self,
        sql: &str,
        params: &[crate::EngineValue],
    ) -> Result<Vec<Vec<crate::EngineValue>>> {
        self.run_attributed(sql, params, false)
    }

    pub(crate) fn run_index_maintenance(
        &self,
        sql: &str,
        params: &[crate::EngineValue],
    ) -> Result<Vec<Vec<crate::EngineValue>>> {
        self.run_attributed(sql, params, true)
    }

    fn run_attributed(
        &self,
        sql: &str,
        params: &[crate::EngineValue],
        maintenance: bool,
    ) -> Result<Vec<Vec<crate::EngineValue>>> {
        let mut statement = self.prepare(sql)?;
        for (i, value) in params.iter().enumerate() {
            statement.bind_at(std::num::NonZeroUsize::new(i + 1).unwrap(), value.clone())?;
        }
        self.meter_statement_attributed(&mut statement, maintenance, crate::collect_rows)
    }

    fn metered_record(
        &self,
        record: &crate::Record,
        params: &Parameters,
        limits: ResultLimits,
    ) -> Result<QueryResult> {
        if let Some(name) = params.keys().next() {
            return Err(Error::Parameter(format!("unused binding {name}")));
        }
        self.atomic(|| {
            let collection = self.catalog(&record.table)?;
            let id = crate::normalized_id(record, &collection.name)?;
            let mut result = QueryResult::documents(Vec::new(), 0);
            let mut budget = crate::budget::ResultBudget::new(Some(limits), &result.columns)?;
            let mut statement = self.prepare(format!(
                "SELECT doc FROM {} WHERE id = ?1 LIMIT 1",
                crate::quote(&collection.storage)
            ))?;
            statement.bind_at(
                std::num::NonZeroUsize::new(1).unwrap(),
                crate::EngineValue::Blob(crate::Value::Record(id).encode()?),
            )?;
            let rows = self.meter_statement(&mut statement, crate::collect_rows)?;
            for row in rows {
                let output = vec![crate::Value::Object(crate::decode_document(&row[0])?)];
                budget.row(&output)?;
                result.rows.push(output);
            }
            Ok(result)
        })
    }

    pub(crate) fn meter_statement<T>(
        &self,
        statement: &mut turso_core::Statement,
        execute: impl FnOnce(&mut turso_core::Statement) -> Result<T>,
    ) -> Result<T> {
        self.meter_statement_attributed(statement, false, execute)
    }

    fn meter_statement_attributed<T>(
        &self,
        statement: &mut turso_core::Statement,
        maintenance: bool,
        execute: impl FnOnce(&mut turso_core::Statement) -> Result<T>,
    ) -> Result<T> {
        let _unnest = crate::unnest::Scope::enter();
        let meter = self
            .work_meter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(meter) = meter else {
            return execute(statement);
        };
        let meter = if maintenance {
            Arc::new(meter.without_row_mutations())
        } else {
            meter
        };
        self.engine.set_execution_meter(Some(meter))?;
        let result = execute(statement);
        // A callback error can leave a live Row. Reset before detaching, so no
        // active statement escapes the scope and cleanup cannot inherit its budget.
        let reset = if result.is_err() {
            statement.reset()
        } else {
            Ok(())
        };
        let detach = self.engine.set_execution_meter(None);
        if let Err(cleanup) = reset.and(detach) {
            return Err(Error::Rollback {
                cause: result
                    .as_ref()
                    .err()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "metered statement completed".into()),
                rollback: cleanup.to_string(),
            });
        }
        result
    }
}
