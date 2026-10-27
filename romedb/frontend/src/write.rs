//! SQL-shaped writes share the document validation and index-maintenance path.
use crate::{
    select::{expand_paths, expand_records, parsed},
    Connection, Document, Error, Parameters, QueryResult, Result, Value,
};
use turso_parser::ast::*;

mod upsert;

struct WriteSource {
    with: Option<With>,
    from: Option<FromClause>,
}

pub(crate) fn is_data_write(connection: &Connection, sql: &str) -> Result<bool> {
    let normalized = crate::update::normalize(sql)?;
    let sql = normalized.as_ref().map_or(sql, |value| value.sql.as_str());
    let expanded = expand_paths(&expand_records(Some(connection), sql)?)?;
    Ok(matches!(
        parsed(&expanded),
        Ok(Cmd::Stmt(
            Stmt::Insert { .. } | Stmt::Update(_) | Stmt::Delete { .. }
        ))
    ))
}

fn unsupported(message: &str) -> Error {
    Error::Unsupported(message.into())
}
pub(crate) fn validate_returning(columns: &[ResultColumn]) -> Result<()> {
    for column in columns {
        if let ResultColumn::Expr(expr, _) = column {
            safe_value_expression(expr)?;
        }
    }
    Ok(())
}
fn id(doc: &Document) -> Result<&crate::Record> {
    match doc.get("id") {
        Some(Value::Record(id)) => Ok(id),
        _ => Err(Error::Storage("stored document has no typed id".into())),
    }
}
fn parameter<'a>(expr: &Expr, params: &'a Parameters) -> Result<Option<&'a Value>> {
    if let Expr::Variable(var) = expr {
        let name = var
            .name
            .as_ref()
            .map_or_else(|| format!("?{}", var.index), |s| s.to_string());
        return params.get(&name).map(Some).ok_or(Error::Parameter(name));
    }
    Ok(None)
}
fn aggregate_function(function: &str, arity: usize) -> bool {
    matches!(
        function,
        "avg"
            | "count"
            | "group_concat"
            | "string_agg"
            | "sum"
            | "total"
            | "json_group_array"
            | "jsonb_group_array"
            | "json_group_object"
            | "jsonb_group_object"
            | "array_agg"
            | "mode"
            | "percentile_cont"
            | "percentile_disc"
    ) || (matches!(function, "min" | "max") && arity <= 1)
}
fn safe_value_expression(expr: &Expr) -> Result<()> {
    validate_value_expression(expr, false)
}
fn validate_value_expression(expr: &Expr, select_projection: bool) -> Result<()> {
    match expr {
        Expr::Literal(_)
        | Expr::Variable(_)
        | Expr::Id(_)
        | Expr::Name(_)
        | Expr::Qualified(..)
        | Expr::DoublyQualified(..) => Ok(()),
        Expr::Unary(_, e)
        | Expr::Cast { expr: e, .. }
        | Expr::Collate(e, _)
        | Expr::IsNull(e)
        | Expr::NotNull(e) => validate_value_expression(e, select_projection),
        Expr::Binary(a, _, b) => {
            validate_value_expression(a, select_projection)?;
            validate_value_expression(b, select_projection)
        }
        Expr::Between {
            lhs, start, end, ..
        } => {
            validate_value_expression(lhs, select_projection)?;
            validate_value_expression(start, select_projection)?;
            validate_value_expression(end, select_projection)
        }
        Expr::InList { lhs, rhs, .. } => {
            validate_value_expression(lhs, select_projection)?;
            for e in rhs {
                validate_value_expression(e, select_projection)?;
            }
            Ok(())
        }
        Expr::Like {
            lhs, rhs, escape, ..
        } => {
            validate_value_expression(lhs, select_projection)?;
            validate_value_expression(rhs, select_projection)?;
            if let Some(e) = escape {
                validate_value_expression(e, select_projection)?;
            }
            Ok(())
        }
        Expr::Case {
            base,
            when_then_pairs,
            else_expr,
        } => {
            if let Some(base) = base {
                validate_value_expression(base, select_projection)?;
            }
            for (condition, value) in when_then_pairs {
                validate_value_expression(condition, select_projection)?;
                validate_value_expression(value, select_projection)?;
            }
            if let Some(value) = else_expr {
                validate_value_expression(value, select_projection)?;
            }
            Ok(())
        }
        Expr::Parenthesized(es) => {
            for e in es {
                validate_value_expression(e, select_projection)?;
            }
            Ok(())
        }
        Expr::FunctionCall {
            name,
            args,
            filter_over,
            order_by,
            within_group,
            ..
        } => {
            if (!select_projection
                && (filter_over.filter_clause.is_some() || filter_over.over_clause.is_some()))
                || !order_by.is_empty()
                || !within_group.is_empty()
            {
                return Err(unsupported("aggregate/window VALUES expressions"));
            }
            let function = name.as_str().to_ascii_lowercase();
            if matches!(
                function.as_str(),
                "__romedb_fetch" | "__romedb_relation_fetch"
            ) {
                return Err(unsupported("fetch in document write expression"));
            }
            if !select_projection && aggregate_function(&function, args.len()) {
                return Err(unsupported("aggregate document write expressions"));
            }
            if name.as_str().eq_ignore_ascii_case("load_extension") {
                return Err(unsupported("extension loading in document writes"));
            }
            for e in args {
                validate_value_expression(e, select_projection)?;
            }
            if let Some(predicate) = &filter_over.filter_clause {
                safe_candidate_assignment(predicate)?;
            }
            Ok(())
        }
        Expr::FunctionCallStar { name, filter_over }
            if select_projection && name.as_str().eq_ignore_ascii_case("count") =>
        {
            if let Some(predicate) = &filter_over.filter_clause {
                safe_candidate_assignment(predicate)?;
            }
            Ok(())
        }
        _ => Err(unsupported("this collection VALUES expression")),
    }
}
// Candidate SELECT lowering owns subquery validation and typed result handling.
// Validate only the surrounding scalar assignment here; VALUES/RETURNING keep
// their stricter validator. Never execute this validation-only expression.
fn safe_candidate_assignment(expr: &Expr) -> Result<()> {
    validate_candidate_expression(expr, false)
}
fn validate_candidate_expression(expr: &Expr, aggregates: bool) -> Result<()> {
    let mut outer = expr.clone();
    turso_core::walk_expr_mut(&mut outer, &mut |expr| {
        match expr {
            Expr::Subquery(_) | Expr::Exists(_) => {
                *expr = Expr::Literal(Literal::Null);
                return Ok(turso_core::WalkControl::SkipChildren);
            }
            Expr::InSelect { lhs, .. } => {
                *expr = Expr::Parenthesized(vec![lhs.clone()]);
            }
            _ => {}
        }
        Ok(turso_core::WalkControl::Continue)
    })?;
    validate_value_expression(&outer, aggregates)
}

// A source-free SELECT has no local columns. Nested SELECTs establish their own
// scopes and remain the responsibility of candidate SELECT lowering.
fn bind_source_free_tuple_field(
    value: &mut Expr,
    target: &QualifiedName,
) -> turso_core::Result<()> {
    bind_source_free_tuple_names(value, target, &[])
}

fn bind_source_free_tuple_names(
    value: &mut Expr,
    target: &QualifiedName,
    aliases: &[String],
) -> turso_core::Result<()> {
    turso_core::walk_expr_mut(value, &mut |expr| {
        if matches!(expr, Expr::Subquery(_) | Expr::Exists(_))
            || matches!(expr, Expr::FunctionCall { name, .. } if name.as_str() == "__romedb_path")
        {
            return Ok(turso_core::WalkControl::SkipChildren);
        }
        if let Expr::InSelect { lhs, .. } = expr {
            // Bind the outer operand without visiting the RHS scope.
            bind_source_free_tuple_names(lhs, target, aliases)?;
            return Ok(turso_core::WalkControl::SkipChildren);
        }
        if let Expr::Id(name) | Expr::Name(name) = expr {
            if aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name.as_str()))
            {
                return Ok(turso_core::WalkControl::SkipChildren);
            }
            if name.quoted()
                || (!name.as_str().eq_ignore_ascii_case("true")
                    && !name.as_str().eq_ignore_ascii_case("false"))
            {
                *expr = Expr::Qualified(
                    target.alias.as_ref().unwrap_or(&target.name).clone(),
                    name.clone(),
                );
            }
        }
        Ok(turso_core::WalkControl::Continue)
    })?;
    Ok(())
}

