use crate::{Connection, Error, Parameters, QueryResult, Result, ResultLimits};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatementKind {
    Read,
    Info,
    Explain,
    Write,
    Schema,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestBudget {
    pub rows_read: u64,
    pub row_mutations: u64,
    pub vm_steps: u64,
    pub catalog_rows_read: u64,
    #[serde(skip)]
    max_reads: u64,
    #[serde(skip)]
    max_mutations: u64,
    #[serde(skip)]
    max_steps: u64,
}

impl RequestBudget {
    pub fn new(max_reads: u64, max_mutations: u64, max_steps: u64) -> Result<Self> {
        if max_reads > 1_000_000 || max_mutations > 100_000 || max_steps > 10_000_000 {
            return Err(Error::Validation(
                "request work limit exceeds service ceiling".into(),
            ));
        }
        Ok(Self {
            rows_read: 0,
            row_mutations: 0,
            vm_steps: 0,
            catalog_rows_read: 0,
            max_reads,
            max_mutations,
            max_steps,
        })
    }
}

impl Connection {
    pub fn execute_atomic_request(
        &self,
        body: &str,
        token: &crate::CancellationToken,
    ) -> Result<String> {
        if body.len() > 65_536 {
            return Err(Error::Limit("request exceeds 64 KiB".into()));
        }
        if self.transaction_state() != crate::TransactionState::Autocommit {
            return Err(Error::Unsupported(
                "managed atomic requests require an idle connection".into(),
            ));
        }
        let request: AtomicRequest = crate::decode_wire_json(body)?;
        let statements =
            crate::request_wire::prepare_statements(&request.statements, request.read_only)?;
        let limits = request.work_limits.unwrap_or_default();
        let mut budget = RequestBudget::new(
            limits.rows_read.unwrap_or(1_000_000),
            limits.row_mutations.unwrap_or(100_000),
            limits.vm_steps.unwrap_or(10_000_000),
        )?;
        let results_ready = std::cell::Cell::new(false);
        let outcome = self.with_cancellation(token, || {
            self.atomic(|| {
                let results = self.execute_request_statements(&statements, &mut budget, token)?;
                let response = serde_json::to_string(&serde_json::json!({
                    "results":results,"executionWork":budget
                }))?;
                results_ready.set(true);
                Ok(response)
            })
        });
        match outcome {
            Err(error) if results_ready.get() => Err(Error::Storage(format!(
                "atomic request publication was not confirmed: {error}"
            ))),
            outcome => outcome,
        }
    }

    pub fn execute_request_statements(
        &self,
        statements: &[crate::PreparedRequestStatement<'_>],
        budget: &mut RequestBudget,
        token: &crate::CancellationToken,
    ) -> Result<Vec<serde_json::Value>> {
        if self.transaction_state() != crate::TransactionState::Active {
            return Err(Error::Unsupported(
                "request batch requires an active transaction".into(),
            ));
        }
        if statements.is_empty() || statements.len() > 32 {
            return Err(Error::Validation(
                "request requires 1..32 statements".into(),
            ));
        }
        let mut results = Vec::new();
        let mut bytes = 2usize;
        for statement in statements {
            let result =
                self.execute_request_statement(statement.sql, &statement.params, budget, token)?;
            if result.affected < 0 {
                return Err(Error::Validation("negative affected count".into()));
            }
            let rows = result
                .rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(crate::Value::into_portable_value)
                        .collect::<Result<Vec<_>>>()
                })
                .collect::<Result<Vec<_>>>()?;
            let result = serde_json::json!({"columns":result.columns,"rows":rows,"affected":result.affected.to_string()});
            bytes += serde_json::to_vec(&result)?.len() + usize::from(!results.is_empty());
            if bytes > 262_144 {
                return Err(Error::Limit("aggregate result JSON exceeds 256 KiB".into()));
            }
            results.push(result);
        }
        Ok(results)
    }

    pub fn request_statement_kind(sql: &str) -> Result<RequestStatementKind> {
        crate::parser_stack(|| classify(sql))
    }

    pub fn execute_request_statement(
        &self,
        sql: &str,
        params: &Parameters,
        budget: &mut RequestBudget,
        token: &crate::CancellationToken,
    ) -> Result<QueryResult> {
        self.with_cancellation(token, || {
            let kind = Self::request_statement_kind(sql)?;
            let results = ResultLimits {
                max_rows: 1000,
                max_payload_bytes: 262_144,
            };
            let reads = budget.max_reads.saturating_sub(budget.rows_read);
            let mutations = budget.max_mutations.saturating_sub(budget.row_mutations);
            let steps = budget.max_steps.saturating_sub(budget.vm_steps);
            match kind {
                RequestStatementKind::Read => {
                    let measured = self.select_metered(
                        sql,
                        params,
                        results,
                        crate::ReadWorkLimits {
                            max_rows_read: Some(reads),
                            max_vm_steps: Some(steps),
                        },
                    );
                    budget.rows_read += measured.work.rows_read;
                    budget.vm_steps += measured.work.vm_steps;
                    request_outcome(
                        measured.outcome,
                        measured.work.read_budget_exhausted || measured.work.vm_budget_exhausted,
                        token,
                    )
                }
                RequestStatementKind::Info => {
                    let measured = self.info_metered(
                        sql,
                        params,
                        results,
                        crate::InfoWorkLimits {
                            max_catalog_rows_read: Some(
                                1_000_000u64.saturating_sub(budget.catalog_rows_read),
                            ),
                            max_vm_steps: Some(steps),
                        },
                    );
                    budget.catalog_rows_read += measured.work.catalog_rows_read;
                    budget.vm_steps += measured.work.vm_steps;
                    request_outcome(
                        measured.outcome,
                        measured.work.catalog_budget_exhausted || measured.work.vm_budget_exhausted,
                        token,
                    )
                }
                RequestStatementKind::Write => {
                    let measured = self.write_metered(
                        sql,
                        params,
                        results,
                        crate::WriteWorkLimits {
                            max_rows_read: Some(reads),
                            max_row_mutations: Some(mutations),
                            max_vm_steps: Some(steps),
                        },
                    );
                    budget.rows_read += measured.work.rows_read;
                    budget.row_mutations += measured.work.row_mutations;
                    budget.vm_steps += measured.work.vm_steps;
                    request_outcome(
                        measured.outcome,
                        measured.work.read_budget_exhausted
                            || measured.work.mutation_budget_exhausted
                            || measured.work.vm_budget_exhausted,
                        token,
                    )
                }
                RequestStatementKind::Explain | RequestStatementKind::Schema => {
                    let limits = crate::CreateWorkLimits {
                        max_rows_read: Some(reads),
                        max_schema_rows_read: Some(
                            1_000_000u64.saturating_sub(budget.catalog_rows_read),
                        ),
                        max_row_mutations: Some(mutations),
                        max_vm_steps: Some(steps),
                    };
                    let measured = if kind == RequestStatementKind::Explain {
                        self.explain_metered(sql, params, results, limits)
                    } else {
                        self.create_metered(sql, params, results, limits)
                            .or_else(|| self.schema_metered(sql, params, results, limits))
                            .or_else(|| self.ddl_metered(sql, params, results, limits))
                    }
                    .ok_or_else(|| {
                        Error::Unsupported("statement has no checked service execution path".into())
                    })?;
                    budget.rows_read += measured.work.rows_read;
                    budget.catalog_rows_read += measured.work.schema_rows_read;
                    budget.row_mutations += measured.work.row_mutations;
                    budget.vm_steps += measured.work.vm_steps;
                    request_outcome(
                        measured.outcome,
                        measured.work.read_budget_exhausted
                            || measured.work.schema_budget_exhausted
                            || measured.work.mutation_budget_exhausted
                            || measured.work.vm_budget_exhausted,
                        token,
                    )
                }
            }
        })
    }
}

