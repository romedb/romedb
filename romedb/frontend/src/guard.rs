//! Guard-only AST normalization. Never execute this redacted representation.
use turso_core::{Result, WalkControl};
use turso_parser::ast::*;

pub(crate) fn reserved_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("__romedb_") || name.starts_with("__turso_internal_")
}

// RomeQL is not always parseable before expansion. Inspect the original token
// roles so generated helper names never need an exception here. SQLite also
// accepts single-quoted function names; ordinary string values remain data.
pub(crate) fn internal_names(sql: &str) -> crate::Result<()> {
    use romeql_parser::Kind;
    let tokens = romeql_parser::tokenize(sql)?;
    for (i, token) in tokens.iter().enumerate() {
        let name = matches!(token.kind, Kind::Word | Kind::Identifier)
            || (token.kind == Kind::String
                && tokens
                    .get(i + 1)
                    .is_some_and(|next| matches!(next.text.as_str(), "(" | ".")));
        if name && reserved_name(&token.text) {
            return Err(crate::Error::Unsupported("managed names".into()));
        }
    }
    Ok(())
}

pub(crate) fn tokens(sql: &str) -> crate::Result<Vec<romeql_parser::Token>> {
    tokens_inner(sql, false)
}

pub(crate) fn native_tokens(sql: &str) -> crate::Result<Vec<romeql_parser::Token>> {
    tokens_inner(sql, true)
}

