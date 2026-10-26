//! Grouped statements over collections read only the field values they need.
//!
//! The engine's GROUP BY sorter stores every column that the result, HAVING or
//! ORDER BY reads after grouping. Lowered collection queries read fields through
//! `__romedb_*` accessor calls on the stored document column, so the sorter would
//! copy whole documents. This rewrite evaluates those calls, and any other source
//! column used after grouping, in a FROM subquery over the original sources and
//! filter. The sorter then holds only the extracted values.
//!
//! The rewrite is skipped whenever the statement has a shape it does not need to
//! reason about: compounds, window functions, subqueries, unqualified names or
//! no accessor call to extract.
use turso_parser::ast::{
    As, Cmd, Expr, FromClause, Name, OneSelect, ResultColumn, Select, SelectBody, SelectTable, Stmt,
};

const SOURCE: &str = "__romedb_grouped";

pub(crate) fn narrow(cmd: &mut Cmd) {
    #[cfg(test)]
    if tests::DISABLED.with(std::cell::Cell::get) {
        return;
    }
    let stmt = match cmd {
        Cmd::Stmt(stmt) | Cmd::Explain(stmt) | Cmd::ExplainQueryPlan { stmt, .. } => stmt,
    };
    if let Stmt::Select(select) = stmt {
        narrow_select(select);
    }
}

fn narrow_select(select: &mut Select) {
    if !select.body.compounds.is_empty() {
        return;
    }
    let OneSelect::Select {
        columns,
        from: Some(from),
        where_clause,
        group_by: Some(group_by),
        window_clause,
        ..
    } = &mut select.body.select
    else {
        return;
    };
    if !window_clause.is_empty() {
        return;
    }
    let sources = source_names(from);
    if sources.is_empty() {
        return;
    }
    // Work on copies so a statement that cannot be rewritten stays untouched.
    let mut outer: Vec<Expr> = Vec::new();
    for column in columns.iter() {
        match column {
            ResultColumn::Expr(expr, _) => outer.push(*expr.clone()),
            ResultColumn::Star | ResultColumn::TableStar(_) => return,
        }
    }
    let group_count = group_by.exprs.len();
    outer.extend(group_by.exprs.iter().map(|expr| *expr.clone()));
    let having = group_by.having.is_some();
    if let Some(expr) = &group_by.having {
        outer.push(*expr.clone());
    }
    outer.extend(select.order_by.iter().map(|sorted| *sorted.expr.clone()));

    let mut extracted: Vec<(String, Expr)> = Vec::new();
    let mut accessors = 0;
    for expr in &mut outer {
        if extract(expr, &sources, &mut extracted, &mut accessors).is_none() {
            return;
        }
    }
    if accessors == 0 {
        return;
    }

    let inner_columns = extracted
        .iter()
        .enumerate()
        .map(|(i, (_, expr))| {
            ResultColumn::Expr(
                Box::new(expr.clone()),
                Some(As::As(Name::exact(format!("__romedb_g{i}")))),
            )
        })
        .collect();
    let inner = Select {
        with: None,
        body: SelectBody {
            select: OneSelect::Select {
                distinctness: None,
                columns: inner_columns,
                from: Some(from.clone()),
                where_clause: where_clause.take(),
                group_by: None,
                window_clause: Vec::new(),
            },
            compounds: Vec::new(),
        },
        order_by: Vec::new(),
        limit: None,
    };
    *from = FromClause {
        select: Box::new(SelectTable::Select(
            inner,
            Some(As::As(Name::exact(SOURCE.to_owned()))),
        )),
        joins: Vec::new(),
    };

    #[cfg(test)]
    tests::NARROWED.with(|count| count.set(count.get() + 1));
    let mut outer = outer.into_iter();
    for column in columns.iter_mut() {
        if let ResultColumn::Expr(expr, _) = column {
            **expr = outer.next().expect("result expression");
        }
    }
    for expr in group_by.exprs.iter_mut().take(group_count) {
        **expr = outer.next().expect("group expression");
    }
    if having {
        **group_by.having.as_mut().expect("HAVING") = outer.next().expect("HAVING expression");
    }
    for sorted in &mut select.order_by {
        *sorted.expr = outer.next().expect("ORDER BY expression");
    }
}

/// Table names and aliases that columns in this FROM clause may be qualified by.
fn source_names(from: &FromClause) -> Vec<String> {
    let mut names = Vec::new();
    for table in std::iter::once(&from.select).chain(from.joins.iter().map(|join| &join.table)) {
        let name = match table.as_ref() {
            SelectTable::Table(name, alias, _) => alias.as_ref().map_or_else(
                || name.name.as_str().to_owned(),
                |a| a.name().as_str().to_owned(),
            ),
            SelectTable::TableCall(_, _, Some(alias))
            | SelectTable::Select(_, Some(alias))
            | SelectTable::Sub(_, Some(alias)) => alias.name().as_str().to_owned(),
            _ => return Vec::new(),
        };
        names.push(name.to_ascii_lowercase());
    }
    names
}