fn pack_tuple_select(select: &Select, width: usize) -> Result<Select> {
    let packed: Vec<_> = (0..width).map(|i| format!("tuple_column_{i}")).collect();
    let input = select.to_string();
    let mut suffix = 0_u64;
    let name = loop {
        let name = format!("romedb_tuple_values_{suffix}");
        if !input.to_ascii_lowercase().contains(&name) {
            break name;
        }
        suffix += 1;
    };
    let Cmd::Stmt(Stmt::Select(packed_select)) = parsed(&format!(
        "WITH {name}({columns}) AS ({input}) SELECT __romedb_h_array_new({columns}) FROM {name}",
        columns = packed.join(",")
    ))?
    else {
        unreachable!("generated tuple SELECT")
    };
    Ok(packed_select)
}

fn insert_clause_subqueries(statement: &Stmt) -> Result<bool> {
    let Stmt::Insert {
        body: InsertBody::Select(_, upsert),
        returning,
        ..
    } = statement
    else {
        return Ok(false);
    };
    let mut found = false;
    let mut check = |expr: &Expr| -> Result<()> {
        turso_core::walk_expr_mut(&mut expr.clone(), &mut |expr| {
            if matches!(
                expr,
                Expr::Subquery(_) | Expr::Exists(_) | Expr::InSelect { .. } | Expr::InTable { .. }
            ) {
                found = true;
                return Ok(turso_core::WalkControl::SkipChildren);
            }
            Ok(turso_core::WalkControl::Continue)
        })?;
        Ok(())
    };
    for column in returning {
        if let ResultColumn::Expr(expr, _) = column {
            check(expr)?;
        }
    }
    let mut current = upsert.as_deref();
    while let Some(clause) = current {
        if let Some(index) = &clause.index {
            for target in &index.targets {
                check(&target.expr)?;
            }
            if let Some(expr) = &index.where_clause {
                check(expr)?;
            }
        }
        if let UpsertDo::Set { sets, where_clause } = &clause.do_clause {
            for set in sets {
                check(&set.expr)?;
            }
            if let Some(expr) = where_clause {
                check(expr)?;
            }
        }
        current = clause.next.as_deref();
    }
    Ok(found)
}
impl Connection {
    /// Execute one data write and reject an oversized returned result atomically.
    /// Native SQL and collection RETURNING rows are checked during frontend
    /// collection, including native destinations with logical sources.
    /// This does not bound engine or candidate materialization memory. SQL DML
    /// and supported object writes are accepted; transaction control and DDL are not.
    pub fn write_with_result_limits(
        &self,
        sql: &str,
        params: &Parameters,
        limits: crate::ResultLimits,
    ) -> Result<QueryResult> {
        self.with_statement_timeout(sql, |sql| {
            crate::parser_stack(|| {
                use romeql_parser::Statement;
                let mut native_sql = None;
                let valid = match romeql_parser::parse(sql)? {
                    Statement::Insert { .. }
                    | Statement::MutateDocument { .. }
                    | Statement::Upsert { .. }
                    | Statement::Patch { .. }
                    | Statement::PatchWhere { .. }
                    | Statement::Relate { .. }
                    | Statement::Delete { .. } => true,
                    Statement::Sql(sql) => {
                        let normalized = crate::update::normalize(&sql)?;
                        let syntax = normalized
                            .as_ref()
                            .map_or(sql.as_str(), |value| value.sql.as_str());
                        let valid = matches!(
                            parsed(&expand_paths(&expand_records(Some(self), syntax)?)?)?,
                            Cmd::Stmt(Stmt::Insert { .. } | Stmt::Update(_) | Stmt::Delete { .. })
                        );
                        native_sql = Some(sql);
                        valid
                    }
                    _ => false,
                };
                if !valid {
                    return Err(unsupported("write result limits require one data write"));
                }
                self.atomic(|| {
                    let result = if let Some(sql) = &native_sql {
                        if let Some(result) =
                            self.collection_write_with_limits(sql, params, Some(limits))?
                        {
                            result
                        } else if let Some(result) = self.collection_select(sql, params)? {
                            result
                        } else {
                            return self
                                .native_profiled_with_limits(sql, params, Some(limits))
                                .map(|profile| profile.result);
                        }
                    } else {
                        self.execute_inner_with_result_limits(sql, params, Some(limits))?
                    };
                    let mut budget =
                        crate::budget::ResultBudget::new(Some(limits), &result.columns)?;
                    for row in &result.rows {
                        budget.row(row)?;
                    }
                    Ok(result)
                })
            })
        })
    }