fn tokens_inner(sql: &str, native: bool) -> crate::Result<Vec<romeql_parser::Token>> {
    let Ok(mut cmd) = crate::select::parsed(sql) else {
        if native {
            // Preserve the pinned engine's first-statement parse error before
            // unresolved managed names can hide it. RomeQL write guards retain
            // lexical fallback for syntax that has not been expanded yet.
            crate::parser_stack(|| turso_parser::parser::Parser::new(sql.as_bytes()).next_cmd())
                .map_err(turso_core::LimboError::from)?;
        }
        return Ok(romeql_parser::tokenize(sql)?);
    };
    let statement = match &mut cmd {
        Cmd::Stmt(s) | Cmd::Explain(s) | Cmd::ExplainQueryPlan { stmt: s, .. } => s,
    };
    match statement {
        Stmt::Select(s) => {
            if native {
                redact_cte_sources(s, &std::collections::BTreeSet::new())?;
            }
            select(s)?;
        }
        Stmt::Insert {
            with,
            body,
            returning,
            ..
        } => {
            ctes(with)?;
            if let InsertBody::Select(s, conflict) = body {
                select(s)?;
                upserts(conflict)?;
            }
            projections(returning)?;
        }
        Stmt::Update(s) => {
            ctes(&mut s.with)?;
            for set in &mut s.sets {
                expression(&mut set.expr)?;
            }
            from(&mut s.from)?;
            optional(&mut s.where_clause)?;
            projections(&mut s.returning)?;
        }
        Stmt::Delete {
            with,
            where_clause,
            returning,
            ..
        } => {
            ctes(with)?;
            optional(where_clause)?;
            projections(returning)?;
        }
        Stmt::CreateView { select: s, .. }
        | Stmt::CreateTable {
            body: CreateTableBody::AsSelect(s),
            ..
        } => select(s)?,
        Stmt::CreateTable {
            body:
                CreateTableBody::ColumnsAndConstraints {
                    columns,
                    constraints,
                    ..
                },
            ..
        } => {
            for column in columns {
                column_definition(column)?;
            }
            for constraint in constraints {
                match &mut constraint.constraint {
                    TableConstraint::Check { expr, source } => {
                        *source = None;
                        expression(expr)?;
                    }
                    TableConstraint::PrimaryKey { columns, .. }
                    | TableConstraint::Unique { columns, .. } => ordering(columns)?,
                    TableConstraint::ForeignKey { .. } => {}
                }
            }
        }
        Stmt::CreateIndex {
            columns,
            where_clause,
            ..
        } => {
            ordering(columns)?;
            optional(where_clause)?;
        }
        Stmt::AlterTable(AlterTable {
            body:
                AlterTableBody::AddColumn(column) | AlterTableBody::AlterColumn { new: column, .. },
            ..
        }) => column_definition(column)?,
        Stmt::CreateTrigger {
            when_clause,
            commands,
            ..
        } => {
            optional(when_clause)?;
            for command in commands {
                trigger_command(command)?;
            }
        }
        // PRAGMA, DDL names, and other uncovered contexts
        // retain the conservative guard until their reference roles are covered.
        _ => {}
    }
    Ok(romeql_parser::tokenize(&cmd.to_string())?)
}
fn expression(expr: &mut Expr) -> Result<()> {
    turso_core::walk_expr_mut(expr, &mut |expr| {
        match expr {
            Expr::Literal(Literal::String(_)) => {
                *expr = Expr::Literal(Literal::Numeric("0".into()));
            }
            Expr::Exists(s) | Expr::Subquery(s) => select(s)?,
            Expr::InSelect { rhs, .. } => select(rhs)?,
            _ => {}
        }
        Ok(WalkControl::Continue)
    })?;
    Ok(())
}
fn optional(expr: &mut Option<Box<Expr>>) -> Result<()> {
    if let Some(expr) = expr {
        expression(expr)?;
    }
    Ok(())
}
fn projections(columns: &mut [ResultColumn]) -> Result<()> {
    for column in columns {
        if let ResultColumn::Expr(expr, _) = column {
            expression(expr)?;
        }
    }
    Ok(())
}
fn ordering(columns: &mut [SortedColumn]) -> Result<()> {
    for column in columns {
        expression(&mut column.expr)?;
    }
    Ok(())
}
fn ctes(with: &mut Option<With>) -> Result<()> {
    if let Some(with) = with {
        for cte in &mut with.ctes {
            select(&mut cte.select)?;
        }
    }
    Ok(())
}
fn table(source: &mut SelectTable) -> Result<()> {
    match source {
        SelectTable::Select(s, _) => select(s)?,
        SelectTable::Sub(s, _) => from_clause(s)?,
        // Table-function arguments can designate objects (e.g. PRAGMA tables).
        SelectTable::Table(..) | SelectTable::TableCall(..) => {}
    }
    Ok(())
}
fn from_clause(source: &mut FromClause) -> Result<()> {
    table(&mut source.select)?;
    for join in &mut source.joins {
        table(&mut join.table)?;
        if let Some(JoinConstraint::On(expr)) = &mut join.constraint {
            expression(expr)?;
        }
    }
    Ok(())
}
fn from(source: &mut Option<FromClause>) -> Result<()> {
    if let Some(source) = source {
        from_clause(source)?;
    }
    Ok(())
}
fn core(core: &mut OneSelect) -> Result<()> {
    match core {
        OneSelect::Values(rows) => {
            for row in rows {
                for expr in row {
                    expression(expr)?;
                }
            }
        }
        OneSelect::Select {
            columns,
            from: source,
            where_clause,
            group_by,
            ..
        } => {
            projections(columns)?;
            from(source)?;
            optional(where_clause)?;
            if let Some(group) = group_by {
                for expr in &mut group.exprs {
                    expression(expr)?;
                }
                optional(&mut group.having)?;
            }
        }
    }
    Ok(())
}
fn select(select: &mut Select) -> Result<()> {
    ctes(&mut select.with)?;
    core(&mut select.body.select)?;
    for compound in &mut select.body.compounds {
        core(&mut compound.select)?;
    }
    ordering(&mut select.order_by)?;
    Ok(())
}

