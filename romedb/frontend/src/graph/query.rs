//! GRAPH(...) sources: bounded traversal over adjacency indexes, materialized
//! as a typed relation that the outer SQL query joins, filters and orders.
use super::predicate::{parameters, Predicate};
use crate::{quote, Collection, Connection, Document, Error, Parameters, Result, Value};
use romeql_parser::{Direction, GraphMode, GraphQuery};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use turso_core::Value as EngineValue;

/// Per-statement graph bounds. Hosts may lower these; exceeding one is an error,
/// never a truncated result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphLimits {
    pub max_depth: u32,
    pub max_adjacency_entries: u64,
    pub max_expansions: u64,
    pub max_retained_nodes: usize,
    pub max_path_states: usize,
    pub max_rows: usize,
    pub max_bytes: usize,
}
impl Default for GraphLimits {
    fn default() -> Self {
        Self {
            max_depth: 32,
            max_adjacency_entries: 100_000,
            max_expansions: 100_000,
            max_retained_nodes: 50_000,
            max_path_states: 10_000,
            max_rows: 10_000,
            max_bytes: 32 * 1024 * 1024,
        }
    }
}
impl GraphLimits {
    fn within(&self, ceiling: &Self) -> bool {
        self.max_depth <= ceiling.max_depth
            && self.max_adjacency_entries <= ceiling.max_adjacency_entries
            && self.max_expansions <= ceiling.max_expansions
            && self.max_retained_nodes <= ceiling.max_retained_nodes
            && self.max_path_states <= ceiling.max_path_states
            && self.max_rows <= ceiling.max_rows
            && self.max_bytes <= ceiling.max_bytes
    }
}

/// Work done by GRAPH sources in one statement, reported through query profiles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GraphCounters {
    pub(crate) adjacency_entries: u64,
    pub(crate) expansions: u64,
    pub(crate) documents: u64,
    pub(crate) rows: u64,
    pub(crate) visited: u64,
    pub(crate) peak_bytes: u64,
}

