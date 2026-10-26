//! Index use for documented spatial forms in ordinary SQL.
//!
//! A recognized condition restricts the collection to the IDs an indexed
//! search returns, as a semijoin on the stored ID column, and keeps every other
//! predicate, so the index only limits which documents are evaluated. A spatial index validates every stored value on write, so no
//! skipped document could have raised an error, and documents with missing or
//! NULL fields fail every recognized condition. Released `geo::distance` and
//! `geo::within` raise errors on NULL, so they are not rewritten in WHERE.
use super::{number, point, prepared, search_argument, MAX_NEAREST, NEAREST_FIRST_RADIUS_M};

/// A scalar index that selects fewer documents than this drives the statement
/// instead of a spatial search; each selected document is read once.
const SCALAR_PREFERRED: usize = 4096;

/// Sequential scan steps that cost about as much as reading one candidate by ID.
const SCAN_STEPS_PER_READ: usize = 8;

use crate::{Parameters, Result, Value};
use std::collections::BTreeSet;
use turso_parser::ast::{
    Expr, Indexed, Limit, OneSelect, Operator, ResultColumn, Select, SelectTable, SortOrder,
};

/// An indexed search with its query values already evaluated, and the bound
/// parameters those values came from.
struct Search {
    kind: SearchKind,
    consumed: BTreeSet<String>,
}

enum SearchKind {
    Region {
        index: String,
        region: Value,
        mode: &'static str,
    },
    Radius {
        index: String,
        center: Value,
        radius: Value,
    },
    Containing {
        index: String,
        point: Value,
        mode: &'static str,
    },
}

struct Scope {
    source: String,
    /// Top-level field, index name, and whether the index holds regions.
    fields: Vec<(String, String, bool)>,
    /// Top-level fields whose scalar index the ordinary plan can search.
    scalar: BTreeSet<String>,
}

impl Scope {
    /// Point index on the field an expression names, for a top-level path.
    fn index(&self, expr: &Expr) -> Option<&str> {
        self.lookup(expr, false)
    }

    /// Region index on the field an expression names, for a top-level path.
    fn region_index(&self, expr: &Expr) -> Option<&str> {
        self.lookup(expr, true)
    }

    fn lookup(&self, expr: &Expr, regions: bool) -> Option<&str> {
        let field = self.field(expr)?;
        self.fields
            .iter()
            .find(|(path, _, kind)| path == field && *kind == regions)
            .map(|(_, index, _)| index.as_str())
    }

    fn field<'a>(&self, expr: &'a Expr) -> Option<&'a str> {
        match strip(expr) {
            Expr::Id(name) => Some(name.as_str()),
            Expr::Qualified(table, name) if table.as_str().eq_ignore_ascii_case(&self.source) => {
                Some(name.as_str())
            }
            _ => None,
        }
    }

    /// Whether the ordinary plan can answer this condition from a scalar index:
    /// equality or IN with constants on a scalar-indexed top-level field.
    fn scalar_condition(&self, expr: &Expr, params: &Parameters) -> bool {
        let indexed = |field: &Expr| {
            self.field(field)
                .is_some_and(|name| self.scalar.contains(name))
        };
        match strip(expr) {
            Expr::Binary(left, Operator::Equals, right) => {
                (indexed(left) && constant(right, params).is_some())
                    || (indexed(right) && constant(left, params).is_some())
            }
            Expr::InList {
                lhs,
                rhs,
                not: false,
            } => {
                !rhs.is_empty()
                    && indexed(lhs)
                    && rhs.iter().all(|value| constant(value, params).is_some())
            }
            _ => false,
        }
    }
}

fn strip(expr: &Expr) -> &Expr {
    match expr {
        Expr::Parenthesized(items) if items.len() == 1 => strip(&items[0]),
        _ => expr,
    }
}