fn local(table: &Name, sources: &[String]) -> bool {
    sources.contains(&table.as_str().to_ascii_lowercase())
}

/// Replace accessor calls and source columns with references to the subquery's
/// output. Returns `None` when the expression has a shape the rewrite skips.
fn extract(
    expr: &mut Expr,
    sources: &[String],
    extracted: &mut Vec<(String, Expr)>,
    accessors: &mut usize,
) -> Option<()> {
    let replace = |expr: &mut Expr, extracted: &mut Vec<(String, Expr)>| {
        let key = expr.to_string();
        let index = match extracted.iter().position(|(existing, _)| *existing == key) {
            Some(index) => index,
            None => {
                extracted.push((key, expr.clone()));
                extracted.len() - 1
            }
        };
        *expr = Expr::Qualified(
            Name::exact(SOURCE.to_owned()),
            Name::exact(format!("__romedb_g{index}")),
        );
    };
    match expr {
        Expr::FunctionCall {
            name,
            args,
            order_by,
            filter_over,
            ..
        } if name.as_str().starts_with("__romedb_")
            && order_by.is_empty()
            && filter_over.filter_clause.is_none()
            && filter_over.over_clause.is_none()
            && args.iter().any(
                |arg| matches!(arg.as_ref(), Expr::Qualified(table, _) if local(table, sources)),
            )
            && args.iter().all(|arg| {
                matches!(arg.as_ref(), Expr::Literal(_))
                    || matches!(arg.as_ref(), Expr::Qualified(table, _) if local(table, sources))
            }) =>
        {
            *accessors += 1;
            replace(expr, extracted);
            Some(())
        }
        Expr::Qualified(table, _) if local(table, sources) => {
            replace(expr, extracted);
            Some(())
        }
        // Correlated references to an enclosing query stay in the outer query.
        Expr::Qualified(..) | Expr::Literal(_) | Expr::Variable(_) => Some(()),
        Expr::Id(_)
        | Expr::Name(_)
        | Expr::DoublyQualified(..)
        | Expr::Subquery(_)
        | Expr::Exists(_)
        | Expr::InSelect { .. }
        | Expr::InTable { .. }
        | Expr::Raise(..) => None,
        Expr::FunctionCall {
            args,
            order_by,
            filter_over,
            ..
        } => {
            if filter_over.over_clause.is_some() {
                return None;
            }
            for arg in args.iter_mut() {
                extract(arg, sources, extracted, accessors)?;
            }
            for sorted in order_by.iter_mut() {
                extract(&mut sorted.expr, sources, extracted, accessors)?;
            }
            if let Some(filter) = &mut filter_over.filter_clause {
                extract(filter, sources, extracted, accessors)?;
            }
            Some(())
        }
        Expr::FunctionCallStar { filter_over, .. } => {
            (filter_over.over_clause.is_none() && filter_over.filter_clause.is_none()).then_some(())
        }
        other => {
            let mut result = Some(());
            each_child(other, &mut |child| {
                if result.is_some() {
                    result = extract(child, sources, extracted, accessors);
                }
            })?;
            result
        }
    }
}