// Guard-only proof for CTE declarations and unqualified FROM references.
// Qualified expressions are redacted only for proven CTE sources.
// Schema-qualified tables and uncertain roles remain visible. Never execute this AST.
fn redact_cte_sources(
    select: &mut Select,
    inherited: &std::collections::BTreeSet<String>,
) -> Result<()> {
    use std::collections::BTreeSet;
    fn qualifier(expr: &mut Expr, bound: &BTreeSet<String>) -> Result<()> {
        turso_core::walk_expr_mut(expr, &mut |expr| {
            match expr {
                Expr::Exists(_) | Expr::Subquery(_) | Expr::InSelect { .. } => {
                    return Ok(WalkControl::SkipChildren)
                }
                Expr::Qualified(name, _) if bound.contains(&name.as_str().to_ascii_lowercase()) => {
                    *name = Name::exact(String::new())
                }
                _ => {}
            }
            Ok(WalkControl::Continue)
        })?;
        Ok(())
    }
    fn source(
        table: &mut SelectTable,
        visible: &BTreeSet<String>,
        bound: &mut BTreeSet<String>,
    ) -> Result<()> {
        match table {
            SelectTable::Table(name, alias, _)
                if name.db_name.is_none()
                    && visible.contains(&name.name.as_str().to_ascii_lowercase()) =>
            {
                if let Some(alias) = alias {
                    let identifier = alias.name().as_str().to_ascii_lowercase();
                    if !reserved_name(&identifier) && identifier != "writable_schema" {
                        bound.insert(identifier);
                        *alias = As::As(Name::exact(String::new()));
                    }
                } else {
                    bound.insert(name.name.as_str().to_ascii_lowercase());
                }
                name.name = Name::exact(String::new());
            }
            SelectTable::Select(inner, _) => redact_cte_sources(inner, visible)?,
            SelectTable::Sub(from, _) => sources(from, visible, bound)?,
            _ => {}
        }
        Ok(())
    }
    fn sources(
        from: &mut FromClause,
        visible: &BTreeSet<String>,
        bound: &mut BTreeSet<String>,
    ) -> Result<()> {
        source(&mut from.select, visible, bound)?;
        for join in &mut from.joins {
            source(&mut join.table, visible, bound)?;
        }
        for join in &mut from.joins {
            if let Some(JoinConstraint::On(expr)) = &mut join.constraint {
                qualifier(expr, bound)?;
            }
        }
        Ok(())
    }
    fn core(body: &mut OneSelect, visible: &BTreeSet<String>) -> Result<BTreeSet<String>> {
        let mut bound = BTreeSet::new();
        if let OneSelect::Select {
            from: Some(from),
            columns,
            where_clause,
            group_by,
            window_clause,
            ..
        } = body
        {
            sources(from, visible, &mut bound)?;
            for column in columns {
                match column {
                    ResultColumn::Expr(expr, _) => qualifier(expr, &bound)?,
                    ResultColumn::TableStar(name)
                        if bound.contains(&name.as_str().to_ascii_lowercase()) =>
                    {
                        *name = Name::exact(String::new())
                    }
                    _ => {}
                }
            }
            for definition in window_clause {
                for expr in &mut definition.window.partition_by {
                    qualifier(expr, &bound)?;
                }
                for ordering in &mut definition.window.order_by {
                    qualifier(&mut ordering.expr, &bound)?;
                }
            }
            if let Some(expr) = where_clause {
                qualifier(expr, &bound)?;
            }
            if let Some(group) = group_by {
                for expr in &mut group.exprs {
                    qualifier(expr, &bound)?;
                }
                if let Some(expr) = &mut group.having {
                    qualifier(expr, &bound)?;
                }
            }
        }
        Ok(bound)
    }
    let mut visible = inherited.clone();
    if let Some(with) = &mut select.with {
        if with.recursive {
            return Ok(());
        }
        for cte in &mut with.ctes {
            redact_cte_sources(&mut cte.select, &visible)?;
            let name = cte.tbl_name.as_str().to_ascii_lowercase();
            if !reserved_name(&name) && name != "writable_schema" {
                visible.insert(name);
                cte.tbl_name = Name::exact(String::new());
            }
        }
    }
    let bound = core(&mut select.body.select, &visible)?;
    if select.body.compounds.is_empty() {
        for ordering in &mut select.order_by {
            qualifier(&mut ordering.expr, &bound)?;
        }
    }
    for compound in &mut select.body.compounds {
        core(&mut compound.select, &visible)?;
    }
    Ok(())
}