/// Operand of `x IS NOT NULL` or `x NOT NULL`.
fn not_null_operand(expr: &Expr) -> Option<&Expr> {
    match strip(expr) {
        Expr::NotNull(inner) => Some(inner),
        Expr::Binary(inner, Operator::IsNot, null)
            if matches!(strip(null), Expr::Literal(turso_parser::ast::Literal::Null)) =>
        {
            Some(inner)
        }
        _ => None,
    }
}

/// AND of the conjuncts not in `dropped`, in their original order.
fn remaining(parts: &[Expr], dropped: &BTreeSet<usize>) -> Option<Expr> {
    parts
        .iter()
        .enumerate()
        .filter(|(position, _)| !dropped.contains(position))
        .map(|(_, part)| Expr::Parenthesized(vec![Box::new(part.clone())]))
        .reduce(|left, right| Expr::Binary(Box::new(left), Operator::And, Box::new(right)))
}

fn and(existing: Option<Expr>, condition: Expr) -> Option<Expr> {
    Some(match existing {
        Some(existing) => Expr::Binary(Box::new(existing), Operator::And, Box::new(condition)),
        None => condition,
    })
}

fn set_where(select: &mut Select, condition: Option<Expr>) {
    let OneSelect::Select { where_clause, .. } = &mut select.body.select else {
        unreachable!("rewrite only handles simple SELECT bodies")
    };
    *where_clause = condition.map(Box::new);
}

fn conjuncts(expr: &Expr, out: &mut Vec<Expr>) {
    match strip(expr) {
        Expr::Binary(left, Operator::And, right) => {
            conjuncts(left, out);
            conjuncts(right, out);
        }
        other => out.push(other.clone()),
    }
}

/// Spatial operation and helper name of a lowered function call.
fn call(expr: &Expr) -> Option<(&'static str, &'static str, &[Box<Expr>])> {
    let Expr::FunctionCall {
        name,
        args,
        distinctness: None,
        order_by,
        filter_over,
        ..
    } = strip(expr)
    else {
        return None;
    };
    if !order_by.is_empty()
        || filter_over.filter_clause.is_some()
        || filter_over.over_clause.is_some()
    {
        return None;
    }
    let helper = name.as_str().strip_prefix("__romedb_h_")?;
    let (helper, operation) = super::operation(helper)?;
    Some((operation, helper, args.as_slice()))
}