fn request_outcome(
    outcome: Result<QueryResult>,
    exhausted: bool,
    token: &crate::CancellationToken,
) -> Result<QueryResult> {
    match outcome {
        Err(Error::Engine(turso_core::LimboError::Interrupt))
            if exhausted && !token.is_cancelled() =>
        {
            Err(Error::Limit("request work limit exceeded".into()))
        }
        result => result,
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AtomicRequest {
    statements: Vec<crate::RequestStatement>,
    read_only: bool,
    #[serde(default)]
    work_limits: Option<crate::RequestWorkLimits>,
}

fn classify(sql: &str) -> Result<RequestStatementKind> {
    if sql.len() > 65_536 || romeql_parser::split_script(sql)?.len() != 1 {
        return Err(Error::Validation(
            "request SQL requires exactly one bounded statement".into(),
        ));
    }
    crate::guard::internal_names(sql)?;
    let tokens = romeql_parser::tokenize(sql)?;
    for pair in tokens.windows(2) {
        if pair[1].text == "("
            && matches!(
                pair[0].kind,
                romeql_parser::Kind::Word
                    | romeql_parser::Kind::Identifier
                    | romeql_parser::Kind::String
            )
        {
            let name = pair[0].text.to_ascii_lowercase();
            if name.starts_with("pragma_")
                || matches!(
                    name.as_str(),
                    "load_extension"
                        | "readfile"
                        | "writefile"
                        | "csv"
                        | "sqlite_dbpage"
                        | "sqlite_dbdata"
                        | "sqlite_dbptr"
                )
            {
                return Err(Error::Unsupported(
                    "host and maintenance functions are not available through the service".into(),
                ));
            }
        }
    }
    let sql = crate::timeout::split(sql)?.map_or(sql, |(statement, _)| statement);
    use romeql_parser::Statement;
    use RequestStatementKind::*;
    Ok(match romeql_parser::parse(sql)? {
        Statement::SelectRecord(_) | Statement::SelectRecordProjection { .. } => Read,
        Statement::Info { .. } => Info,
        Statement::Insert { .. }
        | Statement::Upsert { .. }
        | Statement::Patch { .. }
        | Statement::PatchWhere { .. }
        | Statement::Delete { .. }
        | Statement::Relate { .. }
        | Statement::MutateDocument { .. } => Write,
        Statement::Sql(native) => {
            use turso_parser::ast::{Cmd, Stmt};
            let normalized = crate::update::normalize(&native)?;
            let native = normalized
                .as_ref()
                .map_or(native.as_str(), |value| value.sql.as_str());
            let syntax = namespace_syntax(native)?;
            let expanded =
                crate::select::expand_paths(&crate::select::expand_records(None, &syntax)?)?;
            match crate::select::parsed(&expanded)? {
                Cmd::Explain(_) | Cmd::ExplainQueryPlan { .. } => Explain,
                Cmd::Stmt(Stmt::Select(_)) => Read,
                Cmd::Stmt(Stmt::Insert { .. } | Stmt::Update(_) | Stmt::Delete { .. }) => Write,
                Cmd::Stmt(
                    Stmt::CreateTable {
                        temporary: false, ..
                    }
                    | Stmt::CreateIndex { using: None, .. }
                    | Stmt::CreateView {
                        temporary: false, ..
                    }
                    | Stmt::DropTable { .. }
                    | Stmt::DropView { .. }
                    | Stmt::DropIndex { .. }
                    | Stmt::AlterTable(_)
                    | Stmt::Reindex { name: Some(_), .. },
                ) => Schema,
                _ => {
                    return Err(Error::Unsupported(
                        "statement is not available through the service".into(),
                    ))
                }
            }
        }
        _ => Schema,
    })
}

fn namespace_syntax(sql: &str) -> Result<String> {
    let tokens = romeql_parser::tokenize(sql)?;
    let mut syntax = sql.to_owned();
    for tokens in tokens.windows(5).rev() {
        if tokens[0].kind == romeql_parser::Kind::Word
            && tokens[1].text == ":"
            && tokens[2].text == ":"
            && tokens[3].kind == romeql_parser::Kind::Word
            && tokens[4].text == "("
            && tokens[0].end == tokens[1].start
            && tokens[1].end == tokens[2].start
            && tokens[2].end == tokens[3].start
        {
            syntax.replace_range(
                tokens[0].start..tokens[3].end,
                "romedb_classification_function",
            );
        }
    }
    Ok(syntax)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_classification_preserves_field_updates_and_namespaced_functions() {
        for sql in [
            "UPDATE docs:p1 SET title=$title",
            "UPDATE docs:p1 UNSET optional",
            "UPDATE docs SET profile.city=$city WHERE id=$id",
        ] {
            assert_eq!(
                Connection::request_statement_kind(sql).unwrap(),
                RequestStatementKind::Write,
                "{sql}"
            );
        }
        for sql in [
            "SELECT app::normalize($value)",
            "SELECT search::analyze('text',$value)",
            "SELECT id FROM docs WHERE id=type::record('docs',$key)",
        ] {
            assert_eq!(
                Connection::request_statement_kind(sql).unwrap(),
                RequestStatementKind::Read,
                "{sql}"
            );
        }
        for sql in [
            "SELECT readfile('secret')",
            "SELECT 'load_extension'('secret')",
            "SELECT * FROM pragma_table_info('docs')",
            "SELECT 1; DELETE FROM docs",
        ] {
            assert!(Connection::request_statement_kind(sql).is_err(), "{sql}");
        }
    }

    #[test]
    fn checked_direct_field_updates_keep_parameters_and_removal_semantics() {
        let database = crate::Database::open(":memory:").unwrap();
        let connection = database.connect().unwrap();
        connection
            .execute("CREATE TABLE docs;", &Parameters::new())
            .unwrap();
        connection
            .execute(
                "INSERT INTO docs {id:docs:p1,title:'original',optional:null}",
                &Parameters::new(),
            )
            .unwrap();
        let token = crate::CancellationToken::new();
        let mut budget = RequestBudget::new(100, 10, 100000).unwrap();
        let params = Parameters::from([("$title".into(), crate::Value::String("updated".into()))]);
        connection
            .execute_request_statement(
                "UPDATE docs:p1 SET title=$title",
                &params,
                &mut budget,
                &token,
            )
            .unwrap();
        connection
            .execute_request_statement(
                "UPDATE docs:p1 UNSET optional",
                &Parameters::new(),
                &mut budget,
                &token,
            )
            .unwrap();
        assert_eq!(budget.row_mutations, 2);
        let record = crate::Record {
            table: "docs".into(),
            key: crate::Key::String("p1".into()),
        };
        let document = connection.get(&record).unwrap().unwrap();
        assert_eq!(document["title"], crate::Value::String("updated".into()));
        assert!(!document.contains_key("optional"));
    }
}