thread_local! {
    static EXPLAIN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
/// Marks lowering for EXPLAIN so graph and spatial sources describe their plan
/// without reading data.
pub(crate) struct ExplainScope(bool);

/// Whether the current lowering is for EXPLAIN.
pub(crate) fn explaining() -> bool {
    EXPLAIN.with(std::cell::Cell::get)
}
impl ExplainScope {
    pub(crate) fn enter(explain: bool) -> Self {
        Self(EXPLAIN.with(|cell| cell.replace(cell.get() || explain)))
    }
}
impl Drop for ExplainScope {
    fn drop(&mut self) {
        EXPLAIN.with(|cell| cell.set(self.0));
    }
}

/// Replace `GRAPH(...)` after FROM or JOIN with an internal table call carrying the
/// validated path text. Arrows keep their SQL meaning everywhere else.
pub(crate) fn expand_sources(sql: &str) -> Result<std::borrow::Cow<'_, str>> {
    use romeql_parser::Kind;
    if !sql
        .as_bytes()
        .windows(5)
        .any(|w| w.eq_ignore_ascii_case(b"graph"))
    {
        return Ok(std::borrow::Cow::Borrowed(sql));
    }
    let tokens = romeql_parser::tokenize(sql)?;
    let mut out = String::new();
    let mut copied = 0;
    let mut i = 1;
    while i + 1 < tokens.len() {
        let previous = &tokens[i - 1];
        if tokens[i].kind == Kind::Word
            && tokens[i].text.eq_ignore_ascii_case("GRAPH")
            && tokens[i + 1].text == "("
            && previous.kind == Kind::Word
            && (previous.text.eq_ignore_ascii_case("FROM")
                || previous.text.eq_ignore_ascii_case("JOIN"))
        {
            let mut depth = 0usize;
            let mut close = None;
            for (j, token) in tokens.iter().enumerate().skip(i + 1) {
                if token.kind != Kind::Symbol {
                    continue;
                }
                match token.text.as_str() {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let close = close.ok_or_else(|| Error::Validation("unclosed GRAPH source".into()))?;
            let body = &sql[tokens[i + 1].end..tokens[close].start];
            romeql_parser::parse_graph(body).map_err(|e| {
                Error::Syntax(romeql_parser::Error {
                    offset: e.offset + tokens[i + 1].end,
                    message: format!("GRAPH: {}", e.message),
                })
            })?;
            out.push_str(&sql[copied..tokens[i].start]);
            out.push_str("__romedb_graph('");
            out.push_str(&body.replace('\'', "''"));
            out.push_str("')");
            copied = tokens[close].end;
            i = close + 1;
            continue;
        }
        i += 1;
    }
    if copied == 0 {
        return Ok(std::borrow::Cow::Borrowed(sql));
    }
    out.push_str(&sql[copied..]);
    Ok(std::borrow::Cow::Owned(out))
}

struct Hop {
    edges: Collection,
    adjacency: String,
    outgoing: bool,
    incoming: bool,
    edge_predicate: Option<Predicate>,
    node: Collection,
    node_predicate: Option<Predicate>,
}

type Id = Arc<[u8]>;

struct Path {
    nodes: Vec<Id>,
    edges: Vec<Id>,
}

struct Row {
    node: Id,
    edge: Option<Id>,
    depth: u32,
    path: Option<(Vec<Id>, Vec<Id>)>,
}

struct Traversal<'a> {
    connection: &'a Connection,
    limits: GraphLimits,
    counters: GraphCounters,
    retained_bytes: u64,
    checked_plans: HashSet<(String, bool)>,
    edge_matches: HashMap<(usize, Id), bool>,
    node_matches: HashMap<(usize, Id), bool>,
}

fn limit(what: &str, value: impl std::fmt::Display) -> Error {
    Error::Limit(format!("graph {what} exceeds {value}"))
}

impl Connection {
    /// Set lower graph limits for later statements on this connection.
    pub fn set_graph_limits(&self, limits: GraphLimits) -> Result<()> {
        if !limits.within(&GraphLimits::default()) {
            return Err(Error::Validation(
                "graph limits can only be lowered below the defaults".into(),
            ));
        }
        *self.graph_limits.lock().unwrap_or_else(|e| e.into_inner()) = limits;
        Ok(())
    }
    pub fn graph_limits(&self) -> GraphLimits {
        *self.graph_limits.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn take_graph_counters(&self) -> GraphCounters {
        std::mem::take(
            &mut *self
                .graph_counters
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }
    pub(crate) fn take_graph_plan(&self) -> Vec<String> {
        std::mem::take(&mut *self.graph_plan.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Run one GRAPH source and return SQL producing its typed rows, plus whether
    /// path columns are included.
    pub(crate) fn graph_source_sql(
        &self,
        body: &str,
        params: &Parameters,
        consumed: &mut BTreeSet<String>,
    ) -> Result<(String, bool)> {
        let query = romeql_parser::parse_graph(body)?;
        for name in {
            let mut names = BTreeSet::new();
            parameters(&query.root, &mut names);
            for hop in &query.hops {
                for step in [&hop.edge, &hop.node] {
                    if let Some(predicate) = &step.predicate {
                        parameters(predicate, &mut names);
                    }
                }
            }
            names
        } {
            if !params.contains_key(&name) {
                return Err(Error::Parameter(name));
            }
            consumed.insert(name);
        }
        let with_path = query.with_path;
        let limits = self.graph_limits();
        if query.hops.len() > limits.max_depth as usize
            || query.depth.is_some_and(|(_, max)| max > limits.max_depth)
        {
            return Err(limit("depth", limits.max_depth));
        }
        let root = match self.evaluate(query.root.clone(), params, None)? {
            Value::Record(mut record) => {
                record.table = crate::canonical(&record.table)?;
                record
            }
            other => {
                return Err(Error::Validation(format!(
                    "GRAPH root must be a typed record, got {}",
                    match other {
                        Value::Null => "null",
                        Value::String(_) => "a string",
                        Value::Integer(_) | Value::Number(_) => "a number",
                        _ => "another value",
                    }
                )))
            }
        };
        let root_collection = self.catalog(&root.table)?;
        let hops = self.graph_hops(&query, &root_collection, params)?;
        let explain = EXPLAIN.with(std::cell::Cell::get);
        if explain {
            let mut plan = self.graph_plan.lock().unwrap_or_else(|e| e.into_inner());
            plan.push(format!(
                "GRAPH ROOT {} USING PRIMARY KEY",
                root_collection.name
            ));
            for (i, hop) in hops.iter().enumerate() {
                let mut detail = format!("GRAPH HOP {} {}", i + 1, hop.edges.name);
                for (used, index) in [
                    (hop.outgoing, format!("{}_out", hop.adjacency)),
                    (hop.incoming, format!("{}_in", hop.adjacency)),
                ] {
                    if used {
                        detail.push_str(&format!(" SEARCH USING COVERING INDEX {index}"));
                    }
                }
                if hop.edge_predicate.is_some() {
                    detail.push_str(" EDGE FILTER");
                }
                detail.push_str(&format!(" TO {}", hop.node.name));
                if hop.node_predicate.is_some() {
                    detail.push_str(" NODE FILTER");
                }
                plan.push(detail);
            }
            if let (Some((min, max)), Some(mode)) = (query.depth, query.mode) {
                plan.push(format!(
                    "GRAPH RECURSION DEPTH {min} TO {max} MODE {} {}",
                    match mode {
                        GraphMode::Reachable => "REACHABLE (breadth-first, visited set per root)",
                        GraphMode::Paths => "PATHS (simple paths, no repeated node)",
                    },
                    if with_path { "WITH PATH" } else { "" }
                ));
            }
            plan.push(format!(
                "GRAPH MATERIALIZE limits depth={} adjacency_entries={} expansions={} retained_nodes={} path_states={} rows={} bytes={}",
                limits.max_depth,
                limits.max_adjacency_entries,
                limits.max_expansions,
                limits.max_retained_nodes,
                limits.max_path_states,
                limits.max_rows,
                limits.max_bytes
            ));
            return Ok((rows_sql(&[], &[], with_path)?, with_path));
        }
        let root_id: Id = Value::Record(root).encode()?.into();
        let mut traversal = Traversal {
            connection: self,
            limits,
            counters: GraphCounters::default(),
            retained_bytes: 0,
            checked_plans: HashSet::new(),
            edge_matches: HashMap::new(),
            node_matches: HashMap::new(),
        };
        let exists = !self
            .run_customer(
                &format!(
                    "SELECT 1 FROM {} WHERE id = ?1",
                    quote(&root_collection.storage)
                ),
                &[EngineValue::Blob(root_id.to_vec())],
            )?
            .is_empty();
        let rows = if !exists {
            Vec::new()
        } else {
            match (query.depth, query.mode) {
                (Some((min, max)), Some(GraphMode::Reachable)) => {
                    traversal.reachable(&root_id, &hops[0], min, max, with_path)?
                }
                (Some((min, max)), Some(GraphMode::Paths)) => {
                    traversal.paths(&root_id, &hops[0], min, max)?
                }
                _ => traversal.fixed(&root_id, &hops, with_path)?,
            }
        };
        traversal.counters.rows = rows.len() as u64;
        let sql = rows_sql(&rows, &root_id, with_path)?;
        let mut counters = self
            .graph_counters
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let c = traversal.counters;
        counters.adjacency_entries += c.adjacency_entries;
        counters.expansions += c.expansions;
        counters.documents += c.documents;
        counters.rows += c.rows;
        counters.visited += c.visited;
        counters.peak_bytes = counters.peak_bytes.max(c.peak_bytes);
        Ok((sql, with_path))
    }

    fn graph_hops(
        &self,
        query: &GraphQuery,
        root: &Collection,
        params: &Parameters,
    ) -> Result<Vec<Hop>> {
        let mut current = root.name.clone();
        let mut hops = Vec::new();
        for hop in &query.hops {
            let edges = self.catalog(&hop.edge.table)?;
            let Some(edge) = edges.edge.clone() else {
                return Err(Error::Validation(format!(
                    "{} is not an edge table; declare it with CREATE EDGE TABLE",
                    edges.name
                )));
            };
            let node = self.catalog(&hop.node.table)?;
            let forward = edge.source == current && edge.target == node.name;
            let backward = edge.target == current && edge.source == node.name;
            let (outgoing, incoming) = match hop.direction {
                Direction::Out => (forward, false),
                Direction::In => (false, backward),
                Direction::Both => (forward, backward),
            };
            if !outgoing && !incoming {
                return Err(Error::Validation(format!(
                    "edge table {} connects {} to {}; it cannot traverse {current} {} {}",
                    edges.name,
                    edge.source,
                    edge.target,
                    match hop.direction {
                        Direction::Out => "->",
                        Direction::In => "<-",
                        Direction::Both => "<->",
                    },
                    node.name
                )));
            }
            if query.depth.is_some() && !(edge.source == edge.target && node.name == current) {
                return Err(Error::Validation(format!(
                    "recursive GRAPH queries require an edge table from {current} to {current}"
                )));
            }
            let compile = |step: &romeql_parser::GraphStep| {
                step.predicate
                    .clone()
                    .map(|predicate| {
                        let mut names = vec![step.table.as_str()];
                        if let Some(alias) = &step.alias {
                            names.push(alias.as_str());
                        }
                        Predicate::compile(self, predicate, &names, params)
                    })
                    .transpose()
            };
            hops.push(Hop {
                edge_predicate: compile(&hop.edge)?,
                node_predicate: compile(&hop.node)?,
                adjacency: edge.adjacency,
                edges,
                outgoing,
                incoming,
                node,
            });
            current = hops.last().expect("hop").node.name.clone();
        }
        Ok(hops)
    }
}

impl Traversal<'_> {
    fn retain(&mut self, bytes: usize) -> Result<()> {
        self.retained_bytes += bytes as u64 + 16;
        self.counters.peak_bytes = self.counters.peak_bytes.max(self.retained_bytes);
        if self.retained_bytes > self.limits.max_bytes as u64 {
            return Err(limit("retained bytes", self.limits.max_bytes));
        }
        Ok(())
    }

    fn expansion(&mut self) -> Result<()> {
        self.counters.expansions += 1;
        if self.counters.expansions > self.limits.max_expansions {
            return Err(limit("expansions", self.limits.max_expansions));
        }
        Ok(())
    }

    fn row_limit(&self, rows: usize) -> Result<()> {
        if rows > self.limits.max_rows {
            return Err(limit("rows", self.limits.max_rows));
        }
        Ok(())
    }

    /// Adjacent (neighbor, edge) pairs for each frontier node, in canonical
    /// (neighbor, edge) order. A bidirectional self-loop appears once.
    fn neighbors(&mut self, hop: &Hop, frontier: &[Id]) -> Result<HashMap<Id, Vec<(Id, Id)>>> {
        let mut found: HashMap<Id, Vec<(Id, Id)>> = HashMap::new();
        for chunk in frontier.chunks(256) {
            self.connection.check_cancellation()?;
            for (outgoing, used) in [(true, hop.outgoing), (false, hop.incoming)] {
                if !used {
                    continue;
                }
                let (from, to, index) = if outgoing {
                    ("source_id", "target_id", format!("{}_out", hop.adjacency))
                } else {
                    ("target_id", "source_id", format!("{}_in", hop.adjacency))
                };
                let slots = (1..=chunk.len())
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT {from},{to},edge_id FROM {} WHERE {from} IN ({slots})",
                    quote(&hop.adjacency)
                );
                let values: Vec<_> = chunk
                    .iter()
                    .map(|id| EngineValue::Blob(id.to_vec()))
                    .collect();
                if self.checked_plans.insert((index.clone(), outgoing)) {
                    let plan = self
                        .connection
                        .run(&format!("EXPLAIN QUERY PLAN {sql}"), &values)?;
                    if !plan.iter().any(|row| {
                        matches!(row.last(), Some(EngineValue::Text(t))
                            if t.as_str().starts_with("SEARCH") && t.as_str().contains(index.as_str()))
                    }) {
                        let plan: Vec<String> = plan
                            .iter()
                            .filter_map(|row| match row.last() {
                                Some(EngineValue::Text(t)) => Some(t.as_str().to_owned()),
                                _ => None,
                            })
                            .collect();
                        return Err(Error::Storage(format!(
                            "graph adjacency read did not select index {index}: {}",
                            plan.join("; ")
                        )));
                    }
                }
                for row in self.connection.run_customer(&sql, &values)? {
                    let [EngineValue::Blob(from), EngineValue::Blob(to), EngineValue::Blob(edge)] =
                        row.as_slice()
                    else {
                        return Err(Error::Storage("invalid adjacency entry".into()));
                    };
                    self.counters.adjacency_entries += 1;
                    if self.counters.adjacency_entries > self.limits.max_adjacency_entries {
                        return Err(limit(
                            "adjacency entries examined",
                            self.limits.max_adjacency_entries,
                        ));
                    }
                    let from = chunk
                        .iter()
                        .find(|id| id.as_ref() == from.as_slice())
                        .cloned()
                        .ok_or_else(|| Error::Storage("unexpected adjacency entry".into()))?;
                    found
                        .entry(from)
                        .or_default()
                        .push((Arc::from(to.as_slice()), Arc::from(edge.as_slice())));
                }
            }
        }
        for pairs in found.values_mut() {
            pairs.sort();
            if hop.outgoing && hop.incoming {
                let mut seen = HashSet::new();
                pairs.retain(|pair| seen.insert(pair.1.clone()));
            }
        }
        Ok(found)
    }

    fn document(&mut self, collection: &Collection, id: &Id) -> Result<Option<Document>> {
        self.counters.documents += 1;
        self.connection
            .run_customer(
                &format!(
                    "SELECT doc FROM {} WHERE id = ?1",
                    quote(&collection.storage)
                ),
                &[EngineValue::Blob(id.to_vec())],
            )?
            .first()
            .map(|row| crate::decode_document(&row[0]))
            .transpose()
    }

    fn accept(&mut self, step: usize, hop: &Hop, neighbor: &Id, edge: &Id) -> Result<bool> {
        if let Some(predicate) = &hop.edge_predicate {
            let key = (step, edge.clone());
            let matched = match self.edge_matches.get(&key) {
                Some(matched) => *matched,
                None => {
                    let doc = self.document(&hop.edges, edge)?.ok_or_else(|| {
                        Error::Storage("adjacency refers to a missing edge".into())
                    })?;
                    let matched = predicate.matches(&doc)?;
                    self.edge_matches.insert(key, matched);
                    matched
                }
            };
            if !matched {
                return Ok(false);
            }
        }
        if let Some(predicate) = &hop.node_predicate {
            let key = (step, neighbor.clone());
            let matched = match self.node_matches.get(&key) {
                Some(matched) => *matched,
                None => {
                    let doc = self.document(&hop.node, neighbor)?.ok_or_else(|| {
                        Error::Storage("adjacency refers to a missing node".into())
                    })?;
                    let matched = predicate.matches(&doc)?;
                    self.node_matches.insert(key, matched);
                    matched
                }
            };
            return Ok(matched);
        }
        Ok(true)
    }

    fn fixed(&mut self, root: &Id, hops: &[Hop], with_path: bool) -> Result<Vec<Row>> {
        let mut paths = vec![Path {
            nodes: vec![root.clone()],
            edges: Vec::new(),
        }];
        for (step, hop) in hops.iter().enumerate() {
            let mut seen = HashSet::new();
            let frontier: Vec<Id> = paths
                .iter()
                .map(|p| p.nodes.last().expect("path end").clone())
                .filter(|id| seen.insert(id.clone()))
                .collect();
            let adjacent = self.neighbors(hop, &frontier)?;
            let mut next = Vec::new();
            for path in &paths {
                let end = path.nodes.last().expect("path end");
                for (neighbor, edge) in adjacent.get(end).into_iter().flatten() {
                    self.expansion()?;
                    if !self.accept(step, hop, neighbor, edge)? {
                        continue;
                    }
                    if next.len() >= self.limits.max_path_states {
                        return Err(limit("path states", self.limits.max_path_states));
                    }
                    let mut nodes = path.nodes.clone();
                    nodes.push(neighbor.clone());
                    let mut edges = path.edges.clone();
                    edges.push(edge.clone());
                    self.retain(neighbor.len() + edge.len() + 32 * (nodes.len() + edges.len()))?;
                    next.push(Path { nodes, edges });
                }
            }
            paths = next;
        }
        self.row_limit(paths.len())?;
        Ok(paths
            .into_iter()
            .map(|path| Row {
                node: path.nodes.last().expect("path end").clone(),
                edge: path.edges.last().cloned(),
                depth: hops.len() as u32,
                path: with_path.then_some((path.nodes, path.edges)),
            })
            .collect())
    }

    fn reachable(
        &mut self,
        root: &Id,
        hop: &Hop,
        min: u32,
        max: u32,
        with_path: bool,
    ) -> Result<Vec<Row>> {
        let mut visited: HashMap<Id, Option<(Id, Id)>> = HashMap::new();
        visited.insert(root.clone(), None);
        self.counters.visited = 1;
        let mut rows = Vec::new();
        let mut frontier = vec![root.clone()];
        let witness = |visited: &HashMap<Id, Option<(Id, Id)>>, node: &Id| {
            let mut nodes = vec![node.clone()];
            let mut edges = Vec::new();
            let mut current = node.clone();
            while let Some(Some((parent, edge))) = visited.get(&current) {
                nodes.push(parent.clone());
                edges.push(edge.clone());
                current = parent.clone();
            }
            nodes.reverse();
            edges.reverse();
            (nodes, edges)
        };
        if min == 0 {
            rows.push(Row {
                node: root.clone(),
                edge: None,
                depth: 0,
                path: with_path.then(|| (vec![root.clone()], Vec::new())),
            });
        }
        for depth in 1..=max {
            if frontier.is_empty() {
                break;
            }
            let adjacent = self.neighbors(hop, &frontier)?;
            let mut next = Vec::new();
            for node in &frontier {
                for (neighbor, edge) in adjacent.get(node).into_iter().flatten() {
                    self.expansion()?;
                    if visited.contains_key(neighbor) || !self.accept(0, hop, neighbor, edge)? {
                        continue;
                    }
                    visited.insert(neighbor.clone(), Some((node.clone(), edge.clone())));
                    self.counters.visited += 1;
                    if visited.len() + next.len() > self.limits.max_retained_nodes {
                        return Err(limit("retained nodes", self.limits.max_retained_nodes));
                    }
                    self.retain(neighbor.len() * 2 + edge.len() + 48)?;
                    if depth >= min {
                        self.row_limit(rows.len() + 1)?;
                        rows.push(Row {
                            node: neighbor.clone(),
                            edge: Some(edge.clone()),
                            depth,
                            path: with_path.then(|| witness(&visited, neighbor)),
                        });
                    }
                    next.push(neighbor.clone());
                }
            }
            frontier = next;
        }
        Ok(rows)
    }

    fn paths(&mut self, root: &Id, hop: &Hop, min: u32, max: u32) -> Result<Vec<Row>> {
        let mut rows = Vec::new();
        if min == 0 {
            rows.push(Row {
                node: root.clone(),
                edge: None,
                depth: 0,
                path: Some((vec![root.clone()], Vec::new())),
            });
        }
        let mut active = vec![Path {
            nodes: vec![root.clone()],
            edges: Vec::new(),
        }];
        for depth in 1..=max {
            if active.is_empty() {
                break;
            }
            let mut seen = HashSet::new();
            let frontier: Vec<Id> = active
                .iter()
                .map(|p| p.nodes.last().expect("path end").clone())
                .filter(|id| seen.insert(id.clone()))
                .collect();
            self.counters.visited += frontier.len() as u64;
            let adjacent = self.neighbors(hop, &frontier)?;
            let mut next = Vec::new();
            for path in &active {
                let end = path.nodes.last().expect("path end");
                for (neighbor, edge) in adjacent.get(end).into_iter().flatten() {
                    self.expansion()?;
                    if path.nodes.contains(neighbor) || !self.accept(0, hop, neighbor, edge)? {
                        continue;
                    }
                    let mut nodes = path.nodes.clone();
                    nodes.push(neighbor.clone());
                    let mut edges = path.edges.clone();
                    edges.push(edge.clone());
                    self.retain(32 * (nodes.len() + edges.len()) + neighbor.len() + edge.len())?;
                    if depth >= min {
                        self.row_limit(rows.len() + 1)?;
                        rows.push(Row {
                            node: neighbor.clone(),
                            edge: Some(edge.clone()),
                            depth,
                            path: Some((nodes.clone(), edges.clone())),
                        });
                    }
                    if depth < max {
                        if next.len() + rows.len() >= self.limits.max_path_states {
                            return Err(limit("path states", self.limits.max_path_states));
                        }
                        next.push(Path { nodes, edges });
                    }
                }
            }
            active = next;
        }
        Ok(rows)
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2 + 3);
    out.push_str("x'");
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out.push('\'');
    out
}

fn array(ids: &[Id]) -> Result<String> {
    let values = ids
        .iter()
        .map(|id| Value::decode(id))
        .collect::<Result<Vec<_>>>()?;
    Ok(hex(&Value::Array(values).encode()?))
}

static NEXT_RELATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn rows_sql(rows: &[Row], root: &[u8], with_path: bool) -> Result<String> {
    let columns = if with_path {
        "root,node,edge,depth,nodes,edges"
    } else {
        "root,node,edge,depth"
    };
    let values = if rows.is_empty() {
        format!(
            "SELECT {} WHERE 0",
            vec!["NULL"; if with_path { 6 } else { 4 }].join(",")
        )
    } else {
        let root = hex(root);
        let mut tuples = Vec::with_capacity(rows.len());
        for row in rows {
            let mut tuple = format!(
                "({root},{},{},{}",
                hex(&row.node),
                row.edge.as_deref().map_or_else(|| "NULL".into(), hex),
                row.depth
            );
            if let Some((nodes, edges)) = &row.path {
                tuple.push(',');
                tuple.push_str(&array(nodes)?);
                tuple.push(',');
                tuple.push_str(&array(edges)?);
            }
            tuple.push(')');
            tuples.push(tuple);
        }
        format!("VALUES {}", tuples.join(","))
    };
    let name = format!(
        "__romedb_graph_rows_{}",
        NEXT_RELATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    Ok(format!(
        "WITH {name}({columns}) AS MATERIALIZED ({values}) SELECT {columns} FROM {name}"
    ))
}
