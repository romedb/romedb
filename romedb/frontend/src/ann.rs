//! Dense HNSW indexes persisted in the engine transaction, with bounded redo logs.
use crate::{
    canonical, quote, text, Collection, Connection, Document, EngineValue, Error, Index, IndexKind,
    QueryResult, Result, Value,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use usearch::{Index as Graph, IndexOptions, MetricKind, ScalarKind};

const FORMAT: &str = "usearch-2.26.2-f32-v1";
const FORMAT_F16: &str = "usearch-2.26.2-f16-v1";
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct VectorIndexOptions {
    pub dimensions: usize,
    pub metric: String,
    pub quantization: String,
}
impl Default for VectorIndexOptions {
    fn default() -> Self {
        Self {
            dimensions: 0,
            metric: "cosine".into(),
            quantization: "f32".into(),
        }
    }
}
const CHECKPOINT_EVENTS: usize = 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub dimensions: usize,
    pub metric: String,
    pub format: String,
}
fn invalid(message: &str) -> Error {
    Error::Validation(format!("vector index: {message}"))
}
fn stored(message: &str) -> Error {
    Error::Storage(format!("vector index: {message}"))
}
fn native(error: impl std::fmt::Display) -> Error {
    stored(&error.to_string())
}
fn integer(value: &EngineValue) -> Result<i64> {
    match value {
        EngineValue::Numeric(turso_core::Numeric::Integer(n)) => Ok(*n),
        _ => Err(stored("expected integer")),
    }
}
fn string(value: &EngineValue) -> Result<String> {
    match value {
        EngineValue::Text(t) => Ok(t.as_str().into()),
        _ => Err(stored("expected generation token")),
    }
}
impl Config {
    pub(crate) fn compressed(&self) -> bool {
        self.format == FORMAT_F16
    }
    pub(crate) fn quantization(&self) -> &'static str {
        if self.compressed() {
            "f16"
        } else {
            "f32"
        }
    }
    fn scalar_kind(&self) -> ScalarKind {
        if self.compressed() {
            ScalarKind::F16
        } else {
            ScalarKind::F32
        }
    }
    fn validate(&self) -> Result<()> {
        if !(1..=4096).contains(&self.dimensions)
            || !matches!(self.metric.as_str(), "cosine" | "l2")
            || !matches!(self.format.as_str(), FORMAT | FORMAT_F16)
        {
            return Err(invalid(
                "dimensions must be 1..4096, metric cosine or l2, and format supported",
            ));
        }
        Ok(())
    }
    fn graph(&self) -> Result<Graph> {
        self.validate()?;
        Graph::new(&IndexOptions {
            dimensions: self.dimensions,
            metric: if self.metric == "cosine" {
                MetricKind::Cos
            } else {
                MetricKind::L2sq
            },
            quantization: self.scalar_kind(),
            connectivity: 32,
            expansion_add: 200,
            expansion_search: 512,
            multi: false,
        })
        .map_err(native)
    }
    fn raw_components(&self, value: &Value) -> Result<Vec<f32>> {
        let Value::Vector(bytes) = value else {
            return Err(invalid("expected vector32 value"));
        };
        if crate::vectors::dimensions(bytes)? != self.dimensions {
            return Err(invalid("dimension mismatch"));
        }
        let bytes = if bytes.len() % 2 == 1 && bytes.last() == Some(&1) {
            &bytes[..bytes.len() - 1]
        } else {
            bytes.as_slice()
        };
        if bytes.len() != self.dimensions * 4 {
            return Err(invalid("only vector32 encoding is supported by this index"));
        }
        let values: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().expect("component width")))
            .collect();
        Ok(values)
    }
    fn components(&self, value: &Value) -> Result<Vec<f32>> {
        let values = self.raw_components(value)?;
        if self.metric == "cosine" {
            let norm = values
                .iter()
                .map(|x| (*x as f64).powi(2))
                .sum::<f64>()
                .sqrt();
            if norm == 0.0 {
                return Err(invalid("cosine vectors must have nonzero norm"));
            }
            Ok(values
                .into_iter()
                .map(|x| (x as f64 / norm) as f32)
                .collect())
        } else {
            if values.iter().any(|x| x.abs() > 1e15) {
                return Err(invalid("l2 components must have magnitude at most 1e15"));
            }
            if self.compressed() && values.iter().any(|x| x.abs() > 65504.0) {
                return Err(invalid(
                    "f16 l2 components must have magnitude at most 65504",
                ));
            }
            Ok(values)
        }
    }
}
impl Index {
    fn config(&self) -> Result<&Config> {
        self.vector
            .as_ref()
            .ok_or_else(|| stored("missing configuration"))
    }
    pub(crate) fn validate_vector_config(&self, collection: &Collection) -> Result<()> {
        match (&self.kind, &self.vector) {
            (IndexKind::Vector, Some(config)) if !self.unique => {
                config.validate()?;
                for field in &collection.fields {
                    if field.path == self.path
                        && !matches!(field.kind, crate::FieldType::Vector(n) if n==config.dimensions)
                    {
                        return Err(invalid("field dimension disagrees with index"));
                    }
                }
                Ok(())
            }
            (IndexKind::Vector, _) => Err(invalid("invalid vector definition")),
            (_, None) => Ok(()),
            _ => Err(invalid("vector configuration on another index kind")),
        }
    }
    pub(crate) fn vector_keys(&self, value: &Value) -> Result<Vec<EngineValue>> {
        if matches!(value, Value::Null) {
            return Ok(vec![EngineValue::Null]);
        }
        self.config()?.components(value)?;
        Ok(vec![EngineValue::Blob(value.encode()?)])
    }
    pub(crate) fn ann_state(&self) -> String {
        format!("{}_ann_state", self.storage)
    }
    pub(crate) fn ann_log(&self) -> String {
        format!("{}_ann_log", self.storage)
    }
    pub(crate) fn ann_state_ddl(&self) -> String {
        format!("CREATE TABLE {} (slot INTEGER PRIMARY KEY CHECK(slot=1), generation TEXT NOT NULL, graph BLOB NOT NULL, digest BLOB NOT NULL CHECK(length(digest)=32))",quote(&self.ann_state()))
    }
    pub(crate) fn ann_log_ddl(&self) -> String {
        format!("CREATE TABLE {} (seq INTEGER PRIMARY KEY, token TEXT NOT NULL, node INTEGER NOT NULL CHECK(node>0), value BLOB)",quote(&self.ann_log()))
    }
}
pub(crate) struct Cache {
    config: Config,
    storage: String,
    generation: String,
    seq: i64,
    token: String,
    graph: Graph,
}
fn reserve_one(graph: &Graph) -> Result<()> {
    if graph.capacity() <= graph.size() {
        let capacity = graph
            .size()
            .checked_add(1)
            .and_then(usize::checked_next_power_of_two)
            .ok_or_else(|| Error::Limit("vector index capacity overflow".into()))?;
        graph
            .reserve_capacity_and_threads(capacity, 1)
            .map_err(native)?;
    }
    Ok(())
}
fn snapshot(graph: &Graph) -> Result<Vec<u8>> {
    let mut bytes = vec![0; graph.serialized_length()];
    graph.save_to_buffer(&mut bytes).map_err(native)?;
    Ok(bytes)
}
impl Connection {
    pub(crate) fn discard_ann_cache(&self, index: &Index) -> Result<()> {
        let mut cache = self
            .ann_cache
            .lock()
            .map_err(|_| stored("cache mutex poisoned"))?;
        if cache.as_ref().is_some_and(|c| c.storage == index.storage) {
            *cache = None;
        }
        Ok(())
    }
    fn ann_boundary(&self) -> Result<()> {
        if self.engine.is_interrupted() || self.engine.should_interrupt_for_progress(0, 1000) {
            return Err(Error::Engine(turso_core::LimboError::Interrupt));
        }
        Ok(())
    }
    pub(crate) fn ann_generation(&self, index: &Index) -> Result<String> {
        let rows = self.run(
            &format!(
                "SELECT generation FROM {} WHERE slot=1",
                quote(&index.ann_state())
            ),
            &[],
        )?;
        match rows.as_slice() {
            [row] if row.len() == 1 => string(&row[0]),
            _ => Err(stored("missing graph state")),
        }
    }
    fn with_ann_graph<T>(
        &self,
        index: &Index,
        operation: impl FnOnce(&Graph) -> Result<T>,
    ) -> Result<T> {
        let generation = self.ann_generation(index)?;
        let mut slot = self
            .ann_cache
            .lock()
            .map_err(|_| stored("cache mutex poisoned"))?;
        let mut cache = slot.take();
        // A sequence number can be reused after rollback. Its random token must
        // also match, and checkpoints have their own transactionally stored token.
        let compatible = if let Some(c) = &cache {
            c.storage == index.storage
                && c.config == *index.config()?
                && c.generation == generation
                && (c.seq == 0 || {
                    let rows = self.run(
                        &format!("SELECT token FROM {} WHERE seq=?1", quote(&index.ann_log())),
                        &[EngineValue::from_i64(c.seq)],
                    )?;
                    matches!(rows.as_slice(), [row] if row.len()==1 && string(&row[0])?==c.token)
                })
        } else {
            false
        };
        if !compatible {
            let rows = self.run(
                &format!(
                    "SELECT graph,digest FROM {} WHERE slot=1",
                    quote(&index.ann_state())
                ),
                &[],
            )?;
            let Some(EngineValue::Blob(bytes)) = rows.first().and_then(|r| r.first()) else {
                return Err(stored("missing graph bytes"));
            };
            if !matches!(rows[0].get(1), Some(EngineValue::Blob(digest)) if digest.as_slice()==Sha256::digest(bytes).as_slice())
            {
                return Err(stored("graph checksum mismatch"));
            }
            let graph = index.config()?.graph()?;
            graph.load_from_buffer(bytes).map_err(native)?;
            if graph.dimensions() != index.config()?.dimensions
                || graph.scalar_kind() != index.config()?.scalar_kind()
                || graph.multi()
                || graph.metric_kind()
                    != if index.config()?.metric == "cosine" {
                        MetricKind::Cos
                    } else {
                        MetricKind::L2sq
                    }
            {
                return Err(stored("graph metadata mismatch"));
            }
            graph
                .reserve_capacity_and_threads(graph.capacity(), 1)
                .map_err(native)?;
            graph.change_expansion_add(200);
            graph.change_expansion_search(512);
            cache = Some(Cache {
                config: index.config()?.clone(),
                storage: index.storage.clone(),
                generation,
                seq: 0,
                token: String::new(),
                graph,
            });
        }
        let mut cache = cache.expect("initialized graph");
        let events = self.run(
            &format!(
                "SELECT seq,token,node,value FROM {} WHERE seq>?1 ORDER BY seq LIMIT 1025",
                quote(&index.ann_log())
            ),
            &[EngineValue::from_i64(cache.seq)],
        )?;
        if events.len() > CHECKPOINT_EVENTS {
            return Err(stored("redo log exceeds checkpoint bound"));
        }
        for row in events {
            self.ann_boundary()?;
            let [seq, token, node, value] = row.as_slice() else {
                return Err(stored("invalid redo event"));
            };
            let node = integer(node)?;
            if node <= 0 {
                return Err(stored("invalid graph node"));
            }
            cache.graph.remove(node as u64).map_err(native)?;
            match value {
                EngineValue::Null => {}
                EngineValue::Blob(bytes) => {
                    let value = Value::decode(bytes)?;
                    let values = index.config()?.components(&value)?;
                    reserve_one(&cache.graph)?;
                    cache.graph.add(node as u64, &values).map_err(native)?;
                }
                _ => return Err(stored("invalid redo value")),
            }
            cache.seq = integer(seq)?;
            cache.token = string(token)?;
        }
        self.ann_boundary()?;
        let result = operation(&cache.graph)?;
        self.ann_boundary()?;
        *slot = Some(cache);
        Ok(result)
    }
    fn ann_append(&self, index: &Index, node: i64, value: EngineValue) -> Result<()> {
        self.run_index_maintenance(
            &format!(
                "INSERT INTO {} (token,node,value) VALUES (uuid7_str(),?1,?2)",
                quote(&index.ann_log())
            ),
            &[EngineValue::from_i64(node), value],
        )?;
        let rows = self.run_index_maintenance(
            &format!("SELECT count(*) FROM {}", quote(&index.ann_log())),
            &[],
        )?;
        if integer(&rows[0][0])? as usize >= CHECKPOINT_EVENTS {
            let bytes = self.with_ann_graph(index, |graph| {
                graph.compact().map_err(native)?;
                snapshot(graph)
            })?;
            self.run_index_maintenance(
                &format!(
                    "UPDATE {} SET generation=uuid7_str(),graph=?1,digest=?2 WHERE slot=1",
                    quote(&index.ann_state())
                ),
                &[
                    EngineValue::Blob(bytes.clone()),
                    EngineValue::Blob(Sha256::digest(&bytes).to_vec()),
                ],
            )?;
            self.run_index_maintenance(&format!("DELETE FROM {}", quote(&index.ann_log())), &[])?;
        }
        Ok(())
    }
    pub(crate) fn insert_vector_entry(&self, index: &Index, doc: &Document) -> Result<()> {
        let key = index.document_keys(doc)?.remove(0);
        let id = doc
            .get("id")
            .ok_or_else(|| stored("missing record id"))?
            .encode()?;
        let rows = self.run_index_maintenance(
            &format!(
                "INSERT INTO {} VALUES (?1,?2) RETURNING rowid",
                quote(&index.storage)
            ),
            &[key.clone(), EngineValue::Blob(id)],
        )?;
        if !matches!(key, EngineValue::Null) {
            self.ann_append(index, integer(&rows[0][0])?, key)?;
        }
        Ok(())
    }
    pub(crate) fn delete_vector_entry(&self, index: &Index, id: &EngineValue) -> Result<()> {
        let rows = self.run_index_maintenance(
            &format!(
                "DELETE FROM {} WHERE id=?1 RETURNING rowid,\"key\"",
                quote(&index.storage)
            ),
            std::slice::from_ref(id),
        )?;
        for row in rows {
            if !matches!(row[1], EngineValue::Null) {
                self.ann_append(index, integer(&row[0])?, EngineValue::Null)?;
            }
        }
        Ok(())
    }
    /// Create a dense float32 HNSW index. Metrics are `cosine` and Euclidean `l2`.
    pub fn create_vector_index(
        &self,
        table: &str,
        name: &str,
        path: Vec<String>,
        dimensions: usize,
        metric: &str,
        if_not_exists: bool,
    ) -> Result<()> {
        self.create_vector_index_with_options(
            table,
            name,
            path,
            VectorIndexOptions {
                dimensions,
                metric: metric.into(),
                ..Default::default()
            },
            if_not_exists,
        )
    }
    pub fn create_vector_index_with_options(
        &self,
        table: &str,
        name: &str,
        path: Vec<String>,
        options: VectorIndexOptions,
        if_not_exists: bool,
    ) -> Result<()> {
        crate::validate_path(&path)?;
        let name = canonical(name)?;
        let config = Config {
            dimensions: options.dimensions,
            metric: options.metric,
            format: match options.quantization.to_ascii_lowercase().as_str() {
                "f32" => FORMAT,
                "f16" => FORMAT_F16,
                _ => return Err(invalid("quantization must be f32 or f16")),
            }
            .into(),
        };
        config.validate()?;
        let index = Index {
            kind: IndexKind::Vector,
            name: name.clone(),
            path,
            unique: false,
            storage: format!(
                "__romedb_i_{}",
                name.as_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ),
            fulltext: None,
            vector: Some(config),
            scalar: None,
            spatial: None,
        };
        self.atomic(||{
            let mut collection=self.catalog(table)?;
            if let Some(old)=collection.indexes.iter().find(|i|i.name==name) {
                if if_not_exists && old.kind==index.kind && old.path==index.path && old.vector==index.vector {return Ok(());}
                return Err(Error::AlreadyExists(name.clone()));
            }
            if !self.run("SELECT name FROM sqlite_schema WHERE name=?1 COLLATE NOCASE UNION ALL SELECT name FROM __romedb_catalog WHERE name=?1",&[text(&name)])?.is_empty(){return Err(Error::AlreadyExists(name.clone()));}
            crate::catalog::compatible_index(&collection,&index.path,index.kind)?; index.validate_vector_config(&collection)?;
            self.build_vector_storage(&index, &self.documents(&collection)?)?;
            collection.indexes.push(index.clone()); self.save_catalog(&collection)
        })
    }
    pub(crate) fn build_vector_storage(&self, index: &Index, documents: &[Document]) -> Result<()> {
        for ddl in [
            index.table_ddl(),
            index.index_ddl(),
            index.ann_state_ddl(),
            index.ann_log_ddl(),
        ] {
            self.run(&ddl, &[])?;
        }
        let graph = index.config()?.graph()?;
        graph
            .reserve_capacity_and_threads(documents.len().max(1), 1)
            .map_err(native)?;
        for doc in documents {
            self.ann_boundary()?;
            let key = index.document_keys(doc)?.remove(0);
            let id = doc
                .get("id")
                .ok_or_else(|| stored("missing id"))?
                .encode()?;
            let rows = self.run_index_maintenance(
                &format!(
                    "INSERT INTO {} VALUES (?1,?2) RETURNING rowid",
                    quote(&index.storage)
                ),
                &[key, EngineValue::Blob(id)],
            )?;
            if let Some(value) = crate::path_value(doc, &index.path)? {
                if !matches!(value, Value::Null) {
                    let values = index.config()?.components(value)?;
                    graph
                        .add(integer(&rows[0][0])? as u64, &values)
                        .map_err(native)?;
                }
            }
        }
        let bytes = snapshot(&graph)?;
        let digest = Sha256::digest(&bytes).to_vec();
        self.run_index_maintenance(
            &format!(
                "INSERT INTO {} VALUES (1,uuid7_str(),?1,?2)",
                quote(&index.ann_state())
            ),
            &[EngineValue::Blob(bytes), EngineValue::Blob(digest)],
        )?;
        Ok(())
    }
    pub(crate) fn audit_vector_index(&self, index: &Index) -> Result<()> {
        *self
            .ann_cache
            .lock()
            .map_err(|_| stored("cache mutex poisoned"))? = None;
        self.with_ann_graph(index, |graph| {
            let rows = self.run(
                &format!(
                    "SELECT rowid,\"key\" FROM {} WHERE \"key\" IS NOT NULL",
                    quote(&index.storage)
                ),
                &[],
            )?;
            if rows.len() != graph.size() {
                return Err(stored("graph cardinality mismatch"));
            }
            for row in rows {
                self.ann_boundary()?;
                let [node, EngineValue::Blob(value)] = row.as_slice() else {
                    return Err(stored("invalid graph entry"));
                };
                let mut expected = index.config()?.components(&Value::decode(value)?)?;
                if index.config()?.compressed() {
                    for component in &mut expected {
                        *component = half::f16::from_f32(*component).to_f32();
                    }
                }
                let mut actual = vec![0f32; expected.len()];
                if graph
                    .get(integer(node)? as u64, &mut actual)
                    .map_err(native)?
                    != 1
                    || actual != expected
                {
                    return Err(stored("stale or missing graph entry"));
                }
            }
            Ok(())
        })
    }
    pub(crate) fn vector_search_sql(
        &self,
        name: &Value,
        query: &Value,
        limit: &Value,
        filter: Option<&Value>,
    ) -> Result<String> {
        let Value::String(name) = name else {
            return Err(invalid("index name must be a string"));
        };
        let limit = match limit {
            Value::Integer(n) => *n as f64,
            Value::Number(n) => *n,
            _ => return Err(invalid("limit must be integer")),
        };
        if !(0.0..=10_000.0).contains(&limit) || limit.fract() != 0.0 {
            return Err(invalid("limit must be 0..10000"));
        }
        let hits = self.search_vectors_inner(
            name,
            query,
            limit as usize,
            filter.map(crate::search_filter::Input::Value),
        )?;
        let values = if hits.rows.is_empty() {
            "SELECT NULL,NULL WHERE 0".into()
        } else {
            let rows = hits
                .rows
                .into_iter()
                .map(|row| {
                    let [id, Value::Number(distance)] = row.as_slice() else {
                        return Err(stored("invalid ranked hit"));
                    };
                    let id = id
                        .encode()?
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>();
                    Ok(format!("(x'{id}',{distance:?})"))
                })
                .collect::<Result<Vec<_>>>()?;
            format!("VALUES {}", rows.join(","))
        };
        Ok(format!("WITH __romedb_ann_hnsw_hits(id,distance) AS MATERIALIZED ({values}) SELECT id,distance FROM __romedb_ann_hnsw_hits"))
    }
    /// Search an ANN index. Outer application filters apply after this ranked slice.
    /// Ties are stable within the approximate candidate set, not globally exact.
    pub fn search_vectors(&self, name: &str, query: &Value, limit: usize) -> Result<QueryResult> {
        self.search_vectors_inner(name, query, limit, None)
    }
    pub fn search_vectors_filtered(
        &self,
        name: &str,
        query: &Value,
        limit: usize,
        allowed: &[crate::Record],
    ) -> Result<QueryResult> {
        self.search_vectors_inner(
            name,
            query,
            limit,
            Some(crate::search_filter::Input::Records(allowed)),
        )
    }
    fn search_vectors_inner(
        &self,
        name: &str,
        query: &Value,
        limit: usize,
        filter: Option<crate::search_filter::Input<'_>>,
    ) -> Result<QueryResult> {
        if limit > 10_000 {
            return Err(invalid("result limit exceeds 10000"));
        }
        let name = canonical(name)?;
        self.atomic(|| {
            let (table, index) = self
                .collections()?
                .into_iter()
                .find_map(|collection| {
                    collection
                        .indexes
                        .into_iter()
                        .find(|index| index.name == name)
                        .map(|index| (collection.name, index))
                })
                .ok_or_else(|| Error::NotFound(format!("vector index {name}")))?;
            if index.kind != IndexKind::Vector {
                return Err(invalid("search requires a vector index"));
            }
            let values = index.config()?.components(query)?;
            let original_query = index.config()?.raw_components(query)?;
            let filter = filter.map(|filter| filter.resolve(&table)).transpose()?;
            let allowed = filter
                .as_ref()
                .map(|filter| filter.nodes(self, &index))
                .transpose()?;
            let mut rows = Vec::new();
            if limit > 0 && allowed.as_ref().is_none_or(|nodes| !nodes.is_empty()) {
                let hits = self.with_ann_graph(&index, |graph| {
                    let count = limit.saturating_mul(4).min(graph.size());
                    match &allowed {
                        Some(allowed) => {
                            graph.filtered_search(&values, count, |node| allowed.contains(&node))
                        }
                        None => graph.search(&values, count),
                    }
                    .map_err(native)
                })?;
                let mut ordered = Vec::new();
                for node in hits.keys {
                    // Candidate records are customer index reads, even though
                    // SQL lowering fetches them before the outer hit query runs.
                    // Graph loading/replay/traversal remain outside this meter.
                    let records = self.run_customer(
                        &format!(
                            "SELECT id,\"key\" FROM {} WHERE rowid=?1",
                            quote(&index.storage)
                        ),
                        &[EngineValue::from_i64(
                            i64::try_from(node).map_err(|_| stored("node overflow"))?,
                        )],
                    )?;
                    let [record] = records.as_slice() else {
                        return Err(stored("graph node has no record"));
                    };
                    let [EngineValue::Blob(id), EngineValue::Blob(vector)] = record.as_slice()
                    else {
                        return Err(stored("invalid graph record"));
                    };
                    let point = index.config()?.raw_components(&Value::decode(vector)?)?;
                    let distance = if index.config()?.metric == "cosine" {
                        let dot = point
                            .iter()
                            .zip(&original_query)
                            .map(|(a, b)| *a as f64 * *b as f64)
                            .sum::<f64>();
                        let na = point
                            .iter()
                            .map(|x| (*x as f64).powi(2))
                            .sum::<f64>()
                            .sqrt();
                        let nb = original_query
                            .iter()
                            .map(|x| (*x as f64).powi(2))
                            .sum::<f64>()
                            .sqrt();
                        (1.0 - dot / (na * nb)).clamp(0.0, 2.0)
                    } else {
                        point
                            .iter()
                            .zip(&original_query)
                            .map(|(a, b)| (*a as f64 - *b as f64).powi(2))
                            .sum::<f64>()
                            .sqrt()
                    };
                    if !distance.is_finite() {
                        return Err(stored("non-finite candidate distance"));
                    }
                    ordered.push((distance, id.clone()));
                }
                ordered.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
                for (distance, id) in ordered.into_iter().take(limit) {
                    rows.push(vec![Value::decode(&id)?, Value::Number(distance)]);
                }
            }
            Ok(QueryResult {
                columns: vec!["id".into(), "distance".into()],
                rows,
                affected: 0,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ann_metadata_checksum_and_graph_audit_reject_corruption() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.execute(
            "INSERT INTO items {id:items:a,v:vector32('[1,0]')}",
            &Default::default(),
        )
        .unwrap();
        c.create_vector_index("items", "vec", vec!["v".into()], 2, "cosine", false)
            .unwrap();
        let collection = c.catalog("items").unwrap();
        let index = &collection.indexes[0];
        let metadata = serde_json::to_value(&collection).unwrap();
        for (path, value) in [
            ("/indexes/0/vector/format", serde_json::json!("unknown")),
            ("/indexes/0/vector/dimensions", serde_json::json!(0)),
            ("/indexes/0/unique", serde_json::json!(true)),
            ("/indexes/0/vector/metric", serde_json::json!("ip")),
        ] {
            let mut bad = metadata.clone();
            *bad.pointer_mut(path).unwrap() = value;
            assert!(crate::catalog::decode(&bad.to_string(), "items").is_err());
        }
        c.audit_vector_index(index).unwrap();
        c.run("SAVEPOINT corrupt", &[]).unwrap();
        c.run(
            &format!("UPDATE {} SET graph=x'010203'", quote(&index.ann_state())),
            &[],
        )
        .unwrap();
        assert!(c
            .audit_vector_index(index)
            .unwrap_err()
            .to_string()
            .contains("checksum"));
        c.run("ROLLBACK TO corrupt", &[]).unwrap();
        c.run("RELEASE corrupt", &[]).unwrap();
        c.audit_vector_index(index).unwrap();
        c.run(
            &format!("UPDATE {} SET \"key\"=?1", quote(&index.storage)),
            &[EngineValue::Blob(
                Value::vector32(&[0., 1.]).unwrap().encode().unwrap(),
            )],
        )
        .unwrap();
        assert!(c
            .audit_vector_index(index)
            .unwrap_err()
            .to_string()
            .contains("stale or missing"));
    }
    #[test]
    fn interrupted_ann_build_restores_prior_transaction_work() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE items", &Default::default())
            .unwrap();
        c.run("BEGIN", &[]).unwrap();
        for n in 0..100 {
            c.execute(
                &format!("INSERT INTO items {{v:vector32('[{n},1]')}}"),
                &Default::default(),
            )
            .unwrap();
        }
        let count = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        c.engine.set_progress_handler(
            1,
            Some(Box::new(move || seen.fetch_add(1, Ordering::SeqCst) == 100)),
        );
        let result = c.create_vector_index("items", "vec", vec!["v".into()], 2, "l2", false);
        c.engine.set_progress_handler(0, None);
        assert_eq!(result.unwrap_err().code(), "FDB_CANCELLED");
        assert!(count.load(Ordering::SeqCst) > 100);
        assert_eq!(
            c.documents(&c.catalog("items").unwrap()).unwrap().len(),
            100
        );
        assert!(c.catalog("items").unwrap().indexes.is_empty());
        c.validate_storage_schema().unwrap();
        c.create_vector_index("items", "vec", vec!["v".into()], 2, "l2", false)
            .unwrap();
        c.check_collection_integrity("items", Default::default())
            .unwrap();
        c.run("ROLLBACK", &[]).unwrap();
    }
}

#[cfg(test)]
mod topology_test {
    #[test]
    fn bulk_build_keeps_distinct_hnsw_levels() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE items", &Default::default())
            .unwrap();
        c.run("BEGIN", &[]).unwrap();
        for n in 0..256 {
            c.execute(
                &format!("INSERT INTO items {{v:vector32('[{n},1]')}}"),
                &Default::default(),
            )
            .unwrap();
        }
        c.create_vector_index("items", "vec", vec!["v".into()], 2, "l2", false)
            .unwrap();
        let index = c.catalog("items").unwrap().indexes.remove(0);
        c.with_ann_graph(&index, |graph| {
            assert_eq!(graph.stats_for_level(0).nodes, 256);
            let upper = graph.stats_for_level(1).nodes;
            assert!(
                upper > 0 && upper < 256,
                "upper layer must be a proper subset: {upper}"
            );
            Ok(())
        })
        .unwrap();
        c.run("COMMIT", &[]).unwrap();
    }
}

#[cfg(test)]
mod compressed_tests {
    use super::*;
    #[test]
    fn compressed_graph_coordinates_reduce_serialized_size() {
        let mut sizes = Vec::new();
        for format in [FORMAT, FORMAT_F16] {
            let config = Config {
                dimensions: 128,
                metric: "cosine".into(),
                format: format.into(),
            };
            let graph = config.graph().unwrap();
            graph.reserve(256).unwrap();
            for row in 0..256 {
                let vector = (0..128)
                    .map(|column| (((row * 131 + column * 17) % 1009) as f32 / 1009.0) - 0.5)
                    .collect::<Vec<_>>();
                graph
                    .add(
                        row as u64,
                        &config
                            .components(&Value::vector32(&vector).unwrap())
                            .unwrap(),
                    )
                    .unwrap();
            }
            sizes.push(snapshot(&graph).unwrap().len());
        }
        eprintln!("256 x 128 graph bytes: f32={}, f16={}", sizes[0], sizes[1]);
        assert!(sizes[1] + 256 * 128 < sizes[0], "{sizes:?}");
    }
    #[test]
    fn compressed_graph_identity_checks_cache_metadata_and_corruption() {
        let c = crate::Database::open(":memory:")
            .unwrap()
            .connect()
            .unwrap();
        c.execute(
            "INSERT INTO docs {id:docs:a,v:vector32('[1.0001,0.1234567]')}",
            &Default::default(),
        )
        .unwrap();
        c.execute("CREATE SEARCH INDEX compressed ON docs(v) USING VECTOR WITH(dimensions=2,metric='l2',quantization='f16')",&Default::default()).unwrap();
        let collection = c.catalog("docs").unwrap();
        assert_eq!(collection.version, 5);
        let index = collection.indexes[0].clone();
        c.audit_vector_index(&index).unwrap();
        let mut mismatched = index.clone();
        mismatched.vector.as_mut().unwrap().format = FORMAT.into();
        assert!(c.ann_cache.lock().unwrap().is_some());
        assert!(c
            .with_ann_graph(&mismatched, |_| Ok(()))
            .unwrap_err()
            .to_string()
            .contains("graph metadata mismatch"));
        c.audit_vector_index(&index).unwrap();
        let metadata = serde_json::to_value(&collection).unwrap();
        for (path, value) in [
            ("/version", serde_json::json!(4)),
            (
                "/indexes/0/vector/format",
                serde_json::json!("usearch-2.26.2-f16-unknown"),
            ),
        ] {
            let mut changed = metadata.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert_eq!(
                crate::catalog::decode(&changed.to_string(), "docs")
                    .unwrap_err()
                    .code(),
                "FDB_STORAGE"
            );
        }
        c.run("SAVEPOINT corrupt", &[]).unwrap();
        c.run(
            &format!("UPDATE {} SET graph=x'010203'", quote(&index.ann_state())),
            &[],
        )
        .unwrap();
        c.discard_ann_cache(&index).unwrap();
        assert!(c
            .audit_vector_index(&index)
            .unwrap_err()
            .to_string()
            .contains("checksum"));
        c.run("ROLLBACK TO corrupt", &[]).unwrap();
        c.run("RELEASE corrupt", &[]).unwrap();
        c.audit_vector_index(&index).unwrap();
        c.run(
            &format!("UPDATE {} SET \"key\"=?1", quote(&index.storage)),
            &[EngineValue::Blob(
                Value::vector32(&[2.0, 1.0]).unwrap().encode().unwrap(),
            )],
        )
        .unwrap();
        assert!(c
            .audit_vector_index(&index)
            .unwrap_err()
            .to_string()
            .contains("stale or missing"));
    }
}