/// Visit the direct children of the remaining expression kinds. Unknown kinds
/// return `None`, which keeps the statement unchanged.
fn each_child(expr: &mut Expr, visit: &mut dyn FnMut(&mut Expr)) -> Option<()> {
    match expr {
        Expr::Binary(left, _, right) => {
            visit(left);
            visit(right);
        }
        Expr::Unary(_, inner) | Expr::Collate(inner, _) | Expr::Cast { expr: inner, .. } => {
            visit(inner)
        }
        Expr::IsNull(inner) | Expr::NotNull(inner) => visit(inner),
        Expr::Parenthesized(items) => items.iter_mut().for_each(|item| visit(item)),
        Expr::Between {
            lhs, start, end, ..
        } => {
            visit(lhs);
            visit(start);
            visit(end);
        }
        Expr::InList { lhs, rhs, .. } => {
            visit(lhs);
            rhs.iter_mut().for_each(|item| visit(item));
        }
        Expr::Like {
            lhs, rhs, escape, ..
        } => {
            visit(lhs);
            visit(rhs);
            if let Some(escape) = escape {
                visit(escape);
            }
        }
        Expr::Case {
            base,
            when_then_pairs,
            else_expr,
        } => {
            if let Some(base) = base {
                visit(base);
            }
            for (when, then) in when_then_pairs.iter_mut() {
                visit(when);
                visit(then);
            }
            if let Some(else_expr) = else_expr {
                visit(else_expr);
            }
        }
        _ => return None,
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use crate::{Database, Parameters, Value};

    thread_local! {
        pub(super) static DISABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        pub(super) static NARROWED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn rows(c: &crate::Connection, sql: &str, narrowed: bool) -> crate::Result<Vec<Vec<Value>>> {
        DISABLED.with(|disabled| disabled.set(!narrowed));
        let result = c.execute(sql, &Parameters::new()).map(|result| result.rows);
        DISABLED.with(|disabled| disabled.set(false));
        result
    }

    #[test]
    fn narrowed_grouping_returns_the_same_rows_and_errors() {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let none = Parameters::new();
        for sql in [
            "CREATE TABLE people",
            "CREATE TABLE teams",
            "CREATE TABLE plain(k INTEGER, v TEXT)",
            "INSERT INTO plain VALUES(1,'a'),(1,'b'),(2,'c')",
        ] {
            c.execute(sql, &none).unwrap();
        }
        c.execute("BEGIN", &none).unwrap();
        for i in 0..300i64 {
            let team = ["red", "blue", "green"][(i % 3) as usize];
            let extra = match i % 5 {
                0 => ",tag:NULL".to_owned(),
                1 => String::new(),
                2 => format!(",tag:{}", i % 7),
                3 => format!(",tag:'t{}'", i % 4),
                _ => ",tag:[1,2]".to_owned(),
            };
            c.execute(
                &format!(
                    "INSERT INTO people {{id:type::record('people',{i}),team:teams:{team},n:{i},score:{}.5,name:'p{}',location:geo::point({}, {}){extra}}}",
                    i % 11,
                    i % 13,
                    (i % 40) as f64 - 20.0,
                    (i % 30) as f64 - 15.0
                ),
                &none,
            )
            .unwrap();
        }
        for team in ["red", "blue", "green"] {
            c.execute(
                &format!("INSERT INTO teams {{id:teams:{team},label:'{team} team'}}"),
                &none,
            )
            .unwrap();
        }
        c.execute("COMMIT", &none).unwrap();
        c.execute(
            "CREATE SEARCH INDEX people_location ON people(location) USING SPATIAL",
            &none,
        )
        .unwrap();
        let queries = [
            "SELECT team, count(*) FROM people GROUP BY team ORDER BY team",
            "SELECT tag, count(*), sum(n), max(name) FROM people GROUP BY tag ORDER BY tag",
            "SELECT team AS t, sum(score) AS total FROM people WHERE n > 10 GROUP BY t HAVING total > 100 ORDER BY t DESC",
            "SELECT n % 4 AS bucket, avg(score), min(name) FROM people GROUP BY n % 4 ORDER BY bucket",
            "SELECT geo::cell(location, 2) AS cell, count(*) FROM people GROUP BY geo::cell(location, 2) ORDER BY cell",
            "SELECT p.team, count(*), max(t.label) FROM people AS p JOIN teams AS t ON t.id = p.team GROUP BY p.team ORDER BY p.team",
            "SELECT geo::cell(p.location, 1) AS cell, count(*) AS c FROM search::bbox('people_location', -10, -10, 10, 10) AS v JOIN people AS p ON p.id = v.id GROUP BY geo::cell(p.location, 1) ORDER BY cell",
            "SELECT id, name FROM people WHERE n < 5 GROUP BY id ORDER BY id",
            "SELECT team, count(*) FILTER (WHERE n > 100) FROM people GROUP BY team ORDER BY team",
            "SELECT team, group_concat(name) FROM (SELECT team, name FROM people ORDER BY name) GROUP BY team ORDER BY team",
            "SELECT team, count(*) FROM people GROUP BY team HAVING count(*) > (SELECT count(*) FROM teams) ORDER BY team",
            "SELECT k, count(*), group_concat(v) FROM plain GROUP BY k ORDER BY k",
            "SELECT team, count(*) FROM people GROUP BY team ORDER BY count(*) DESC, team LIMIT 2",
            "SELECT tag + 1, count(*) FROM people GROUP BY tag + 1 ORDER BY 1",
        ];
        let mut narrowed = 0;
        for sql in queries {
            let expected = rows(&c, sql, false);
            let before = NARROWED.with(std::cell::Cell::get);
            let actual = rows(&c, sql, true);
            narrowed += usize::from(NARROWED.with(std::cell::Cell::get) > before);
            match (&expected, &actual) {
                (Ok(expected), Ok(actual)) => assert_eq!(actual, expected, "{sql}"),
                (Err(expected), Err(actual)) => assert_eq!(actual.code(), expected.code(), "{sql}"),
                _ => panic!("{sql}: {expected:?} vs {actual:?}"),
            }
        }
        // Grouping over a derived table, a native table and a correlated
        // subquery keep their original shape; the others are narrowed.
        assert!(narrowed >= 10, "{narrowed} of {} narrowed", queries.len());
    }
}