    pub(crate) fn object_returning(
        &self,
        table: &str,
        projection: Option<String>,
        documents: Vec<crate::mutation_result::Snapshot>,
        params: &Parameters,
        limits: Option<crate::ResultLimits>,
    ) -> Result<QueryResult> {
        let mut snapshot_budget = self.write_buffer_budget()?;
        for document in &documents {
            document.charge(&mut snapshot_budget)?;
        }
        let Some(projection) = projection else {
            return Ok(QueryResult::command(documents.len() as i64));
        };
        crate::guard::internal_names(&projection)?;
        let Cmd::Stmt(Stmt::Select(select)) = parsed(&expand_paths(&expand_records(
            Some(self),
            &format!("SELECT {projection}"),
        )?)?)?
        else {
            return Err(unsupported("RETURNING projection"));
        };
        if select.with.is_some()
            || !select.body.compounds.is_empty()
            || !select.order_by.is_empty()
            || select.limit.is_some()
        {
            return Err(unsupported("RETURNING clauses"));
        }
        let OneSelect::Select {
            columns,
            from: None,
            where_clause: None,
            group_by: None,
            window_clause,
            distinctness: None,
        } = select.body.select
        else {
            return Err(unsupported("RETURNING projection list only"));
        };
        if !window_clause.is_empty() {
            return Err(unsupported("RETURNING windows"));
        }
        validate_returning(&columns)?;
        self.returning_rows(
            &QualifiedName::single(Name::from_string(crate::quote(table))),
            &columns,
            documents,
            params,
            limits,
        )
    }
    fn relational_insert_select(
        &self,
        sql: &str,
        statement: &Stmt,
        params: &Parameters,
        leading_with: bool,
        limits: Option<crate::ResultLimits>,
    ) -> Result<Option<QueryResult>> {
        let Stmt::Insert {
            with: None,
            body: InsertBody::Select(select, _),
            ..
        } = statement
        else {
            return Ok(None);
        };
        if matches!(select.body.select, OneSelect::Values(_)) {
            return Ok(None);
        }
        // Source names are checked by logical lowering. Guard the target and
        // remaining native clauses with the existing native SQL boundary.
        for token in crate::guard::tokens(sql)? {
            if matches!(
                token.kind,
                romeql_parser::Kind::Word
                    | romeql_parser::Kind::Identifier
                    | romeql_parser::Kind::String
            ) && token.text.to_ascii_lowercase().starts_with("__romedb_")
            {
                return Err(unsupported("managed names in INSERT SELECT"));
            }
        }
        let mut guarded = statement.clone();
        let Stmt::Insert {
            body: InsertBody::Select(source, _),
            ..
        } = &mut guarded
        else {
            unreachable!()
        };
        // Guard-only source: star leaves width unresolved during parsing. A
        // single NULL makes the parser reject multi-column INSERT targets before
        // the real source can be lowered or delegated to the engine.
        let Cmd::Stmt(Stmt::Select(empty)) = parsed("SELECT * FROM (SELECT NULL)")? else {
            unreachable!()
        };
        *source = empty;
        self.guard_native_sql(&guarded.to_string())?;
        // Moving a leading WITH into the source cannot preserve CTE scope in
        // subqueries in UPSERT/RETURNING. Reject those on managed-source routes;
        // native-only statements still fall back to their original SQL.
        let restricted_clauses = leading_with && insert_clause_subqueries(&guarded)?;
        self.native_insert_source(
            &Stmt::Select(select.clone()).to_string(),
            params,
            statement,
            restricted_clauses,
            limits,
        )
    }
    pub(crate) fn collection_write(
        &self,
        sql: &str,
        params: &Parameters,
    ) -> Result<Option<QueryResult>> {
        self.collection_write_with_limits(sql, params, None)
    }
    fn collection_write_with_limits(
        &self,
        sql: &str,
        params: &Parameters,
        limits: Option<crate::ResultLimits>,
    ) -> Result<Option<QueryResult>> {
        let normalized = crate::update::normalize(sql)?;
        let expanded = expand_paths(&expand_records(
            Some(self),
            normalized.as_ref().map_or(sql, |n| n.sql.as_str()),
        )?)?;
        let Ok(Cmd::Stmt(mut statement)) = parsed(&expanded) else {
            return Ok(None);
        };
        if !matches!(statement, Stmt::Select(_)) && expanded.contains("__romedb_graph(") {
            return Err(unsupported(
                "GRAPH sources are available in SELECT statements",
            ));
        }
        let leading_with = if let Stmt::Insert {
            with,
            body: InsertBody::Select(select, _),
            ..
        } = &mut statement
        {
            if with.is_some() && select.with.is_none() {
                select.with = with.take();
                true
            } else {
                false
            }
        } else {
            false
        };
        let table = match &statement {
            Stmt::Insert { tbl_name, .. } | Stmt::Delete { tbl_name, .. } => tbl_name,
            Stmt::Update(update) => &update.tbl_name,
            _ => return Ok(None),
        };
        if table
            .name
            .as_str()
            .to_ascii_lowercase()
            .starts_with("__romedb_")
        {
            return Err(unsupported("managed storage writes"));
        }
        match self.catalog(table.name.as_str()) {
            Ok(_) => {}
            Err(Error::NotFound(_)) => {
                return self.relational_insert_select(sql, &statement, params, leading_with, limits)
            }
            Err(e) => return Err(e),
        };
        if table
            .db_name
            .as_ref()
            .is_some_and(|n| !n.as_str().eq_ignore_ascii_case("main"))
        {
            return Err(unsupported("attached collection writes"));
        }
        crate::guard::internal_names(sql)?;
        let returning_columns = match &statement {
            Stmt::Insert { returning, .. } | Stmt::Delete { returning, .. } => returning,
            Stmt::Update(update) => &update.returning,
            _ => unreachable!(),
        };
        let capture = crate::mutation_result::needs_before(
            &returning_columns
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        )?;
        let mut retained_failure = None;
        let result = self.atomic(|| match statement {
            Stmt::Insert {
                with,
                or_conflict,
                tbl_name,
                columns,
                body,
                returning,
            } => {
                if with.is_some() || !matches!(or_conflict, None | Some(ResolveType::Abort | ResolveType::Rollback | ResolveType::Ignore | ResolveType::Fail | ResolveType::Replace)) {
                    return Err(unsupported(
                        "collection INSERT WITH/OR CONFLICT; use document UPSERT",
                    ));
                }
                validate_returning(&returning)?;
                if columns.is_empty() {
                    return Err(Error::Validation(
                        "collection SQL INSERT requires a column list".into(),
                    ));
                }
                let fields: Vec<String> =
                    columns.into_iter().map(|n| n.as_str().to_owned()).collect();
                for (i, name) in fields.iter().enumerate() {
                    if fields[..i].contains(name) {
                        return Err(Error::Validation("duplicate INSERT field".into()));
                    }
                }
                let InsertBody::Select(select, upsert) = body else {
                    return Err(unsupported("collection DEFAULT VALUES"));
                };
                let clauses = upsert.as_deref().map(|clause| {
                    if !matches!(or_conflict, None | Some(ResolveType::Abort)) {
                        return Err(unsupported("collection ON CONFLICT with a separate OR policy"));
                    }
                    self.prepare_upsert(&tbl_name, clause)
                }).transpose()?;
                let mut values_budget = self.write_buffer_budget()?;
                let values = if let (OneSelect::Values(rows), None, true) = (
                    &select.body.select,
                    &select.with,
                    select.body.compounds.is_empty(),
                ) {
                    if !select.body.compounds.is_empty()
                        || !select.order_by.is_empty()
                        || select.limit.is_some()
                    {
                        return Err(unsupported("compound or modified VALUES source"));
                    }
                    rows.iter()
                        .map(|row| {
                            if row.len() != fields.len() {
                                return Err(Error::Validation(
                                    "INSERT field/value count mismatch".into(),
                                ));
                            }
                            let values = row
                                .iter()
                                .map(|expr| self.write_value(expr, params))
                                .collect::<Result<Vec<_>>>()?;
                            values_budget.row(&values)?;
                            Ok(values)
                        })
                        .collect::<Result<Vec<_>>>()?
                } else {
                    let selected = self.insert_select(&Stmt::Select(select).to_string(), params)?;
                    if selected.columns.len() != fields.len() {
                        return Err(Error::Validation(
                            "INSERT field/value count mismatch".into(),
                        ));
                    }
                    selected.rows
                };
                // Materialize the source before mutation, including self-inserts.
                let mut documents = Vec::new();
                let mut snapshot_budget = self.write_buffer_budget()?;
                for row in values {
                    let doc: Document = fields.iter().cloned().zip(row).collect();
                    if let Some(clauses) = &clauses {
                        if let Some(snapshot) = self.insert_on_conflict(&tbl_name, doc, clauses, params, capture)? {
                            crate::retain_write_document(&mut snapshot_budget, &mut documents, snapshot)?;
                        }
                        continue;
                    }
                    let before = if capture && or_conflict == Some(ResolveType::Replace) { match doc.get("id") {
                        Some(Value::Record(record))=>self.get(&crate::normalized_id(record,&crate::canonical(tbl_name.name.as_str())?)?)?,
                        _=>None,
                    }} else {None};
                    let document = match self.insert_with_replace(tbl_name.name.as_str(), doc, or_conflict == Some(ResolveType::Replace)) {
                        Ok(document) => document,
                        Err(cause) => {
                            // insert() has already restored the failed document
                            // and all index work through its candidate savepoint.
                            if or_conflict == Some(ResolveType::Fail)
                                && matches!(cause.code(), "FDB_CONSTRAINT" | "FDB_VALIDATION")
                                && !self.engine.get_auto_commit()
                            {
                                retained_failure = Some(cause);
                                return Ok(None);
                            }
                            if or_conflict == Some(ResolveType::Ignore)
                                && matches!(cause.code(), "FDB_CONSTRAINT" | "FDB_VALIDATION")
                                && !self.engine.get_auto_commit()
                            {
                                continue;
                            }
                            if or_conflict == Some(ResolveType::Rollback)
                                && matches!(cause.code(), "FDB_CONSTRAINT" | "FDB_VALIDATION")
                                && !self.engine.get_auto_commit()
                            {
                                if let Err(rollback) = self.run("ROLLBACK", &[]) {
                                    return Err(Error::Rollback { cause: cause.to_string(), rollback: rollback.to_string() });
                                }
                            }
                            return Err(cause);
                        }
                    };
                    crate::retain_write_document(
                        &mut snapshot_budget,
                        &mut documents,
                        crate::mutation_result::Snapshot::after(document,before),
                    )?;
                }
                Ok(Some(self.returning_rows(
                    &tbl_name, &returning, documents, params, limits,
                )?))
            }
            Stmt::Update(mut update) => {
                // A leading RIGHT join with ON or no constraint is equivalent to
                // the reversed LEFT join, with qualified source names unchanged.
                if let Some(from) = &mut update.from {
                    if let Some(join) = from.joins.first_mut() {
                        if matches!(join.operator, JoinOperator::TypedJoin(Some(kind))
                            if kind.contains(JoinType::RIGHT) && !kind.intersects(JoinType::LEFT | JoinType::NATURAL))
                            && matches!(join.constraint, None | Some(JoinConstraint::On(_)))
                        {
                            std::mem::swap(&mut from.select, &mut join.table);
                            join.operator = JoinOperator::TypedJoin(Some(JoinType::LEFT | JoinType::OUTER));
                        }
                    }
                }
                if !matches!(update.or_conflict, None | Some(ResolveType::Abort | ResolveType::Rollback | ResolveType::Ignore | ResolveType::Fail | ResolveType::Replace))
                    || update.from.as_ref().is_some_and(|from| from.joins.iter().any(|join| {
                        matches!(join.constraint, Some(JoinConstraint::Using(_)))
                            || matches!(join.operator, JoinOperator::TypedJoin(Some(kind)) if kind.intersects(JoinType::RIGHT | JoinType::NATURAL))
                    }))
                    || update.indexed.is_some()
                {
                    return Err(unsupported("this collection UPDATE clause"));
                }
                validate_returning(&update.returning)?;
                let mut fields = Vec::new();
                let mut exprs = Vec::new();
                let mut widths = Vec::new();
                for set in update.sets {
                    if set.col_names.len() == 1 {
                        fields.push(set.col_names[0].as_str().to_owned());
                        exprs.push(*set.expr);
                        widths.push(1);
                    } else if let Expr::Parenthesized(values) = *set.expr {
                        if values.len() != set.col_names.len() {
                            return Err(unsupported("tuple assignment arity mismatch"));
                        }
                        fields.extend(set.col_names.iter().map(|name| name.as_str().to_owned()));
                        widths.extend(std::iter::repeat_n(1, values.len()));
                        exprs.extend(values.into_iter().map(|value| *value));
                    } else if let Expr::Subquery(mut select) = *set.expr {
                        if select.with.as_ref().is_some_and(|with| with.recursive) {
                            return Err(unsupported("this tuple SELECT assignment"));
                        }
                        if select.body.compounds.is_empty() {
                            if let OneSelect::Values(rows) = &mut select.body.select {
                                if rows.len() != 1 {
                                    return Err(unsupported("multi-row tuple VALUES assignment"));
                                }
                                for row in rows {
                                    if row.len() != set.col_names.len() {
                                        return Err(unsupported("tuple VALUES assignment arity"));
                                    }
                                    for value in row {
                                        safe_candidate_assignment(value)?;
                                        bind_source_free_tuple_field(value, &update.tbl_name)?;
                                    }
                                }
                                exprs.push(Expr::Subquery(pack_tuple_select(&select, set.col_names.len())?));
                                fields.extend(set.col_names.iter().map(|name| name.as_str().to_owned()));
                                widths.push(set.col_names.len());
                                continue;
                            }
                        }
                        if !select.body.compounds.is_empty() {
                            for arm in std::iter::once(&mut select.body.select).chain(select.body.compounds.iter_mut().map(|arm| &mut arm.select)) {
                                let OneSelect::Select { columns, from, where_clause, .. } = arm else {
                                    return Err(unsupported("tuple compound VALUES arm"));
                                };
                                if columns.len() != set.col_names.len() {
                                    return Err(unsupported("tuple compound projection arity"));
                                }
                                let aliases: Vec<_> = columns.iter().filter_map(|column| match column {
                                    ResultColumn::Expr(_, Some(alias)) if alias.is_explicit() => Some(alias.name().as_str().to_owned()),
                                    _ => None,
                                }).collect();
                                for column in columns {
                                    let ResultColumn::Expr(value, _) = column else { return Err(unsupported("tuple compound projection")); };
                                    validate_candidate_expression(value, true)?;
                                    if from.is_none() { bind_source_free_tuple_field(value, &update.tbl_name)?; }
                                }
                                if from.is_none() {
                                    if let Some(predicate) = where_clause { bind_source_free_tuple_names(predicate, &update.tbl_name, &aliases)?; }
                                }
                            }
                            exprs.push(Expr::Subquery(pack_tuple_select(&select, set.col_names.len())?));
                            fields.extend(set.col_names.iter().map(|name| name.as_str().to_owned()));
                            widths.push(set.col_names.len());
                            continue;
                        }
                        fn ordinal(value: &Expr) -> bool {
                            match value {
                                Expr::Literal(Literal::Numeric(n)) => n.parse::<i64>().is_ok(),
                                Expr::Parenthesized(values) if values.len() == 1 => {
                                    ordinal(&values[0])
                                }
                                Expr::Unary(_, value) | Expr::Collate(value, _) => ordinal(value),
                                _ => false,
                            }
                        }
                        let positional_order = select.order_by.iter().any(|sort| ordinal(&sort.expr));
                        let OneSelect::Select {
                            columns,
                            from,
                            group_by,
                            window_clause,
                            where_clause,
                            distinctness,
                            ..
                        } = &mut select.body.select
                        else {
                            return Err(unsupported("this tuple SELECT assignment"));
                        };
                        if columns.len() != set.col_names.len() {
                            return Err(unsupported("tuple SELECT assignment shape or arity"));
                        }
                        let explicit_aliases = columns.iter().any(|column| {
                            matches!(column, ResultColumn::Expr(_, Some(alias)) if alias.is_explicit())
                        });
                        let mut complex_projection = !window_clause.is_empty();
                        for column in columns.iter() {
                            if let ResultColumn::Expr(value, _) = column {
                                validate_candidate_expression(value, true)?;
                                turso_core::walk_expr_mut(&mut value.clone(), &mut |expr| {
                                    complex_projection |= match expr {
                                        Expr::FunctionCall { name, args, filter_over, .. } => filter_over.over_clause.is_some() || aggregate_function(&name.as_str().to_ascii_lowercase(), args.len()),
                                        Expr::FunctionCallStar { .. } | Expr::Subquery(_) | Expr::Exists(_) | Expr::InSelect { .. } => true,
                                        _ => false,
                                    };
                                    Ok(turso_core::WalkControl::Continue)
                                })?;
                            }
                        }
                        if positional_order || explicit_aliases || distinctness.is_some() || group_by.is_some() || complex_projection {
                            let aliases: Vec<_> = columns.iter().filter_map(|column| {
                                match column {
                                    ResultColumn::Expr(_, Some(alias)) if alias.is_explicit() => Some(alias.name().as_str().to_owned()),
                                    _ => None,
                                }
                            }).collect();
                            let mut packed = Vec::new();
                            for (position, column) in columns.iter_mut().enumerate() {
                                let ResultColumn::Expr(value, _) = column else {
                                    return Err(unsupported("tuple SELECT projection"));
                                };
                                validate_candidate_expression(value, true)?;
                                if from.is_none() {
                                    bind_source_free_tuple_field(value, &update.tbl_name)?;
                                }
                                packed.push(format!("tuple_column_{position}"));
                            }
                            if from.is_none() {
                                if let Some(predicate) = where_clause {
                                    bind_source_free_tuple_names(predicate, &update.tbl_name, &aliases)?;
                                }
                                for sort in &mut select.order_by {
                                    bind_source_free_tuple_names(&mut sort.expr, &update.tbl_name, &aliases)?;
                                }
                            }
                            // Preserve the original alias scope and evaluate each
                            // selected row before packing its tuple result.
                            let packed_select = pack_tuple_select(&select, packed.len())?;
                            fields.extend(set.col_names.iter().map(|name| name.as_str().to_owned()));
                            widths.push(set.col_names.len());
                            exprs.push(Expr::Subquery(packed_select));
                            continue;
                        }
                        if from.is_none() {
                            for sort in &mut select.order_by {
                                bind_source_free_tuple_field(&mut sort.expr, &update.tbl_name)?;
                            }
                            if let Some(predicate) = where_clause {
                                bind_source_free_tuple_field(predicate, &update.tbl_name)?;
                            }
                        }
                        let mut args = Vec::new();
                        for column in std::mem::take(columns) {
                            let ResultColumn::Expr(value, None | Some(As::ImplicitColumnName(_))) =
                                column
                            else {
                                return Err(unsupported("tuple SELECT projection"));
                            };
                            safe_value_expression(&value)?;
                            let mut value = value;
                            if from.is_none() {
                                bind_source_free_tuple_field(&mut value, &update.tbl_name)?;
                            }
                            args.push(value);
                        }
                        columns.push(ResultColumn::Expr(
                            Box::new(Expr::FunctionCall {
                                name: Name::exact("__romedb_h_array_new".into()),
                                distinctness: None,
                                args,
                                order_by: Vec::new(),
                                within_group: Vec::new(),
                                filter_over: FunctionTail {
                                    filter_clause: None,
                                    over_clause: None,
                                },
                            }),
                            None,
                        ));
                        fields.extend(set.col_names.iter().map(|name| name.as_str().to_owned()));
                        widths.push(set.col_names.len());
                        exprs.push(Expr::Subquery(select));
                    } else {
                        return Err(unsupported(
                            "collection tuple assignment requires explicit values",
                        ));
                    }
                }
                let paths = normalized.as_ref().map_or_else(
                    || fields.iter().map(|f| vec![f.clone()]).collect(),
                    |n| n.paths.clone(),
                );
                crate::update::validate_targets(&paths)?;
                let unset = normalized.as_ref().is_some_and(|n| n.unset);
                if unset
                    && paths.iter().any(|path| matches!(path[0].as_str(), "source" | "target"))
                    && self.catalog(update.tbl_name.name.as_str())?.edge.is_some()
                {
                    return Err(Error::Validation(
                        "edge source and target are system fields and cannot be removed".into(),
                    ));
                }
                let rows = self.write_candidates(
                    &update.tbl_name,
                    WriteSource { with: update.with, from: update.from },
                    update.where_clause,
                    &exprs,
                    params,
                )?;
                let mut documents = Vec::new();
                let mut snapshot_budget = self.write_buffer_budget()?;
                let mut output_budget = self.write_buffer_budget()?;
                for row in rows {
                    let mutate = || -> Result<()> {
                    let mut values = row.into_iter();
                    let Some(Value::Object(mut document)) = values.next() else {
                        return Err(Error::Storage("invalid update candidate".into()));
                    };
                    if update.or_conflict == Some(ResolveType::Replace) {
                        let record = id(&document)?;
                        let collection = self.catalog(&record.table)?;
                        if self.get_in(&collection, record)?.is_none() {
                            return Ok(());
                        }
                    }
                    // All assignments were evaluated before any mutation. Move
                    // their owned values and the snapshot instead of cloning them.
                    let before = capture.then(||document.clone());
                    let mut targets = paths.iter();
                    for (value, width) in values.zip(&widths) {
                        let assigned = if *width == 1 {
                            vec![value]
                        } else {
                            match value {
                                Value::Array(values) if values.len() == *width => values,
                                Value::Null => vec![Value::Null; *width],
                                _ => {
                                    return Err(Error::Storage(
                                        "invalid tuple assignment result".into(),
                                    ))
                                }
                            }
                        };
                        for value in assigned {
                            let path = targets.next().ok_or_else(|| {
                                Error::Storage("missing assignment target".into())
                            })?;
                            crate::update::apply(
                                &mut document,
                                path,
                                if unset { None } else { Some(value) },
                            )?;
                        }
                    }
                    // Reject the next snapshot before validation and storage work.
                    // Earlier rows still belong to the operation savepoint.
                    // validate_targets forbids changing the record identity.
                    snapshot_budget.document(&document)?;
                    let collection = self.catalog(&id(&document)?.table)?;
                    if update.or_conflict == Some(ResolveType::Replace) {
                        self.replace_conflicting_document(&collection, &mut document)?;
                    } else {
                        self.replace_document(&collection, &mut document)?;
                    }
                    let snapshot=crate::mutation_result::Snapshot::after(document,before);
                    snapshot.charge(&mut output_budget)?;
                    documents.push(snapshot);
                    Ok(())
                    };
                    // A skipped candidate must restore both its document and every
                    // managed index, including indexes changed before a conflict.
                    let mutation = if matches!(update.or_conflict, Some(ResolveType::Ignore | ResolveType::Fail)) {
                        self.atomic(mutate)
                    } else {
                        mutate()
                    };
                    if let Err(cause) = mutation {
                        if update.or_conflict == Some(ResolveType::Fail)
                            && matches!(cause.code(), "FDB_CONSTRAINT" | "FDB_VALIDATION")
                            && !self.engine.get_auto_commit()
                        {
                            // Release the statement frame before returning the error,
                            // retaining the successful prefix but no partial failed row.
                            retained_failure = Some(cause);
                            return Ok(None);
                        }
                        if update.or_conflict == Some(ResolveType::Ignore)
                            && matches!(cause.code(), "FDB_CONSTRAINT" | "FDB_VALIDATION")
                            && !self.engine.get_auto_commit()
                        {
                            continue;
                        }

                        if update.or_conflict == Some(ResolveType::Rollback)
                            && matches!(cause.code(), "FDB_CONSTRAINT" | "FDB_VALIDATION")
                            && !self.engine.get_auto_commit()
                        {
                            if let Err(rollback) = self.run("ROLLBACK", &[]) {
                                return Err(Error::Rollback { cause: cause.to_string(), rollback: rollback.to_string() });
                            }
                        }
                        return Err(cause);
                    }
                }
                Ok(Some(self.returning_rows(
                    &update.tbl_name,
                    &update.returning,
                    documents,
                    params,
                    limits,
                )?))
            }
            Stmt::Delete {
                with,
                tbl_name,
                indexed,
                where_clause,
                returning,
            } => {
                if indexed.is_some() {
                    return Err(unsupported("this collection DELETE clause"));
                }
                validate_returning(&returning)?;
                let rows = self.write_candidates(&tbl_name, WriteSource { with, from: None }, where_clause, &[], params)?;
                let mut documents = Vec::new();
                let mut snapshot_budget = self.write_buffer_budget()?;
                for row in rows {
                    let Some(Value::Object(doc)) = row.into_iter().next() else {
                        return Err(Error::Storage("invalid delete candidate".into()));
                    };
                    snapshot_budget.document(&doc)?;
                    let collection = self.catalog(&id(&doc)?.table)?;
                    self.delete_document(&collection, &doc)?;
                    documents.push(crate::mutation_result::Snapshot::deleted(doc));
                }
                Ok(Some(self.returning_rows(
                    &tbl_name, &returning, documents, params, limits,
                )?))
            }
            _ => unreachable!("write statement dispatched above"),
        })?;
        match retained_failure {
            Some(cause) => Err(cause),
            None => Ok(result),
        }
    }
    fn write_value(&self, expr: &Expr, params: &Parameters) -> Result<Value> {
        if let Some(value) = parameter(expr, params)? {
            value.validate()?;
            return Ok(value.clone());
        }
        safe_value_expression(expr)?;
        let rows = self
            .collection_select_internal(&format!("SELECT {expr}"), params, true)?
            .ok_or_else(|| Error::Storage("VALUES expression lowering failed".into()))?
            .rows;
        rows.into_iter()
            .next()
            .and_then(|row| row.into_iter().next())
            .ok_or_else(|| Error::Storage("VALUES expression returned no value".into()))
    }