fn column_definition(column: &mut ColumnDefinition) -> Result<()> {
    for constraint in &mut column.constraints {
        match &mut constraint.constraint {
            ColumnConstraint::Check { expr, source } => {
                *source = None;
                expression(expr)?;
            }
            ColumnConstraint::Default(expr) | ColumnConstraint::Generated { expr, .. } => {
                expression(expr)?
            }
            _ => {}
        }
    }
    Ok(())
}
fn upserts(conflict: &mut Option<Box<Upsert>>) -> Result<()> {
    let mut current = conflict.as_deref_mut();
    while let Some(clause) = current {
        if let Some(index) = &mut clause.index {
            ordering(&mut index.targets)?;
            optional(&mut index.where_clause)?;
        }
        if let UpsertDo::Set { sets, where_clause } = &mut clause.do_clause {
            for set in sets {
                expression(&mut set.expr)?;
            }
            optional(where_clause)?;
        }
        current = clause.next.as_deref_mut();
    }
    Ok(())
}

fn trigger_command(command: &mut TriggerCmd) -> Result<()> {
    match command {
        TriggerCmd::Select(s) => select(s)?,
        TriggerCmd::Insert {
            select: s,
            upsert,
            returning,
            ..
        } => {
            select(s)?;
            upserts(upsert)?;
            projections(returning)?;
        }
        TriggerCmd::Update {
            sets,
            from: source,
            where_clause,
            ..
        } => {
            for set in sets {
                expression(&mut set.expr)?;
            }
            from(source)?;
            optional(where_clause)?;
        }
        TriggerCmd::Delete { where_clause, .. } => optional(where_clause)?,
    }
    Ok(())
}

#[cfg(test)]
mod cte_guard_tests {
    #[test]
    fn cte_redaction_preserves_real_schema_and_internal_references() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE docs", &crate::Parameters::new())
            .unwrap();
        c.guard_native_sql(
            "WITH docs AS (SELECT 2 AS n), chosen AS (SELECT n FROM docs) SELECT n FROM chosen",
        )
        .unwrap();
        for sql in [
            "WITH docs AS (SELECT 2 AS n) SELECT * FROM main.docs",
            "WITH docs AS (SELECT 2 AS n) SELECT sum(docs.n) OVER w FROM docs WINDOW w AS (ORDER BY (SELECT n FROM main.docs))",
            "WITH docs AS (SELECT 2 AS n) SELECT sum(docs.n) OVER w FROM docs WINDOW w AS (ORDER BY main.docs.n)",
            "WITH safe AS (SELECT 2 AS n) SELECT docs.n FROM main.docs AS docs",
            "WITH safe AS (SELECT 2 AS n) SELECT __romedb_catalog.n FROM safe AS __romedb_catalog",
            "WITH safe AS (SELECT 2 AS n) SELECT writable_schema.n FROM safe AS writable_schema",
            "WITH safe AS (SELECT 2 AS n) SELECT (SELECT docs.n FROM main.docs AS docs) FROM safe AS docs",
            "WITH docs AS (SELECT 2 AS n) SELECT docs.n FROM main.docs",
            "WITH docs AS (SELECT 2 AS n) SELECT (SELECT docs.n FROM main.docs) FROM docs",
            "WITH docs AS (SELECT 2 AS n) SELECT main.docs.n FROM main.docs",
            "WITH alias AS (SELECT * FROM main.docs) SELECT * FROM alias",
            "WITH docs AS (SELECT * FROM __romedb_catalog) SELECT * FROM docs",
            "WITH __romedb_catalog AS (SELECT 1 AS n) SELECT n FROM __romedb_catalog",
            "WITH writable_schema AS (SELECT 1 AS n) SELECT n FROM writable_schema",
            "WITH chosen AS (SELECT * FROM docs), docs AS (SELECT 2 AS n) SELECT * FROM chosen",
            "WITH docs AS (SELECT * FROM docs) SELECT * FROM docs",
            "WITH docs AS (SELECT 2 AS n) SELECT (SELECT * FROM main.docs)",
        ] {
            assert!(c.guard_native_sql(sql).is_err(), "{sql}");
        }
    }
}