impl crate::Connection {
    pub(crate) fn rewrite_spatial(
        &self,
        select: &mut Select,
        params: &Parameters,
        explain: bool,
    ) -> Result<()> {
        if !select.body.compounds.is_empty() {
            return Ok(());
        }
        let OneSelect::Select {
            from: Some(from),
            where_clause,
            group_by,
            distinctness,
            window_clause,
            columns,
        } = &select.body.select
        else {
            return Ok(());
        };
        if !from.joins.is_empty() {
            return Ok(());
        }
        let SelectTable::Table(name, alias, indexed) = from.select.as_ref() else {
            return Ok(());
        };
        if name.db_name.is_some()
            || matches!(indexed, Some(Indexed::NotIndexed | Indexed::IndexedBy(_)))
        {
            return Ok(());
        }
        let Ok(collection) = self.catalog(name.name.as_str()) else {
            return Ok(());
        };
        // SQLite lets WHERE name a result alias, which would hide the stored field.
        let aliases = columns
            .iter()
            .filter_map(|column| match column {
                ResultColumn::Expr(_, Some(alias)) => {
                    Some(alias.name().as_str().to_ascii_lowercase())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let fields = collection
            .indexes
            .iter()
            .filter(|index| index.kind == crate::IndexKind::Spatial && index.path.len() == 1)
            .filter(|index| !aliases.contains(&index.path[0].to_ascii_lowercase()))
            .map(|index| {
                (
                    index.path[0].clone(),
                    index.name.clone(),
                    index.region_index(),
                )
            })
            .collect::<Vec<_>>();
        if fields.is_empty() {
            return Ok(());
        }
        let scalar = collection
            .indexes
            .iter()
            .filter(|index| index.kind == crate::IndexKind::Scalar && index.path.len() == 1)
            .filter(|index| !aliases.contains(&index.path[0].to_ascii_lowercase()))
            .map(|index| index.path[0].clone())
            .collect();
        let scope = Scope {
            source: alias.as_ref().map_or_else(
                || name.name.as_str().to_owned(),
                |a| a.name().as_str().to_owned(),
            ),
            fields,
            scalar,
        };
        let from_sql = format!(
            "{} AS {}",
            crate::quote(name.name.as_str()),
            crate::quote(&scope.source)
        );
        let mut parts = Vec::new();
        if let Some(condition) = where_clause {
            conjuncts(condition, &mut parts);
        }
        let mut searches = Vec::new();
        let mut not_null = BTreeSet::new();
        let mut guards = Vec::new();
        for (position, part) in parts.iter().enumerate() {
            if let Some((index, search, exact)) = self.condition(part, &scope, params) {
                not_null.insert(index);
                searches.push((position, search, exact));
            } else if let Some(inner) = not_null_operand(part) {
                if let Some((index, _)) = distance(inner, &scope) {
                    not_null.insert(index.to_owned());
                    guards.push((position, inner.to_string()));
                }
            }
        }
        let nearest = if group_by.is_none()
            && distinctness.is_none()
            && window_clause.is_empty()
            && !aggregates(columns)
        {
            nearest_order(select, columns, &scope)
                .filter(|(index, _, _, _)| not_null.contains(*index))
        } else {
            None
        };
        let spatial_positions = searches
            .iter()
            .map(|(position, _, _)| *position)
            .chain(guards.iter().map(|(position, _)| *position))
            .collect::<BTreeSet<_>>();
        // Choices that count data are made only when the statement runs, so
        // EXPLAIN reads nothing and shows the spatial plan.
        let scalar = if explain {
            None
        } else {
            self.scalar_matches(&from_sql, &scope, &parts, &spatial_positions, params)?
        };
        if let Some((index, center, k, ordered)) = nearest {
            if let Some(k) = limit(k, params) {
                // The indexed candidate set excludes missing locations, so a
                // guard on the same distance expression is redundant.
                let ordered = ordered.to_string();
                let dropped = guards
                    .iter()
                    .filter(|(_, text)| *text == ordered)
                    .map(|(position, _)| *position)
                    .collect::<BTreeSet<_>>();
                let mut base = select.clone();
                set_where(&mut base, remaining(&parts, &dropped));
                // The scalar plan reads the `found` matching documents; rounds read
                // about k * entries / found candidates when matches are spread
                // evenly, plus about as much again in earlier rounds.
                let scalar_cheaper = match scalar.filter(|found| *found < SCALAR_PREFERRED) {
                    Some(found) => {
                        let entries =
                            self.index_entries(&self.spatial_index(index, "nearest ordering")?)?;
                        found * found < 2 * k * entries
                    }
                    None => false,
                };
                let search = if scalar_cheaper {
                    None
                } else {
                    self.nearest_round(&base, index, center, k, params, explain)?
                };
                if let Some(search) = search {
                    let condition = self.member(&search)?;
                    set_where(select, and(remaining(&parts, &dropped), condition));
                    return Ok(());
                }
                if searches.is_empty() {
                    // The ordinary plan computes distances only for documents
                    // that pass the cheaper conditions.
                    let guard_positions = guards
                        .iter()
                        .map(|(position, _)| *position)
                        .collect::<BTreeSet<_>>();
                    let others = remaining(&parts, &guard_positions);
                    let guarded = guard_positions
                        .iter()
                        .map(|position| {
                            Expr::Parenthesized(vec![Box::new(parts[*position].clone())])
                        })
                        .fold(others, and);
                    set_where(select, guarded);
                    return Ok(());
                }
            }
        }
        let Some((position, search, exact)) = searches.into_iter().next() else {
            return Ok(());
        };
        if let Some(found) = scalar.filter(|found| *found < SCALAR_PREFERRED) {
            // The scalar index plan reads `found` documents; prefer it unless
            // the spatial search returns no more candidates.
            if self.count_candidates(&search, found + 1)? > found {
                return Ok(());
            }
        }
        // An exactly equivalent condition is evaluated by the index refinement
        // on the same stored coordinates, so it is replaced, not repeated.
        let dropped = if exact {
            BTreeSet::from([position])
        } else {
            BTreeSet::new()
        };
        let condition = self.member(&search)?;
        set_where(select, and(remaining(&parts, &dropped), condition));
        Ok(())
    }

    /// Semijoin of the collection's stored IDs with the IDs the search returns.
    /// Index entries hold the same encoded IDs as the collection's key column,
    /// so no typed membership conversion is needed.
    fn member(&self, search: &Search) -> Result<Expr> {
        let sql = self.search_sql(search)?;
        let consumed = search
            .consumed
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(",");
        crate::select::rewrite_expression(&format!(
            "__romedb_h_spatial_member('{}', '{}')",
            format!("SELECT id FROM ({sql})").replace('\'', "''"),
            consumed.replace('\'', "''")
        ))
    }

    /// Index SQL returning the IDs of a search's candidates, refined exactly.
    fn search_sql(&self, search: &Search) -> Result<String> {
        Ok(match &search.kind {
            SearchKind::Region {
                index,
                region,
                mode,
            } => self.polygon_search_sql(&[
                Value::String(index.clone()),
                region.clone(),
                Value::String((*mode).into()),
            ])?,
            SearchKind::Radius {
                index,
                center,
                radius,
            } => self.spatial_search_sql(&Value::String(index.clone()), center, radius)?,
            SearchKind::Containing { index, point, mode } => self.containing_search_sql(&[
                Value::String(index.clone()),
                point.clone(),
                Value::String((*mode).into()),
            ])?,
        })
    }

    /// Entries in a spatial index, one per document.
    fn index_entries(&self, index: &crate::Index) -> Result<usize> {
        let rows = self.run_customer(
            &format!("SELECT count(*) FROM {}", crate::quote(&index.storage)),
            &[],
        )?;
        Ok(match rows.first().and_then(|row| row.first()) {
            Some(turso_core::Value::Numeric(turso_core::Numeric::Integer(n))) => *n as usize,
            _ => 0,
        })
    }

    /// Number of IDs a search returns, counting at most `limit`.
    fn count_candidates(&self, search: &Search, limit: usize) -> Result<usize> {
        let rows = self.run_customer(
            &format!(
                "SELECT count(*) FROM (SELECT 1 FROM ({}) LIMIT {limit})",
                self.search_sql(search)?
            ),
            &[],
        )?;
        Ok(match rows.first().and_then(|row| row.first()) {
            Some(turso_core::Value::Numeric(turso_core::Numeric::Integer(n))) => *n as usize,
            _ => 0,
        })
    }

    /// Fewest documents any scalar-indexed condition selects, counted up to
    /// SCALAR_PREFERRED through the ordinary plan, which searches that index.
    fn scalar_matches(
        &self,
        from: &str,
        scope: &Scope,
        parts: &[Expr],
        skip: &BTreeSet<usize>,
        params: &Parameters,
    ) -> Result<Option<usize>> {
        let mut fewest: Option<usize> = None;
        for (position, part) in parts.iter().enumerate() {
            if skip.contains(&position) || !scope.scalar_condition(part, params) {
                continue;
            }
            let found = self
                .collection_select_internal(
                    &format!("SELECT 1 FROM {from} WHERE {part} LIMIT {SCALAR_PREFERRED}"),
                    params,
                    true,
                )?
                .map_or(0, |result| result.rows.len());
            fewest = Some(fewest.map_or(found, |fewest| fewest.min(found)));
        }
        Ok(fewest)
    }

    /// Recognized condition, its index, and whether the index search returns
    /// exactly the documents for which the condition is true.
    fn condition(
        &self,
        expr: &Expr,
        scope: &Scope,
        params: &Parameters,
    ) -> Option<(String, Search, bool)> {
        if let Some(found) = containing_condition(expr, scope, params) {
            return Some(found);
        }
        if let Some((operation, helper, args)) = call(expr) {
            return match (operation, args) {
                ("contains", [region, field]) | ("inside", [field, region]) => {
                    let index = scope.index(field)?;
                    Some((
                        index.to_owned(),
                        region_search(index, region, "contains", params)?,
                        true,
                    ))
                }
                ("covers", [region, field]) => {
                    let index = scope.index(field)?;
                    Some((
                        index.to_owned(),
                        region_search(index, region, "covers", params)?,
                        true,
                    ))
                }
                ("intersects", [a, b]) => {
                    let (index, region) = match (scope.index(a), scope.index(b)) {
                        (Some(index), None) => (index, b),
                        (None, Some(index)) => (index, a),
                        _ => return None,
                    };
                    Some((
                        index.to_owned(),
                        region_search(index, region, "covers", params)?,
                        true,
                    ))
                }
                ("within", [a, b, radius, rest @ ..]) if helper == "st_dwithin" => {
                    if !rest.iter().all(|flag| sphere_flag(flag, params)) {
                        return None;
                    }
                    let (index, center) = match (scope.index(a), scope.index(b)) {
                        (Some(index), None) => (index, b),
                        (None, Some(index)) => (index, a),
                        _ => return None,
                    };
                    Some((
                        index.to_owned(),
                        radius_search(index, center, radius, params)?,
                        true,
                    ))
                }
                _ => None,
            };
        }
        let Expr::Binary(left, op, right) = strip(expr) else {
            return None;
        };
        let (measured, radius) = match op {
            Operator::Less | Operator::LessEquals => (left, right),
            Operator::Greater | Operator::GreaterEquals => (right, left),
            _ => return None,
        };
        let (index, center) = distance(measured, scope)?;
        if call(measured).is_some_and(|(_, helper, _)| helper == "geo_distance") {
            return None;
        }
        Some((
            index.to_owned(),
            radius_search(index, center, radius, params)?,
            matches!(op, Operator::LessEquals | Operator::GreaterEquals),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn nearest_round(
        &self,
        select: &Select,
        index: &str,
        center: &Expr,
        k: usize,
        params: &Parameters,
        explain: bool,
    ) -> Result<Option<Search>> {
        let Some(value) = constant(center, params) else {
            return Ok(None);
        };
        let Ok(origin) = point(&value) else {
            return Ok(None);
        };
        // Filters only remove points, so the radius holding k unfiltered points
        // is a safe first probe. Raw index rounds find it without document reads.
        let spatial = self.spatial_index(index, "nearest ordering")?;
        // EXPLAIN shows the first round at its nominal radius without reading
        // the index; execution starts from the radius that holds k points.
        let mut radius = if explain {
            NEAREST_FIRST_RADIUS_M
        } else {
            self.nearest_rows(&spatial, origin, k, "", NEAREST_FIRST_RADIUS_M)?
                .1
        };
        let mut examined = 0;
        let mut entries = None;
        loop {
            // A round over the whole globe cannot prune; the ordinary plan reads
            // each document once instead of through a candidate set of all IDs.
            if radius >= std::f64::consts::PI * super::EARTH_RADIUS_M {
                return Ok(None);
            }
            let search = Search {
                kind: SearchKind::Radius {
                    index: index.to_owned(),
                    center: value.clone(),
                    radius: Value::Number(radius),
                },
                consumed: names(&[center], params),
            };
            if explain {
                return Ok(Some(search));
            }
            let mut probe = select.clone();
            probe.order_by.clear();
            probe.limit = Some(Limit {
                expr: Box::new(Expr::Literal(turso_parser::ast::Literal::Numeric(
                    k.to_string(),
                ))),
                offset: None,
            });
            let OneSelect::Select {
                columns,
                where_clause,
                ..
            } = &mut probe.body.select
            else {
                unreachable!()
            };
            *columns = vec![ResultColumn::Expr(
                Box::new(Expr::Literal(turso_parser::ast::Literal::Numeric(
                    "1".into(),
                ))),
                None,
            )];
            let condition = self.member(&search)?;
            *where_clause = Some(Box::new(match where_clause.take() {
                Some(existing) => Expr::Binary(
                    Box::new(Expr::Parenthesized(vec![existing])),
                    Operator::And,
                    Box::new(condition),
                ),
                None => condition,
            }));
            self.record_spatial(0, 1);
            let found = self
                .collection_select_internal(&probe.to_string(), params, true)?
                .map_or(0, |result| result.rows.len());
            if found >= k {
                return Ok(Some(search));
            }
            // A selective filter can need most of the collection. Reading a
            // candidate by ID costs several times a sequential scan step, so once
            // the documents read so far, or those the observed match rate
            // predicts for the next round, reach an eighth of the index, one
            // ordinary scan is cheaper.
            let candidates = self.count_candidates(&search, i64::MAX as usize)?;
            examined += candidates;
            let entries = match entries {
                Some(entries) => entries,
                None => *entries.insert(self.index_entries(&spatial)?),
            };
            let predicted = k * candidates.max(1) / found.max(1);
            if SCAN_STEPS_PER_READ * examined.max(predicted) >= entries {
                return Ok(None);
            }
            radius = super::next_radius(radius, found, k);
        }
    }
}

/// `use_spheroid => false`, as a keyword, zero or a bound boolean.
fn sphere_flag(flag: &Expr, params: &Parameters) -> bool {
    use turso_parser::ast::Literal;
    match strip(flag) {
        Expr::Literal(Literal::False) => true,
        Expr::Literal(Literal::Numeric(n)) => n == "0",
        other => matches!(
            constant(other, params),
            Some(Value::Boolean(false) | Value::Integer(0))
        ),
    }
}

fn constant(expr: &Expr, params: &Parameters) -> Option<Value> {
    search_argument(expr, params, &mut BTreeSet::new()).ok()
}

/// Bound parameters an evaluated query value read.
fn names(exprs: &[&Expr], params: &Parameters) -> BTreeSet<String> {
    let mut consumed = BTreeSet::new();
    for expr in exprs {
        let _ = search_argument(expr, params, &mut consumed);
    }
    consumed
}

/// `None` keeps ordinary evaluation: arguments that are NULL, not constant or
/// not valid leave the statement's own predicate to decide the result (no rows
/// for NULL) or raise its error.
fn region_search(
    index: &str,
    region: &Expr,
    mode: &'static str,
    params: &Parameters,
) -> Option<Search> {
    let value = constant(region, params)?;
    prepared(&value).ok()?;
    Some(Search {
        kind: SearchKind::Region {
            index: index.to_owned(),
            region: value,
            mode,
        },
        consumed: names(&[region], params),
    })
}

fn radius_search(index: &str, center: &Expr, radius: &Expr, params: &Parameters) -> Option<Search> {
    let (c, r) = (constant(center, params)?, constant(radius, params)?);
    if point(&c).is_err()
        || !matches!(r, Value::Integer(_) | Value::Number(_))
        || !number(&r).is_ok_and(|r| r >= 0.0)
    {
        return None;
    }
    Some(Search {
        kind: SearchKind::Radius {
            index: index.to_owned(),
            center: c,
            radius: r,
        },
        consumed: names(&[center, radius], params),
    })
}

/// `covers(field, point)`, `contains(field, point)`, `inside(point, field)` and
/// `intersects` in either order, on a region-indexed field with a constant point.
/// The containing search refines with the same stored shapes, so it is exact.
fn containing_condition(
    expr: &Expr,
    scope: &Scope,
    params: &Parameters,
) -> Option<(String, Search, bool)> {
    let (operation, _, args) = call(expr)?;
    let (field, point_expr, mode) = match (operation, args) {
        ("covers", [field, p]) => (field, p, "covers"),
        ("contains", [field, p]) | ("inside", [p, field]) => (field, p, "contains"),
        ("intersects", [a, b]) => match (scope.region_index(a), scope.region_index(b)) {
            (Some(_), None) => (a, b, "covers"),
            (None, Some(_)) => (b, a, "covers"),
            _ => return None,
        },
        _ => return None,
    };
    let index = scope.region_index(field)?;
    let value = constant(point_expr, params)?;
    point(&value).ok()?;
    Some((
        index.to_owned(),
        Search {
            kind: SearchKind::Containing {
                index: index.to_owned(),
                point: value,
                mode,
            },
            consumed: names(&[point_expr], params),
        },
        true,
    ))
}

fn distance<'a>(expr: &'a Expr, scope: &'a Scope) -> Option<(&'a str, &'a Expr)> {
    let (operation, _, args) = call(expr)?;
    if operation != "distance" {
        return None;
    }
    match args {
        [a, b] => match (scope.index(a), scope.index(b)) {
            (Some(index), None) => Some((index, b)),
            (None, Some(index)) => Some((index, a)),
            _ => None,
        },
        _ => None,
    }
}

fn aggregates(columns: &[ResultColumn]) -> bool {
    columns.iter().any(|column| match column {
        ResultColumn::Expr(expr, _) => {
            let text = expr.to_string().to_ascii_lowercase();
            [
                "count(",
                "sum(",
                "avg(",
                "min(",
                "max(",
                "total(",
                "group_concat(",
                "string_agg(",
            ]
            .iter()
            .any(|name| text.contains(name))
        }
        _ => false,
    })
}

/// `ORDER BY distance(field, point) [, id] LIMIT k`, ascending, without OFFSET.
/// Returns the index, the query point, the LIMIT expression and the distance
/// expression being ordered.
fn nearest_order<'a>(
    select: &'a Select,
    columns: &'a [ResultColumn],
    scope: &'a Scope,
) -> Option<(&'a str, &'a Expr, &'a Expr, &'a Expr)> {
    let Limit {
        expr: k,
        offset: None,
    } = select.limit.as_ref()?
    else {
        return None;
    };
    let (first, rest) = select.order_by.split_first()?;
    let ascending = |order: &Option<SortOrder>| matches!(order, None | Some(SortOrder::Asc));
    if !ascending(&first.order) || first.nulls.is_some() || rest.len() > 1 {
        return None;
    }
    if let Some(second) = rest.first() {
        let is_id = match strip(&second.expr) {
            Expr::Id(name) => name.as_str().eq_ignore_ascii_case("id"),
            Expr::Qualified(table, name) => {
                table.as_str().eq_ignore_ascii_case(&scope.source)
                    && name.as_str().eq_ignore_ascii_case("id")
            }
            _ => false,
        };
        if !is_id || !ascending(&second.order) || second.nulls.is_some() {
            return None;
        }
    }
    let ordered = match strip(&first.expr) {
        Expr::Id(alias) => columns.iter().find_map(|column| match column {
            ResultColumn::Expr(expr, Some(name))
                if name.name().as_str().eq_ignore_ascii_case(alias.as_str()) =>
            {
                Some(expr.as_ref())
            }
            _ => None,
        })?,
        expr => expr,
    };
    let (index, center) = distance(ordered, scope)?;
    Some((index, center, k.as_ref(), ordered))
}

/// LIMIT as an integer from 1 to the nearest maximum. Literal numbers arrive as
/// floating-point values from argument evaluation.
fn limit(k: &Expr, params: &Parameters) -> Option<usize> {
    let k = match constant(k, params)? {
        Value::Integer(k) => k as f64,
        Value::Number(k) => k,
        _ => return None,
    };
    (k.fract() == 0.0 && (1.0..=MAX_NEAREST as f64).contains(&k)).then_some(k as usize)
}