    fn write_candidates(
        &self,
        table: &QualifiedName,
        source: WriteSource,
        predicate: Option<Box<Expr>>,
        assignments: &[Expr],
        params: &Parameters,
    ) -> Result<Vec<Vec<Value>>> {
        for expr in assignments {
            safe_candidate_assignment(expr)?;
        }
        if let Some(from) = &source.from {
            // The UPDATE target is not in scope while source joins are prepared.
            let Cmd::Stmt(Stmt::Select(mut select)) = parsed("SELECT 1")? else {
                unreachable!()
            };
            select.with = source.with.clone();
            let OneSelect::Select {
                from: source_from, ..
            } = &mut select.body.select
            else {
                unreachable!()
            };
            let mut validation_from = from.clone();
            // Native table-function arguments may reference the UPDATE target.
            // Validate their expressions later in the full candidate scope;
            // this source-only preparation still checks tables and JOIN ON.
            for table in std::iter::once(&mut validation_from.select)
                .chain(validation_from.joins.iter_mut().map(|join| &mut join.table))
            {
                if let SelectTable::TableCall(_, args, _) = table.as_mut() {
                    for arg in args {
                        **arg = Expr::Literal(Literal::Null);
                    }
                }
            }
            *source_from = Some(validation_from);
            self.validate_write_source(&select.to_string(), params)?;
        }
        let joined = source.from.is_some();
        let mut columns = vec![ResultColumn::TableStar(
            table.alias.as_ref().unwrap_or(&table.name).clone(),
        )];
        for (i, expr) in assignments.iter().enumerate() {
            let mut expr = if parameter(expr, params)?.is_some() {
                Expr::Literal(Literal::Null)
            } else {
                expr.clone()
            };
            // Native UPDATE evaluates assignment aggregates per target row.
            // The candidate SELECT needs its own scalar scope so 0.8.1's
            // outer-aggregate lifting cannot collapse the candidate rows.
            let Cmd::Stmt(Stmt::Select(scalar_scope)) = parsed("SELECT NULL")? else {
                unreachable!()
            };
            turso_core::walk_expr_mut(&mut expr, &mut |value| {
                if matches!(value, Expr::Subquery(_) | Expr::Exists(_)) {
                    let mut scalar = scalar_scope.clone();
                    let OneSelect::Select { columns, .. } = &mut scalar.body.select else {
                        unreachable!()
                    };
                    *columns = vec![ResultColumn::Expr(Box::new(value.clone()), None)];
                    *value = Expr::Subquery(scalar);
                    return Ok(turso_core::WalkControl::SkipChildren);
                }
                Ok(turso_core::WalkControl::Continue)
            })?;
            columns.push(ResultColumn::Expr(
                Box::new(expr),
                Some(As::As(Name::exact(format!("__romedb_set_{i}")))),
            ));
        }
        fn bind_expression(
            value: &mut Expr,
            table: &QualifiedName,
            exposed: &Name,
        ) -> turso_core::Result<()> {
            turso_core::walk_expr_mut(value, &mut |expr| {
                match expr {
                    Expr::Subquery(inner) | Expr::Exists(inner) => {
                        bind_target(inner, table, exposed)?;
                        return Ok(turso_core::WalkControl::SkipChildren);
                    }
                    Expr::InSelect { lhs, rhs, .. } => {
                        bind_expression(lhs, table, exposed)?;
                        bind_target(rhs, table, exposed)?;
                        return Ok(turso_core::WalkControl::SkipChildren);
                    }
                    _ => {}
                }
                Ok(turso_core::WalkControl::Continue)
            })
            .map(|_| ())
        }
        fn bind_target(
            select: &mut Select,
            table: &QualifiedName,
            exposed: &Name,
        ) -> turso_core::Result<()> {
            if let Some(with) = &mut select.with {
                // A nested declaration establishes a new local binding.
                if with.recursive
                    || with
                        .ctes
                        .iter()
                        .any(|cte| cte.tbl_name.as_str().eq_ignore_ascii_case(exposed.as_str()))
                {
                    return Ok(());
                }
                for cte in &mut with.ctes {
                    bind_target(&mut cte.select, table, exposed)?;
                }
            }
            for body in std::iter::once(&mut select.body.select)
                .chain(select.body.compounds.iter_mut().map(|arm| &mut arm.select))
            {
                match body {
                    OneSelect::Select {
                        columns,
                        from,
                        where_clause,
                        group_by,
                        window_clause,
                        ..
                    } => {
                        for column in columns {
                            if let ResultColumn::Expr(value, _) = column {
                                bind_expression(value, table, exposed)?;
                            }
                        }
                        if let Some(from) = from {
                            for source in std::iter::once(&mut from.select)
                                .chain(from.joins.iter_mut().map(|join| &mut join.table))
                            {
                                match source.as_mut() {
                                    SelectTable::Select(inner, _) => {
                                        bind_target(inner, table, exposed)?
                                    }
                                    SelectTable::Table(name, alias, _)
                                        if name.db_name.is_none()
                                            && name
                                                .name
                                                .as_str()
                                                .eq_ignore_ascii_case(exposed.as_str()) =>
                                    {
                                        name.db_name = table.db_name.clone();
                                        name.name = table.name.clone();
                                        if alias.is_none() {
                                            *alias = Some(As::As(exposed.clone()));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            for join in &mut from.joins {
                                if let Some(JoinConstraint::On(value)) = &mut join.constraint {
                                    bind_expression(value, table, exposed)?;
                                }
                            }
                        }
                        if let Some(value) = where_clause {
                            bind_expression(value, table, exposed)?;
                        }
                        if let Some(group) = group_by {
                            for value in &mut group.exprs {
                                bind_expression(value, table, exposed)?;
                            }
                            if let Some(value) = &mut group.having {
                                bind_expression(value, table, exposed)?;
                            }
                        }
                        for definition in window_clause {
                            for value in &mut definition.window.partition_by {
                                bind_expression(value, table, exposed)?;
                            }
                            for sorted in &mut definition.window.order_by {
                                bind_expression(&mut sorted.expr, table, exposed)?;
                            }
                        }
                    }
                    OneSelect::Values(rows) => {
                        for row in rows {
                            for value in row {
                                bind_expression(value, table, exposed)?;
                            }
                        }
                    }
                }
            }
            for sorted in &mut select.order_by {
                bind_expression(&mut sorted.expr, table, exposed)?;
            }
            if let Some(limit) = &mut select.limit {
                bind_expression(&mut limit.expr, table, exposed)?;
                if let Some(offset) = &mut limit.offset {
                    bind_expression(offset, table, exposed)?;
                }
            }
            Ok(())
        }
        let mut with = source.with;
        // Turso 0.8.1 resolves source CTEs before binding UPDATE/DELETE
        // targets, including non-joined writes. Keep their lexical bindings.
        fn flatten(
            connection: &Connection,
            select: &mut Select,
            lifted: &mut Vec<CommonTableExpr>,
        ) -> Result<()> {
            if select.with.as_ref().is_none_or(|with| with.recursive) {
                return Ok(());
            }
            let mut nested = select.with.take().unwrap();
            let mut names = std::collections::BTreeSet::new();
            if nested
                .ctes
                .iter()
                .any(|cte| !names.insert(cte.tbl_name.as_str().to_ascii_lowercase()))
            {
                select.with = Some(nested);
                return Ok(());
            }
            let mut replacements = Vec::new();
            for cte in &mut nested.ctes {
                let old = cte.tbl_name.clone();
                let id = connection
                    .next_subquery_id
                    .fetch_update(
                        std::sync::atomic::Ordering::Relaxed,
                        std::sync::atomic::Ordering::Relaxed,
                        |id| id.checked_add(1),
                    )
                    .map_err(|_| Error::Limit("internal subquery identifiers exhausted".into()))?;
                cte.tbl_name = Name::exact(format!("__romedb_write_cte_{id}"));
                replacements.push((
                    old,
                    QualifiedName {
                        name: cte.tbl_name.clone(),
                        db_name: None,
                        alias: None,
                    },
                ));
            }
            for (old, renamed) in &replacements {
                bind_target(select, renamed, old)?;
                for cte in &mut nested.ctes {
                    bind_target(&mut cte.select, renamed, old)?;
                }
            }
            for mut cte in nested.ctes {
                flatten(connection, &mut cte.select, lifted)?;
                lifted.push(cte);
            }
            Ok(())
        }
        if let Some(with) = with.as_mut().filter(|with| !with.recursive) {
            let mut lifted = Vec::new();
            for mut cte in std::mem::take(&mut with.ctes) {
                flatten(self, &mut cte.select, &mut lifted)?;
                lifted.push(cte);
            }
            with.ctes = lifted;
        }
        let mut target = table.clone();
        // UPDATE/DELETE target names refer to actual tables, even if a CTE
        // has the same name. Preserve that rule in the candidate SELECT.
        if with.is_some() && target.db_name.is_none() {
            target.db_name = Some(Name::exact("main".into()));
        }
        let alias = target.alias.take().map(As::As);
        let select = Select {
            with,
            body: SelectBody {
                select: OneSelect::Select {
                    distinctness: None,
                    columns,
                    from: Some(FromClause {
                        select: Box::new(SelectTable::Table(target, alias, None)),
                        joins: source
                            .from
                            .into_iter()
                            .flat_map(|from| {
                                std::iter::once(JoinedSelectTable {
                                    operator: JoinOperator::Comma,
                                    table: from.select,
                                    constraint: None,
                                })
                                .chain(from.joins)
                            })
                            .collect(),
                    }),
                    where_clause: predicate,
                    group_by: None,
                    window_clause: Vec::new(),
                },
                compounds: Vec::new(),
            },
            order_by: Vec::new(),
            limit: None,
        };
        let mut result = self
            .write_candidate_select(&Stmt::Select(select).to_string(), params, true)?
            .ok_or_else(|| unsupported("this collection write candidate query"))?;
        let mut budget =
            crate::budget::ResultBudget::new(self.write_buffer_limits, &result.columns)?;
        for row in &mut result.rows {
            for (i, expr) in assignments.iter().enumerate() {
                if let Some(value) = parameter(expr, params)? {
                    value.validate()?;
                    row[i + 1] = value.clone();
                }
            }
            budget.row(row)?;
        }
        if joined {
            return self.coalesce_update_candidates(result.rows);
        }
        Ok(result.rows)
    }
    fn coalesce_update_candidates(&self, candidates: Vec<Vec<Value>>) -> Result<Vec<Vec<Value>>> {
        let mut positions = std::collections::BTreeMap::new();
        let mut key_budget = self.write_buffer_budget()?;
        let mut rows = Vec::new();
        for row in candidates {
            if self.engine.should_interrupt_for_progress(0, 1) {
                return Err(Error::Engine(turso_core::LimboError::Interrupt));
            }
            let Some(Value::Object(document)) = row.first() else {
                return Err(Error::Storage("invalid joined update candidate".into()));
            };
            let Some(id @ Value::Record(_)) = document.get("id") else {
                return Err(Error::Storage("missing joined update target ID".into()));
            };
            let key = id.encode_with_limit(
                self.write_buffer_limits
                    .map(|limits| limits.max_payload_bytes),
            )?;
            if let Some(position) = positions.get(&key) {
                rows[*position] = row;
            } else {
                // Charge each retained encoded identity once, before map insertion.
                // Duplicate matches replace a row without growing this map.
                let encoded = Value::Binary(key);
                key_budget.row(std::slice::from_ref(&encoded))?;
                let Value::Binary(key) = encoded else {
                    unreachable!()
                };
                positions.insert(key, rows.len());
                rows.push(row);
            }
        }
        Ok(rows)
    }
}

#[cfg(test)]
mod candidate_cancellation_tests {
    use super::*;
    #[test]
    fn joined_source_preparation_does_not_evaluate_scalar_callbacks() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use turso_ext::{scalar, ResultCode, Value as ExtValue};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        #[scalar(name = "write_scope_tick")]
        fn write_scope_tick(args: &[ExtValue]) -> ExtValue {
            CALLS.fetch_add(1, Ordering::SeqCst);
            ExtValue::from_integer(args[0].to_integer().unwrap())
        }
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        unsafe {
            let api = c.engine._build_turso_ext();
            let code = (api.register_scalar_function)(
                api.ctx,
                c"write_scope_tick".as_ptr(),
                1,
                false,
                0,
                write_scope_tick,
                None,
                None,
            );
            c.engine._free_extension_ctx(api);
            assert_eq!(code, ResultCode::OK);
        }
        let p = Parameters::new();
        for sql in [
            "CREATE TABLE native(n INTEGER,v INTEGER)",
            "INSERT INTO native VALUES(1,0)",
            "CREATE TABLE docs",
            "INSERT INTO docs(n,v) VALUES(1,0)",
            "CREATE TABLE source(k INTEGER,v INTEGER)",
            "INSERT INTO source VALUES(1,10),(1,11)",
        ] {
            c.execute(sql, &p).unwrap();
        }
        for hint in ["", "MATERIALIZED", "NOT MATERIALIZED"] {
            let with =
                format!("WITH chosen AS {hint} (SELECT k,write_scope_tick(v) AS v FROM source)");
            CALLS.store(0, Ordering::SeqCst);
            c.validate_write_source(&format!("{with} SELECT 1 FROM chosen"), &p)
                .unwrap();
            assert_eq!(CALLS.load(Ordering::SeqCst), 0);
            let mut expected = None;
            for target in ["native", "docs"] {
                c.execute("BEGIN", &p).unwrap();
                CALLS.store(0, Ordering::SeqCst);
                let result=c.execute(&format!("{with} UPDATE {target} SET v=s.v FROM chosen s WHERE {target}.n=s.k RETURNING v"),&p).unwrap();
                let observed = (result.rows, CALLS.load(Ordering::SeqCst));
                assert!(observed.1 > 0);
                if let Some(expected) = &expected {
                    assert_eq!(&observed, expected, "{hint}");
                } else {
                    expected = Some(observed);
                }
                c.execute("ROLLBACK", &p).unwrap();
            }
        }
        for argument in [
            "json_array(write_scope_tick(t.n))",
            "(SELECT json_array(write_scope_tick(t.n)))",
        ] {
            let mut expected = None;
            for target in ["native", "docs"] {
                c.execute("BEGIN", &p).unwrap();
                CALLS.store(0, Ordering::SeqCst);
                let result=c.execute(&format!("UPDATE {target} AS t SET v=s.value FROM json_each({argument}) s RETURNING v"),&p).unwrap();
                let observed = (result.rows, CALLS.load(Ordering::SeqCst));
                assert!(observed.1 > 0);
                if let Some(expected) = &expected {
                    assert_eq!(&observed, expected, "{argument}");
                } else {
                    expected = Some(observed);
                }
                c.execute("ROLLBACK", &p).unwrap();
            }
        }
    }

    #[test]
    fn duplicate_identity_budget_counts_retained_encoded_keys() {
        let db = crate::Database::open(":memory:").unwrap();
        let row = |key, value| {
            vec![
                Value::Object(Document::from([(
                    "id".into(),
                    Value::Record(crate::Record {
                        table: "docs".into(),
                        key,
                    }),
                )])),
                Value::Integer(value),
            ]
        };
        let first = row(crate::Key::String("\"\\é".into()), 1);
        let last = row(crate::Key::String("\"\\é".into()), 2);
        let Value::Object(doc) = &first[0] else {
            unreachable!()
        };
        let bytes = doc["id"].encode().unwrap().len();
        let c = db
            .connect()
            .unwrap()
            .with_write_buffer_limits(crate::ResultLimits {
                max_rows: 1,
                max_payload_bytes: bytes,
            });
        assert_eq!(
            c.coalesce_update_candidates(vec![first.clone(), last.clone()])
                .unwrap(),
            vec![last]
        );
        let c = c.with_write_buffer_limits(crate::ResultLimits {
            max_rows: 1,
            max_payload_bytes: bytes - 1,
        });
        assert_eq!(
            c.coalesce_update_candidates(vec![first])
                .unwrap_err()
                .code(),
            "FDB_LIMIT"
        );
        let c = c.with_write_buffer_limits(crate::ResultLimits {
            max_rows: 1,
            max_payload_bytes: 10000,
        });
        assert_eq!(
            c.coalesce_update_candidates(vec![
                row(crate::Key::Integer(1), 1),
                row(crate::Key::String("1".into()), 2)
            ])
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        let c = c.with_write_buffer_limits(crate::ResultLimits {
            max_rows: 2,
            max_payload_bytes: 10000,
        });
        let rows = vec![
            row(crate::Key::Integer(1), 1),
            row(crate::Key::String("1".into()), 2),
        ];
        assert_eq!(c.coalesce_update_candidates(rows.clone()).unwrap(), rows);
    }

    #[test]
    fn cancelled_duplicate_resolution_discards_candidates_and_allows_retry() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let token = crate::CancellationToken::new();
        let row = vec![
            Value::Object(Document::from([(
                "id".into(),
                Value::Record(crate::Record {
                    table: "docs".into(),
                    key: crate::Key::Integer(1),
                }),
            )])),
            Value::Integer(7),
        ];
        let error = c
            .with_cancellation(&token, || {
                token.cancel();
                c.coalesce_update_candidates(vec![row.clone(), row.clone()])
            })
            .unwrap_err();
        assert_eq!(error.code(), "FDB_CANCELLED");
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        c.engine.set_progress_handler(
            1,
            Some(Box::new(move || {
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1
            })),
        );
        let interrupted = c.coalesce_update_candidates(vec![row.clone(), row.clone(), row.clone()]);
        c.engine.set_progress_handler(0, None);
        assert_eq!(interrupted.unwrap_err().code(), "FDB_CANCELLED");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            c.coalesce_update_candidates(vec![row.clone(), row.clone()])
                .unwrap(),
            vec![row]
        );
        assert_eq!(
            c.execute("SELECT 1", &Parameters::new()).unwrap().rows,
            vec![vec![Value::Integer(1)]]
        );
    }
}

#[cfg(test)]
mod insert_source_oracle_tests {
    use crate::Database;

    #[test]
    fn raw_engine_two_column_insert_select_probe() {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.run("CREATE TABLE items(n INTEGER UNIQUE,v INTEGER)", &[])
            .unwrap();
        c.run("INSERT INTO items VALUES(1,0)", &[]).unwrap();
        let error = c
            .run(
                "INSERT OR ABORT INTO items(n,v) SELECT 2,0 UNION ALL SELECT 1,0",
                &[],
            )
            .unwrap_err();
        assert_eq!(error.code(), "FDB_CONSTRAINT", "{error:?}");
        let error = c
            .execute(
                "INSERT OR ABORT INTO items(n,v) SELECT 2,0 UNION ALL SELECT 1,0",
                &crate::Parameters::new(),
            )
            .unwrap_err();
        assert_eq!(error.code(), "FDB_CONSTRAINT", "{error:?}");
    }
    #[test]
    fn native_insert_source_width_and_defaults_match_raw_engine() {
        for sql in [
            "INSERT INTO items(a,b,c) SELECT 1,2,3 RETURNING a,b,c",
            "INSERT OR ABORT INTO items(a,c) SELECT 1,3 RETURNING a,b,c",
            "INSERT INTO items SELECT * FROM source RETURNING a,b,c",
            "WITH chosen AS (SELECT * FROM source) INSERT INTO items SELECT * FROM chosen RETURNING a,b,c",
            "INSERT INTO items(a,b,c) SELECT 1,2",
            "INSERT INTO items(a,b) SELECT 1,2,3",
        ] {
            let mut baseline = None;
            for frontend in [false, true] {
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                c.run("CREATE TABLE items(a INTEGER,b INTEGER DEFAULT 9,c INTEGER)", &[]).unwrap();
                c.run("CREATE TABLE source(a INTEGER,b INTEGER,c INTEGER)", &[]).unwrap();
                c.run("INSERT INTO source VALUES(1,2,3),(4,5,6)", &[]).unwrap();
                let result = if frontend {
                    c.execute(sql, &crate::Parameters::new()).map(|result| result.rows)
                } else {
                    c.run(sql, &[]).map(|rows| rows.into_iter().map(|row| row.into_iter().map(crate::from_engine).collect()).collect())
                };
                let result = result.map_err(|error| error.to_string());
                let stored = c.run("SELECT a,b,c FROM items ORDER BY a", &[]).unwrap();
                let observed = (result, stored);
                if frontend {
                    assert_eq!(Some(&observed), baseline.as_ref(), "{sql}");
                } else {
                    baseline = Some(observed);
                }
            }
        }
    }
    #[test]
    fn native_insert_select_conflict_dispositions_match_raw_engine() {
        for policy in ["ABORT", "ROLLBACK", "FAIL", "IGNORE", "REPLACE"] {
            for active in [false, true] {
                let mut baseline = None;
                for mode in [0, 1, 2] {
                    let db = Database::open(":memory:").unwrap();
                    let c = db.connect().unwrap();
                    for sql in [
                        "CREATE TABLE items(n INTEGER UNIQUE,v INTEGER)",
                        "INSERT INTO items VALUES(1,0)",
                    ] {
                        c.run(sql, &[]).unwrap();
                    }
                    if mode == 2 {
                        c.execute("CREATE TABLE source", &crate::Parameters::new())
                            .unwrap();
                        c.execute(
                            "INSERT INTO source(n,v) VALUES(2,10),(1,20),(3,30)",
                            &crate::Parameters::new(),
                        )
                        .unwrap();
                    } else {
                        c.run("CREATE TABLE source(n INTEGER,v INTEGER)", &[])
                            .unwrap();
                        c.run("INSERT INTO source VALUES(2,10),(1,20),(3,30)", &[])
                            .unwrap();
                    }
                    if active {
                        c.run("BEGIN", &[]).unwrap();
                        c.run("INSERT INTO items VALUES(4,40)", &[]).unwrap();
                    }
                    let sql = format!(
                        "INSERT OR {policy} INTO items(n,v) SELECT n,v FROM source ORDER BY v RETURNING n,v"
                    );
                    let result = if mode != 0 {
                        c.execute(&sql, &crate::Parameters::new())
                            .map(|result| result.rows)
                    } else {
                        c.run(&sql, &[]).map(|rows| {
                            rows.into_iter()
                                .map(|row| row.into_iter().map(crate::from_engine).collect())
                                .collect()
                        })
                    }
                    .map_err(|error| error.to_string());
                    let observed = (
                        result,
                        c.transaction_state(),
                        c.run("SELECT n,v FROM items ORDER BY n", &[]).unwrap(),
                        c.run("SELECT changes(),last_insert_rowid()", &[]).unwrap(),
                    );
                    if mode != 0 {
                        assert_eq!(
                            Some(&observed),
                            baseline.as_ref(),
                            "{policy}, active={active}, mode={mode}"
                        );
                    } else {
                        baseline = Some(observed);
                    }
                }
            }
        }
    }
}
