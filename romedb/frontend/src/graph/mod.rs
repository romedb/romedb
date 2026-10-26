//! Property graph: edge collections, endpoint integrity and bounded traversal.
mod predicate;
mod query;
pub use query::GraphLimits;
pub(crate) use query::{expand_sources, explaining, ExplainScope, GraphCounters};

use crate::{
    canonical, decode_document, quote, text, Collection, Connection, Document, Error, Result, Value,
};
use serde::{Deserialize, Serialize};
use turso_core::Value as EngineValue;

/// Catalog row that makes a graph-enabled file unreadable by releases before 2.4.0.
/// It stays after the last edge table is dropped because edge data may have been
/// written and older binaries must never mutate endpoint collections afterwards.
pub(crate) const MARKER: &str = "__romedb_graph";
pub(crate) const CATALOG_VERSION: u32 = 6;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Edge {
    pub(crate) source: String,
    pub(crate) target: String,
    #[serde(default, skip_serializing_if = "crate::field_rules::is_false")]
    pub(crate) cascade: bool,
    pub(crate) adjacency: String,
}
impl Edge {
    pub(crate) fn outgoing_index(&self) -> String {
        format!("{}_out", self.adjacency)
    }
    pub(crate) fn incoming_index(&self) -> String {
        format!("{}_in", self.adjacency)
    }
    fn table_ddl(&self) -> String {
        format!(
            "CREATE TABLE {} (edge_id BLOB PRIMARY KEY, source_id BLOB NOT NULL, target_id BLOB NOT NULL)",
            quote(&self.adjacency)
        )
    }
    fn outgoing_ddl(&self) -> String {
        format!(
            "CREATE INDEX {} ON {} (source_id, target_id, edge_id)",
            quote(&self.outgoing_index()),
            quote(&self.adjacency)
        )
    }
    fn incoming_ddl(&self) -> String {
        format!(
            "CREATE INDEX {} ON {} (target_id, source_id, edge_id)",
            quote(&self.incoming_index()),
            quote(&self.adjacency)
        )
    }
    pub(crate) fn schema(&self) -> [(String, &'static str, String); 3] {
        [
            (self.adjacency.clone(), "table", self.table_ddl()),
            (self.outgoing_index(), "index", self.outgoing_ddl()),
            (self.incoming_index(), "index", self.incoming_ddl()),
        ]
    }
}

pub(crate) fn adjacency_storage(name: &str) -> String {
    format!(
        "__romedb_g_{}",
        name.as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

pub(crate) fn marker_metadata() -> String {
    format!(
        r#"{{"version":{CATALOG_VERSION},"name":"{MARKER}","storage":"{MARKER}","fields":[],"indexes":[]}}"#
    )
}

pub(crate) fn validate_version(collection: &Collection) -> Result<()> {
    if (collection.edge.is_some() || !collection.edge_dependents.is_empty())
        && collection.version < CATALOG_VERSION
    {
        return Err(Error::Storage(
            "graph metadata requires catalog version 6".into(),
        ));
    }
    if let Some(edge) = &collection.edge {
        canonical(&edge.source)?;
        canonical(&edge.target)?;
        if edge.adjacency != adjacency_storage(&collection.name) {
            return Err(Error::Storage("edge adjacency identity mismatch".into()));
        }
        if !collection.edge_dependents.is_empty() {
            return Err(Error::Storage(
                "edge tables cannot be graph endpoints".into(),
            ));
        }
    }
    for name in &collection.edge_dependents {
        canonical(name)?;
    }
    Ok(())
}

/// Every edge names existing node collections that list it as a dependent,
/// and every dependent entry is backed by such an edge.
pub(crate) fn validate_catalog(collections: &[Collection], marker: bool) -> Result<()> {
    let by_name: std::collections::BTreeMap<_, _> =
        collections.iter().map(|c| (c.name.as_str(), c)).collect();
    let mut expected = std::collections::BTreeSet::new();
    for c in collections {
        let Some(edge) = &c.edge else { continue };
        if !marker {
            return Err(Error::Storage(
                "edge table without the graph catalog marker".into(),
            ));
        }
        for endpoint in [&edge.source, &edge.target] {
            match by_name.get(endpoint.as_str()) {
                Some(node) if node.edge.is_none() => {}
                _ => {
                    return Err(Error::Storage(format!(
                        "edge table {} has an invalid endpoint collection {endpoint}",
                        c.name
                    )))
                }
            }
            expected.insert((endpoint.clone(), c.name.clone()));
        }
    }
    let actual: std::collections::BTreeSet<_> = collections
        .iter()
        .flat_map(|c| {
            c.edge_dependents
                .iter()
                .map(|edge| (c.name.clone(), edge.clone()))
        })
        .collect();
    if actual != expected
        || collections.iter().any(|c| {
            let mut seen = std::collections::BTreeSet::new();
            c.edge_dependents.iter().any(|name| !seen.insert(name))
        })
    {
        return Err(Error::Storage(
            "graph endpoint dependencies do not match edge declarations".into(),
        ));
    }
    Ok(())
}

pub(crate) fn constraint(message: String) -> Error {
    Error::Engine(turso_core::LimboError::Constraint(message))
}

fn display(record: &crate::Record) -> String {
    match &record.key {
        crate::Key::Integer(key) => format!("{}:{key}", record.table),
        crate::Key::String(key)
            if key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !key.starts_with(|c: char| c.is_ascii_digit()) =>
        {
            format!("{}:{key}", record.table)
        }
        crate::Key::String(key) => format!("{}:`{}`", record.table, key.replace('`', "``")),
    }
}

impl Connection {
    pub fn create_edge_table(
        &self,
        name: &str,
        source: &str,
        target: &str,
        cascade: bool,
        if_not_exists: bool,
    ) -> Result<()> {
        let name = canonical(name)?;
        let source = canonical(source)?;
        let target = canonical(target)?;
        self.atomic(|| {
            match self.catalog(&name) {
                Ok(existing) => {
                    let same = existing.edge.as_ref().is_some_and(|edge| {
                        edge.source == source && edge.target == target && edge.cascade == cascade
                    });
                    return if if_not_exists && same {
                        Ok(())
                    } else {
                        Err(Error::AlreadyExists(name.clone()))
                    };
                }
                Err(Error::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
            let mut endpoints = Vec::new();
            for endpoint in [&source, &target] {
                let collection = self.catalog(endpoint).map_err(|error| match error {
                    Error::NotFound(_) => Error::NotFound(format!(
                        "edge endpoint collection {endpoint}; create it with CREATE TABLE {endpoint}"
                    )),
                    error => error,
                })?;
                if collection.edge.is_some() {
                    return Err(Error::Unsupported(
                        "edge tables cannot be endpoints of other edge tables".into(),
                    ));
                }
                if !endpoints.iter().any(|c: &Collection| c.name == collection.name) {
                    endpoints.push(collection);
                }
            }
            self.create_collection(&name, false)?;
            let mut collection = self.catalog(&name)?;
            let edge = Edge {
                source: source.clone(),
                target: target.clone(),
                cascade,
                adjacency: adjacency_storage(&name),
            };
            for (_, _, ddl) in edge.schema() {
                self.run(&ddl, &[])?;
            }
            collection.edge = Some(edge);
            self.save_catalog(&collection)?;
            for mut endpoint in endpoints {
                endpoint.edge_dependents.push(name.clone());
                self.save_catalog(&endpoint)?;
            }
            self.run(
                "INSERT OR IGNORE INTO __romedb_catalog VALUES (?1, ?2)",
                &[text(MARKER), text(&marker_metadata())],
            )?;
            Ok(())
        })
    }

    /// Remove an edge table's adjacency storage and endpoint registrations.
    /// Caller owns the DROP TABLE savepoint.
    pub(crate) fn drop_edge_metadata(&self, collection: &Collection) -> Result<()> {
        if !collection.edge_dependents.is_empty() {
            return Err(Error::Validation(format!(
                "drop dependent edge tables ({}) before their endpoint collection",
                collection.edge_dependents.join(", ")
            )));
        }
        let Some(edge) = &collection.edge else {
            return Ok(());
        };
        self.run(&format!("DROP TABLE {}", quote(&edge.adjacency)), &[])?;
        for endpoint in [&edge.source, &edge.target] {
            let mut node = self.catalog(endpoint)?;
            if node.edge_dependents.contains(&collection.name) {
                node.edge_dependents.retain(|name| name != &collection.name);
                self.save_catalog(&node)?;
            }
        }
        Ok(())
    }

    pub(crate) fn graph_marker_present(&self) -> Result<bool> {
        let rows = self.run(
            "SELECT metadata FROM __romedb_catalog WHERE name=?1",
            &[text(MARKER)],
        )?;
        match rows.as_slice() {
            [] => Ok(false),
            [row] if matches!(row.as_slice(), [EngineValue::Text(t)] if t.as_str() == marker_metadata()) => {
                Ok(true)
            }
            _ => Err(Error::Storage("invalid graph catalog marker".into())),
        }
    }

    /// Fill system fields omitted by CONTENT/PATCH replacement of an existing edge.
    pub(crate) fn graph_restore_endpoints(&self, c: &Collection, doc: &mut Document) -> Result<()> {
        if c.edge.is_none() || (doc.contains_key("source") && doc.contains_key("target")) {
            return Ok(());
        }
        let Some(Value::Record(id)) = doc.get("id") else {
            return Ok(());
        };
        if let Some(before) = self.get_in(c, &id.clone())? {
            for field in ["source", "target"] {
                if !doc.contains_key(field) {
                    if let Some(value) = before.get(field) {
                        doc.insert(field.into(), value.clone());
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn graph_normalize_edge(&self, c: &Collection, doc: &mut Document) -> Result<()> {
        let Some(edge) = &c.edge else {
            return Ok(());
        };
        for (field, table) in [("source", &edge.source), ("target", &edge.target)] {
            let Some(Value::Record(record)) = doc.get(field) else {
                return Err(Error::Validation(format!(
                    "edge {} requires a typed record {field} in {table}",
                    c.name
                )));
            };
            let normalized = crate::normalized_id(record, table).map_err(|_| {
                Error::Validation(format!(
                    "edge {} field {field} must reference collection {table}",
                    c.name
                ))
            })?;
            doc.insert(field.into(), Value::Record(normalized));
        }
        Ok(())
    }

    /// Check both endpoints inside the caller's write transaction. In MVCC mode the
    /// endpoint rows are rewritten so a concurrent endpoint delete conflicts.
    pub(crate) fn graph_check_new_edge(&self, c: &Collection, doc: &Document) -> Result<()> {
        let Some(edge) = &c.edge else {
            return Ok(());
        };
        let mut doc = doc.clone();
        self.graph_normalize_edge(c, &mut doc)?;
        for (field, table) in [("source", &edge.source), ("target", &edge.target)] {
            let Value::Record(record) = &doc[field] else {
                unreachable!("normalized endpoint");
            };
            let node = self.catalog(table)?;
            let encoded = EngineValue::Blob(Value::Record(record.clone()).encode()?);
            let exists = if self.engine.mvcc_enabled() {
                self.run_customer(
                    &format!(
                        "UPDATE {} SET doc = doc WHERE id = ?1 RETURNING 1",
                        quote(&node.storage)
                    ),
                    &[encoded],
                )?
            } else {
                self.run_customer(
                    &format!("SELECT 1 FROM {} WHERE id = ?1", quote(&node.storage)),
                    &[encoded],
                )?
            };
            if exists.is_empty() {
                return Err(constraint(format!(
                    "edge {} {field} {} does not exist",
                    c.name,
                    display(record)
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn graph_link_edge(&self, c: &Collection, doc: &Document) -> Result<()> {
        let Some(edge) = &c.edge else {
            return Ok(());
        };
        self.run_index_maintenance(
            &format!("INSERT INTO {} VALUES (?1, ?2, ?3)", quote(&edge.adjacency)),
            &[
                EngineValue::Blob(doc["id"].encode()?),
                EngineValue::Blob(doc["source"].encode()?),
                EngineValue::Blob(doc["target"].encode()?),
            ],
        )?;
        Ok(())
    }

    /// Reject changed endpoints when an existing edge document is rewritten.
    pub(crate) fn graph_check_edge_update(&self, c: &Collection, doc: &Document) -> Result<()> {
        if c.edge.is_none() {
            return Ok(());
        }
        let mut doc = doc.clone();
        self.graph_normalize_edge(c, &mut doc)?;
        let Some(Value::Record(id)) = doc.get("id") else {
            return Err(Error::Validation("id must be a typed record".into()));
        };
        let before = self
            .get_in(c, id)?
            .ok_or_else(|| Error::Storage("updated edge is missing".into()))?;
        if before.get("source") != doc.get("source") || before.get("target") != doc.get("target") {
            return Err(Error::Validation(format!(
                "edge {} source and target are immutable; delete and recreate the edge",
                c.name
            )));
        }
        Ok(())
    }

    /// Remove adjacency for a deleted edge, or apply endpoint deletion policies for a
    /// deleted node. Caller owns the deletion savepoint.
    pub(crate) fn graph_delete(&self, c: &Collection, doc: &Document) -> Result<()> {
        let id = EngineValue::Blob(doc["id"].encode()?);
        if let Some(edge) = &c.edge {
            self.run_index_maintenance(
                &format!("DELETE FROM {} WHERE edge_id = ?1", quote(&edge.adjacency)),
                &[id],
            )?;
            return Ok(());
        }
        if c.edge_dependents.is_empty() {
            return Ok(());
        }
        let mut cascades = Vec::new();
        for name in &c.edge_dependents {
            let edges = self.catalog(name)?;
            let edge = edges
                .edge
                .clone()
                .ok_or_else(|| Error::Storage(format!("{name} is not an edge table")))?;
            let mut incident = std::collections::BTreeSet::new();
            for (column, applies) in [
                ("source_id", edge.source == c.name),
                ("target_id", edge.target == c.name),
            ] {
                if !applies {
                    continue;
                }
                let limit = if edge.cascade { "" } else { " LIMIT 1" };
                for row in self.run_customer(
                    &format!(
                        "SELECT edge_id FROM {} WHERE {column} = ?1{limit}",
                        quote(&edge.adjacency)
                    ),
                    &[id.clone()],
                )? {
                    if let Some(EngineValue::Blob(edge_id)) = row.into_iter().next() {
                        incident.insert(edge_id);
                    }
                }
            }
            if incident.is_empty() {
                continue;
            }
            if !edge.cascade {
                let Some(Value::Record(record)) = doc.get("id") else {
                    unreachable!("stored document id");
                };
                return Err(constraint(format!(
                    "{} has {name} edges and {name} uses ON DELETE RESTRICT",
                    display(record)
                )));
            }
            cascades.push((edges, incident));
        }
        for (edges, incident) in cascades {
            for edge_id in incident {
                let rows = self.run_customer(
                    &format!("SELECT doc FROM {} WHERE id = ?1", quote(&edges.storage)),
                    &[EngineValue::Blob(edge_id)],
                )?;
                let Some(row) = rows.first() else {
                    return Err(Error::Storage("adjacency refers to a missing edge".into()));
                };
                let edge_doc = decode_document(&row[0])?;
                self.delete_document(&edges, &edge_doc)?;
            }
        }
        Ok(())
    }

    /// Build the edge document for RELATE from its endpoints and property payload.
    pub(crate) fn relate_document(
        &self,
        source: romeql_parser::Expr,
        edge: &str,
        id: Option<crate::Record>,
        target: romeql_parser::Expr,
        value: Option<romeql_parser::Expr>,
        params: &crate::Parameters,
    ) -> Result<Document> {
        let collection = self.catalog(edge)?;
        if collection.edge.is_none() {
            return Err(Error::Validation(format!(
                "RELATE requires an edge table; declare {} with CREATE EDGE TABLE",
                collection.name
            )));
        }
        let mut doc = match value {
            None => Document::new(),
            Some(value) => match self.evaluate(value, params, None)? {
                Value::Object(doc) => doc,
                _ => {
                    return Err(Error::Validation(
                        "RELATE properties must be an object".into(),
                    ))
                }
            },
        };
        for field in ["id", "source", "target"] {
            if doc.contains_key(field) {
                return Err(Error::Validation(format!(
                    "RELATE properties cannot contain {field}"
                )));
            }
        }
        for (field, expr) in [("source", source), ("target", target)] {
            match self.evaluate(expr, params, None)? {
                Value::Record(record) => {
                    doc.insert(field.into(), Value::Record(record));
                }
                _ => {
                    return Err(Error::Validation(format!(
                        "RELATE {field} must be a typed record"
                    )))
                }
            }
        }
        if let Some(id) = id {
            doc.insert("id".into(), Value::Record(id));
        }
        Ok(doc)
    }

    /// Reject DEFINE FIELD on edge system fields.
    pub(crate) fn graph_check_field(&self, table: &str, path: &[String]) -> Result<()> {
        if matches!(path.first().map(String::as_str), Some("source" | "target"))
            && self.catalog(table).is_ok_and(|c| c.edge.is_some())
        {
            return Err(Error::Validation(
                "edge source and target are system fields".into(),
            ));
        }
        Ok(())
    }

    /// Compare adjacency with edge documents and endpoints; return the entry count.
    pub(crate) fn graph_integrity(&self, c: &Collection) -> Result<u64> {
        let Some(edge) = &c.edge else {
            return Ok(0);
        };
        let count = |sql: String| -> Result<i64> {
            match self.run(&sql, &[])?.first().and_then(|row| row.first()) {
                Some(EngineValue::Numeric(turso_core::Numeric::Integer(n))) => Ok(*n),
                _ => Err(Error::Storage("invalid graph integrity count".into())),
            }
        };
        let fail = |what: &str| Err(Error::Storage(format!("edge table {} {what}", c.name)));
        if count(format!(
            "SELECT count(*) FROM {} AS a LEFT JOIN {} AS d ON d.id = a.edge_id WHERE d.id IS NULL",
            quote(&edge.adjacency),
            quote(&c.storage)
        ))? != 0
        {
            return fail("has orphan adjacency entries");
        }
        for (column, table) in [("source_id", &edge.source), ("target_id", &edge.target)] {
            let node = self.catalog(table)?;
            if count(format!(
                "SELECT count(*) FROM {} AS a LEFT JOIN {} AS n ON n.id = a.{column} WHERE n.id IS NULL",
                quote(&edge.adjacency),
                quote(&node.storage)
            ))? != 0
            {
                return fail(&format!("references missing {table} records"));
            }
        }
        let mut entries = 0u64;
        for row in self.run(
            &format!(
                "SELECT d.doc, a.source_id, a.target_id FROM {} AS d JOIN {} AS a ON a.edge_id = d.id",
                quote(&c.storage),
                quote(&edge.adjacency)
            ),
            &[],
        )? {
            let doc = decode_document(&row[0])?;
            for (field, stored) in [("source", &row[1]), ("target", &row[2])] {
                let expected = doc.get(field).map(Value::encode).transpose()?;
                if !matches!(stored, EngineValue::Blob(bytes) if expected.as_deref() == Some(bytes.as_slice()))
                {
                    return fail("has stale adjacency entries");
                }
            }
            entries += 1;
        }
        Ok(entries)
    }
}
