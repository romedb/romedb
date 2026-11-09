//! Managed full-text storage and deterministic ranking over pinned native FTS.
use crate::{
    canonical, quote, text, Collection, Connection, Document, EngineValue, Error, Index, IndexKind,
    Result, Value,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub additional_paths: Vec<Vec<String>>,
    pub tokenizer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_gram: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_gram: Option<usize>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fold: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stemmer: Option<String>,
    #[serde(default = "legacy_storage_version")]
    pub storage_version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weights: Vec<Weight>,
}
const fn legacy_storage_version() -> u32 {
    1
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Weight(pub f32);
impl Eq for Weight {}
pub(crate) const WEIGHTS_CATALOG_VERSION: u32 = 9;
pub(crate) const FOLD_STEMMER_CATALOG_VERSION: u32 = 10;
const TOKENIZER: &str = "tantivy-default-0.26";
// Stay below the pinned native FTS cursor's 1,000-document flush threshold,
// and within 2,176 bound parameters: 128 rows of sixteen text fields plus the
// ID. Each statement has a fixed cost in the native index, so an index on
// fewer fields builds with more rows per statement: two fields take 725, and
// built 500,000 documents in 287 s instead of 653 s.
const BUILD_BATCH_ROWS: usize = 900;
const BUILD_BATCH_PARAMETERS: usize = 2176;
const _: () = assert!(BUILD_BATCH_ROWS < turso_core::index_method::fts::BATCH_COMMIT_SIZE);
fn invalid(message: &str) -> Error {
    Error::Validation(format!("fulltext: {message}"))
}

impl Config {
    pub(crate) fn is_custom(&self) -> bool {
        self.tokenizer != TOKENIZER
            || self.min_gram.is_some()
            || self.max_gram.is_some()
            || self.folds_or_stems()
    }
    pub(crate) fn folds_or_stems(&self) -> bool {
        self.fold || self.stemmer.is_some()
    }
    pub(crate) fn options(&self) -> Result<crate::FullTextOptions> {
        let name = self
            .tokenizer
            .strip_prefix("tantivy-")
            .and_then(|s| s.strip_suffix("-0.26"))
            .ok_or_else(|| invalid("unknown tokenizer identity"))?;
        let options = crate::FullTextOptions {
            tokenizer: name.into(),
            min_gram: self.min_gram,
            max_gram: self.max_gram,
            fold: self.fold,
            stemmer: self.stemmer.clone(),
        };
        if options.normalized()? != options || (self.is_custom() && self.storage_version != 2) {
            return Err(invalid("noncanonical analyzer configuration"));
        }
        Ok(options)
    }
}

impl Index {
    pub(crate) fn require_current_text_storage(&self) -> Result<()> {
        if self
            .fulltext
            .as_ref()
            .is_some_and(|config| config.storage_version == 1)
        {
            return Err(Error::Storage(format!("full-text index {} requires REINDEX {} for Turso 0.8.1; back up the database before upgrading", self.name, quote(&self.name))));
        }
        Ok(())
    }

    pub(crate) fn paths(&self) -> impl Iterator<Item = &[String]> {
        std::iter::once(self.path.as_slice())
            .chain(
                self.fulltext
                    .iter()
                    .flat_map(|c| c.additional_paths.iter().map(Vec::as_slice)),
            )
            .chain(
                self.scalar
                    .iter()
                    .flat_map(|c| c.additional_paths.iter().map(Vec::as_slice)),
            )
    }
    pub(crate) fn validate_text_config(&self) -> Result<()> {
        match (&self.kind, &self.fulltext) {
            (IndexKind::FullText, Some(config))
                if config.options().is_ok()
                    && matches!(config.storage_version, 1 | 2)
                    && !self.unique =>
            {
                // The native method generates its directory DDL from the index
                // name without quoting. Reject names it cannot represent safely.
                if self.name.is_empty()
                    || !self
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                {
                    return Err(invalid(
                        "index names must contain ASCII letters, digits or underscores",
                    ));
                }
                let mut seen = std::collections::BTreeSet::new();
                for path in self.paths() {
                    crate::validate_path(path)?;
                    if !seen.insert(path) {
                        return Err(invalid("duplicate field path"));
                    }
                }
                if seen.len() > 16 {
                    return Err(invalid("at most 16 text fields per index"));
                }
                if !config.weights.is_empty()
                    && (config.weights.len() != seen.len()
                        || config.storage_version != 2
                        || !config.weights.iter().all(|w| w.0.is_finite() && w.0 > 0.0))
                {
                    return Err(invalid("invalid field weights"));
                }
                Ok(())
            }
            (
                IndexKind::Scalar | IndexKind::Array | IndexKind::Spatial | IndexKind::Vector,
                None,
            ) => Ok(()),
            _ => Err(invalid("incompatible text index configuration")),
        }
    }
    pub(crate) fn document_keys(&self, doc: &Document) -> Result<Vec<EngineValue>> {
        if self.kind == IndexKind::Array {
            self.array_keys(doc)?;
            return Ok(Vec::new());
        }
        if self.kind == IndexKind::Scalar {
            return self.scalar_keys(doc);
        }
        if self.kind != IndexKind::FullText {
            return self.keys(crate::path_value(doc, &self.path)?.unwrap_or(&Value::Null));
        }
        self.paths()
            .map(
                |path| match crate::path_value(doc, path)?.unwrap_or(&Value::Null) {
                    Value::Null => Ok(EngineValue::Null),
                    Value::String(s) => Ok(text(s)),
                    _ => Err(invalid("indexed fields must be strings, missing or null")),
                },
            )
            .collect()
    }
    pub(crate) fn key_columns(&self) -> String {
        match self.kind {
            IndexKind::Scalar => self.scalar_columns().join(","),
            IndexKind::Vector | IndexKind::Array => "\"key\"".into(),
            IndexKind::Spatial if self.spatial.is_some() => "\"key\",longitude,latitude".into(),
            IndexKind::Spatial => "\"key\",longitude".into(),
            IndexKind::FullText => std::iter::once("\"key\"".to_owned())
                .chain((1..self.paths().count()).map(|i| format!("f{i}")))
                .collect::<Vec<_>>()
                .join(","),
        }
    }
    fn text_columns(&self) -> impl Iterator<Item = String> {
        std::iter::once("key".to_owned()).chain((1..self.paths().count()).map(|i| format!("f{i}")))
    }
    pub(crate) fn text_table_ddl(&self) -> String {
        let extra = (1..self.paths().count())
            .map(|i| format!(",f{i} TEXT"))
            .collect::<String>();
        format!(
            "CREATE TABLE {} (\"key\" TEXT,id BLOB UNIQUE NOT NULL{extra})",
            quote(&self.storage)
        )
    }
    pub(crate) fn text_index_ddl(&self) -> String {
        let config = self.fulltext.as_ref().expect("fulltext config");
        let mut parameters = Vec::new();
        if config.is_custom() {
            let options = config.options().expect("validated analyzer");
            parameters.push(format!("tokenizer='{}'", options.tokenizer));
            if let Some((min, max)) = options.min_gram.zip(options.max_gram) {
                parameters.push(format!("min_gram={min},max_gram={max}"));
            }
            if options.fold {
                parameters.push("fold=1".into());
            }
            if let Some(stemmer) = &options.stemmer {
                parameters.push(format!("stemmer='{stemmer}'"));
            }
        }
        if !config.weights.is_empty() {
            let weights = self
                .text_columns()
                .zip(&config.weights)
                .map(|(column, weight)| format!("{column}={}", weight.0))
                .collect::<Vec<_>>()
                .join(",");
            parameters.push(format!("weights='{weights}'"));
        }
        let suffix = if parameters.is_empty() {
            String::new()
        } else {
            format!(" WITH ({})", parameters.join(","))
        };
        format!(
            "CREATE INDEX {} ON {} USING fts ({}){suffix}",
            quote(&self.name),
            quote(&self.storage),
            self.key_columns()
        )
    }
    pub(crate) fn text_stats(&self) -> String {
        format!("{}_stats", self.storage)
    }
    pub(crate) fn text_stats_ddl(&self) -> String {
        format!("CREATE TABLE {} (slot INTEGER PRIMARY KEY CHECK(slot=1), count INTEGER NOT NULL CHECK(count>=0))", quote(&self.text_stats()))
    }
    pub(crate) fn text_directory(&self) -> String {
        format!("__turso_internal_fts_dir_{}", self.name)
    }
    /// Every table that holds data for this index. `drop_index_storage`
    /// removes the same tables.
    pub(crate) fn storage_tables(&self) -> Vec<String> {
        let mut tables = vec![self.storage.clone()];
        match self.kind {
            IndexKind::FullText => {
                tables.push(self.text_directory());
                if self
                    .fulltext
                    .as_ref()
                    .is_some_and(|config| config.storage_version == 1)
                {
                    tables.push(self.text_stats());
                }
            }
            IndexKind::Vector => tables.extend([self.ann_state(), self.ann_log()]),
            IndexKind::Scalar | IndexKind::Spatial | IndexKind::Array => {}
        }
        tables.extend(
            self.extra_schema()
                .into_iter()
                .filter(|object| object.kind == "table")
                .map(|object| object.name),
        );
        tables
    }
    pub(crate) fn text_directory_ddl(&self) -> String {
        format!(
            "CREATE TABLE {} (path TEXT NOT NULL, chunk_no INTEGER NOT NULL, bytes BLOB NOT NULL)",
            self.text_directory()
        )
    }
    pub(crate) fn text_directory_index_ddl(&self) -> String {
        let dir = self.text_directory();
        format!("CREATE INDEX IF NOT EXISTS {dir}_key ON {dir} USING backing_btree (path, chunk_no, bytes)")
    }
}

impl Connection {
    /// Index up to 16 string paths with the pinned default text analyzer.
    pub fn create_fulltext_index(
        &self,
        table: &str,
        name: &str,
        paths: Vec<Vec<String>>,
        if_not_exists: bool,
    ) -> Result<()> {
        self.create_fulltext_index_with_options(
            table,
            name,
            paths,
            crate::FullTextOptions::default(),
            if_not_exists,
        )
    }
    pub fn create_fulltext_index_with_options(
        &self,
        table: &str,
        name: &str,
        paths: Vec<Vec<String>>,
        options: crate::FullTextOptions,
        if_not_exists: bool,
    ) -> Result<()> {
        self.create_fulltext_index_with_weights(table, name, paths, options, None, if_not_exists)
    }
    /// `weights` is `path=weight,...` with each path written with dots, as in
    /// `title=3,meta.summary=2`. Paths it leaves out weigh 1.
    pub fn create_fulltext_index_with_weights(
        &self,
        table: &str,
        name: &str,
        paths: Vec<Vec<String>>,
        options: crate::FullTextOptions,
        weights: Option<&str>,
        if_not_exists: bool,
    ) -> Result<()> {
        let options = options.normalized()?;
        let weights = weights
            .map(|text| parse_weights(text, &paths))
            .transpose()?
            .unwrap_or_default();
        let name = canonical(name)?;
        let Some(path) = paths.first() else {
            return Err(invalid("at least one text field is required"));
        };
        let index = Index {
            kind: IndexKind::FullText,
            name: name.clone(),
            path: path.clone(),
            unique: false,
            storage: format!(
                "__romedb_i_{}",
                name.as_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ),
            vector: None,
            scalar: None,
            spatial: None,
            fulltext: Some(Config {
                additional_paths: paths[1..].to_vec(),
                tokenizer: options.identity(),
                min_gram: options.min_gram,
                max_gram: options.max_gram,
                fold: options.fold,
                stemmer: options.stemmer.clone(),
                storage_version: 2,
                weights,
            }),
        };
        index.validate_text_config()?;
        self.index_build(|| {
            let mut c = self.catalog(table)?;
            if let Some(existing) = c.indexes.iter().find(|i| i.name == name) {
                if if_not_exists && existing.kind == index.kind && existing.path == index.path && existing.fulltext == index.fulltext { return Ok(()); }
                return Err(Error::AlreadyExists(name.clone()));
            }
            if !self.run("SELECT name FROM sqlite_schema WHERE name=?1 COLLATE NOCASE UNION ALL SELECT name FROM __romedb_catalog WHERE name=?1", &[text(&name)])?.is_empty() {
                return Err(Error::AlreadyExists(name.clone()));
            }
            for path in index.paths() { crate::catalog::compatible_index(&c, path, index.kind)?; }
            self.build_text_storage(&index, &c, None)?;
            c.indexes.push(index.clone());
            self.save_catalog(&c)
        })
    }
    pub(crate) fn build_text_storage(
        &self,
        index: &Index,
        collection: &Collection,
        budget: Option<&mut crate::budget::ResultBudget>,
    ) -> Result<()> {
        self.run(&index.table_ddl(), &[])?;
        self.run(&index.index_ddl(), &[])?;
        let rows = (BUILD_BATCH_PARAMETERS / (index.paths().count() + 1)).min(BUILD_BATCH_ROWS);
        self.document_batches(collection, rows, budget, |batch| {
            self.insert_text_build_batch(index, batch)
        })
    }
    fn insert_text_build_batch(&self, index: &Index, documents: &[Document]) -> Result<()> {
        let mut values = Vec::new();
        let mut rows = Vec::with_capacity(documents.len());
        for document in documents {
            let mut row = index.document_keys(document)?;
            let id = document
                .get("id")
                .ok_or_else(|| Error::Storage("document has no id".into()))?;
            row.insert(1, EngineValue::Blob(id.encode()?));
            let first = values.len() + 1;
            let slots = (first..first + row.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(",");
            rows.push(format!("({slots})"));
            values.extend(row);
        }
        // One statement shares an FTS cursor, amortizing its Tantivy commit.
        // Keep batches below its internal mid-insert flush/reentry path.
        self.run_index_maintenance(
            &format!(
                "INSERT INTO {} VALUES {}",
                quote(&index.storage),
                rows.join(",")
            ),
            &values,
        )?;
        self.change_text_count(index, documents.len() as i64)
    }
    pub(crate) fn change_text_count(&self, index: &Index, delta: i64) -> Result<()> {
        if index
            .fulltext
            .as_ref()
            .is_some_and(|config| config.storage_version == 2)
        {
            return Ok(());
        }
        let rows = self.run_index_maintenance(
            &format!(
                "UPDATE {} SET count=count+?1 WHERE slot=1 RETURNING count",
                quote(&index.text_stats())
            ),
            &[EngineValue::from_i64(delta)],
        )?;
        if rows.len() != 1 {
            return Err(Error::Storage("missing full-text count".into()));
        }
        Ok(())
    }
    pub(crate) fn text_count(&self, index: &Index) -> Result<i64> {
        if index
            .fulltext
            .as_ref()
            .is_some_and(|config| config.storage_version == 2)
        {
            let rows = self.run(
                &format!("SELECT count(*) FROM {}", quote(&index.storage)),
                &[],
            )?;
            return match rows.as_slice() {
                [row] => match row.as_slice() {
                    [EngineValue::Numeric(turso_core::Numeric::Integer(n))] if *n >= 0 => Ok(*n),
                    _ => Err(Error::Storage("invalid full-text count".into())),
                },
                _ => Err(Error::Storage("missing full-text count".into())),
            };
        }
        let rows = self.run(
            &format!(
                "SELECT count FROM {} WHERE slot=1",
                quote(&index.text_stats())
            ),
            &[],
        )?;
        match rows.as_slice() {
            [row] => match row.as_slice() {
                [EngineValue::Numeric(turso_core::Numeric::Integer(n))] if *n >= 0 => Ok(*n),
                _ => Err(Error::Storage("invalid full-text count".into())),
            },
            _ => Err(Error::Storage("missing full-text count".into())),
        }
    }
    pub(crate) fn delete_index_entry(&self, index: &Index, id: &EngineValue) -> Result<()> {
        index.require_current_text_storage()?;
        if index.kind == IndexKind::Vector {
            return self.delete_vector_entry(index, id);
        }
        let returning = if index.kind == IndexKind::FullText {
            " RETURNING id"
        } else {
            ""
        };
        let rows = self.run_index_maintenance(
            &format!(
                "DELETE FROM {} WHERE id=?1{returning}",
                quote(&index.storage)
            ),
            std::slice::from_ref(id),
        )?;
        if index.region_index() {
            self.delete_region_shape(index, id)?;
        }
        if index.kind == IndexKind::FullText && !rows.is_empty() {
            self.change_text_count(index, -(rows.len() as i64))?;
        }
        Ok(())
    }
    pub(crate) fn drop_index_storage(&self, index: &Index) -> Result<()> {
        // Explicit DROP INDEX invokes the native method's directory/cache cleanup.
        if index.kind == IndexKind::FullText {
            self.run(&format!("DROP INDEX {}", quote(&index.name)), &[])?;
            if index
                .fulltext
                .as_ref()
                .is_some_and(|config| config.storage_version == 1)
            {
                self.run(&format!("DROP TABLE {}", quote(&index.text_stats())), &[])?;
            }
        }
        if index.kind == IndexKind::Vector {
            for name in [index.ann_state(), index.ann_log()] {
                self.run(&format!("DROP TABLE {}", quote(&name)), &[])?;
            }
            self.discard_ann_cache(index)?;
        }
        for object in index.extra_schema() {
            if object.kind == "table" {
                self.run(&format!("DROP TABLE {}", quote(&object.name)), &[])?;
            }
        }
        self.run(&format!("DROP TABLE {}", quote(&index.storage)), &[])?;
        Ok(())
    }
    pub(crate) fn text_search_sql(
        &self,
        name: &Value,
        query: &Value,
        limit: &Value,
        filter: Option<&Value>,
    ) -> Result<String> {
        let (Value::String(name), Value::String(query)) = (name, query) else {
            return Err(invalid("index and query must be strings"));
        };
        validate_query(query)?;
        let limit = match limit {
            Value::Integer(n) => *n as f64,
            Value::Number(n) => *n,
            _ => return Err(invalid("limit must be an integer from 0 through 10000")),
        };
        if !(0.0..=10_000.0).contains(&limit) || limit.fract() != 0.0 {
            return Err(invalid("limit must be an integer from 0 through 10000"));
        }
        let name = canonical(name)?;
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
            .ok_or_else(|| Error::NotFound(format!("text index {name}")))?;
        if index.kind != IndexKind::FullText {
            return Err(invalid("search::text requires a full-text index"));
        }
        index.require_current_text_storage()?;
        let filter = filter
            .map(|ids| crate::search_filter::Filter::from_value(ids, &table))
            .transpose()?;
        let query = format!("'{}'", query.replace('\'', "''"));
        let columns = index.key_columns();
        let storage = quote(&index.storage);
        let Some(filter) = filter else {
            return self.text_best_hits_sql(&storage, &columns, &query, limit as usize);
        };
        // Turso 0.8.1 treats a negative LIMIT as all live indexed documents.
        // A filter can drop any of the best hits, so it needs them all.
        let inner = format!("SELECT id,fts_score({columns},{query}) AS score FROM {storage} WHERE fts_match({columns},{query}) LIMIT -1");
        self.require_native_text_search(&inner)?;
        Ok(format!("WITH __romedb_text_hits AS MATERIALIZED ({inner}) SELECT id,score FROM __romedb_text_hits WHERE {} ORDER BY score DESC,id LIMIT {limit}", filter.predicate()))
    }
    /// The best hits, read once while the statement is planned and given to it
    /// as rows, like search::nearest. The native index finds its best hits
    /// without scoring every match, but it breaks ties its own way, so the
    /// window grows until every hit that ties with the last one wanted is
    /// inside it. The statement runs in one snapshot with this read.
    fn text_best_hits_sql(
        &self,
        storage: &str,
        columns: &str,
        query: &str,
        limit: usize,
    ) -> Result<String> {
        let best = |window: usize| {
            format!("SELECT id,fts_score({columns},{query}) AS score FROM {storage} WHERE fts_match({columns},{query}) ORDER BY score DESC LIMIT {window}")
        };
        self.require_native_text_search(&best(limit + 1))?;
        if crate::graph::explaining() {
            return Ok(format!("WITH __romedb_text_hits AS MATERIALIZED ({}) SELECT id,score FROM __romedb_text_hits ORDER BY score DESC,id LIMIT {limit}", best(limit)));
        }
        let mut window = limit * 2 + 16;
        let mut hits = loop {
            let mut hits = self
                .run_customer(&best(window), &[])?
                .iter()
                .map(|row| text_hit(row))
                .collect::<Result<Vec<_>>>()?;
            hits.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            if limit == 0 || hits.len() < window || hits[window - 1].1 < hits[limit - 1].1 {
                break hits;
            }
            window *= 2;
        };
        hits.truncate(limit);
        if hits.is_empty() {
            return Ok("SELECT NULL AS id, NULL AS score WHERE 0".into());
        }
        let values = hits
            .iter()
            .map(|(id, score)| {
                let id = id.iter().map(|b| format!("{b:02x}")).collect::<String>();
                format!("(x'{id}',{score:?})")
            })
            .collect::<Vec<_>>()
            .join(",");
        Ok(format!("WITH __romedb_text_hits(id,score) AS MATERIALIZED (VALUES {values}) SELECT id,score FROM __romedb_text_hits ORDER BY score DESC,id"))
    }
    fn require_native_text_search(&self, sql: &str) -> Result<()> {
        let plan = self.run(&format!("EXPLAIN QUERY PLAN {sql}"), &[])?;
        if !plan.iter().any(|row| matches!(row.last(), Some(EngineValue::Text(t)) if t.as_str() == "QUERY INDEX METHOD fts")) {
            return Err(Error::Storage("full-text query did not select its native index".into()));
        }
        Ok(())
    }
}

fn parse_weights(text: &str, paths: &[Vec<String>]) -> Result<Vec<Weight>> {
    let mut weights: Vec<Option<f32>> = vec![None; paths.len()];
    for part in text.split(',') {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| invalid("weights are written as path=weight, separated by commas"))?;
        let key = key.trim();
        let mut slots = paths
            .iter()
            .enumerate()
            .filter(|(_, path)| path.join(".") == key);
        let (Some((slot, _)), None) = (slots.next(), slots.next()) else {
            return Err(invalid(&format!(
                "weights name {key}, which is not one of the index's paths"
            )));
        };
        let weight: f32 = value
            .trim()
            .parse()
            .map_err(|_| invalid(&format!("the weight of {key} is not a number")))?;
        if !weight.is_finite() || weight <= 0.0 {
            return Err(invalid(&format!(
                "the weight of {key} must be a positive number"
            )));
        }
        if weights[slot].replace(weight).is_some() {
            return Err(invalid(&format!("weights name {key} twice")));
        }
    }
    if weights.iter().all(|w| w.is_none_or(|w| w == 1.0)) {
        return Ok(Vec::new());
    }
    Ok(weights
        .into_iter()
        .map(|w| Weight(w.unwrap_or(1.0)))
        .collect())
}

fn text_hit(row: &[EngineValue]) -> Result<(Vec<u8>, f64)> {
    let [EngineValue::Blob(id), score] = row else {
        return Err(Error::Storage("invalid full-text hit".into()));
    };
    let score = match score {
        EngineValue::Numeric(turso_core::Numeric::Float(score)) => f64::from(*score),
        EngineValue::Numeric(turso_core::Numeric::Integer(score)) => *score as f64,
        _ => return Err(Error::Storage("full-text score is not a number".into())),
    };
    Ok((id.clone(), score))
}

pub(crate) fn validate_query(query: &str) -> Result<()> {
    use tantivy_query_grammar::{UserInputAst, UserInputLeaf};
    if query.len() > 4096 {
        return Err(Error::Limit("full-text query exceeds 4096 bytes".into()));
    }
    let ast = tantivy_query_grammar::parse_query(query)
        .map_err(|_| invalid("invalid text query syntax"))?;
    let mut pending = vec![(&ast, 0)];
    while let Some((node, depth)) = pending.pop() {
        if depth > 32 {
            return Err(Error::Limit("full-text query nesting exceeds 32".into()));
        }
        match node {
            UserInputAst::Clause(children) => {
                // The pinned grammar represents `a AND NOT b` as
                // (+a +(-b)). Tantivy treats the negative-only inner clause as
                // empty, producing incorrect Boolean exclusion. Reject that
                // shape instead of returning misleading matches. A positive
                // clause with sibling exclusions (`+a -b`) remains supported.
                if !children.is_empty()
                    && children
                        .iter()
                        .all(|(occur, _)| *occur == Some(tantivy_query_grammar::Occur::MustNot))
                {
                    return Err(invalid(
                        "negative-only query clauses are unsupported; use a positive term with -term in the same clause",
                    ));
                }
                pending.extend(children.iter().map(|(_, node)| (node, depth + 1)))
            }
            UserInputAst::Leaf(leaf) => match leaf.as_ref() {
                UserInputLeaf::All => {}
                UserInputLeaf::Literal(literal) if literal.field_name.is_none() => {
                    validate_prefix(literal)?
                }
                _ => return Err(unsupported_query()),
            },
            UserInputAst::Boost(..) => return Err(unsupported_query()),
        }
    }
    Ok(())
}

fn unsupported_query() -> Error {
    invalid("queries support unqualified terms, phrases, prefixes, boolean clauses and *")
}

fn validate_prefix(literal: &tantivy_query_grammar::UserInputLiteral) -> Result<()> {
    use turso_core::index_method::fts::MIN_PREFIX_CHARS;
    let stem = if literal.prefix {
        literal.phrase.as_str()
    } else if literal.delimiter == tantivy_query_grammar::Delimiter::None {
        match literal.phrase.strip_suffix('*') {
            Some(stem) if !stem.is_empty() => stem,
            _ => return Ok(()),
        }
    } else {
        return Ok(());
    };
    if !literal.prefix && stem.contains('*') {
        return Err(invalid("a prefix takes one * at the end of a word"));
    }
    let words: Vec<&str> = stem
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    if words.len() <= 1
        && words
            .first()
            .is_none_or(|w| w.chars().count() < MIN_PREFIX_CHARS)
    {
        return Err(invalid(&format!(
            "a prefix needs at least {MIN_PREFIX_CHARS} characters before *"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (crate::Database, Connection, Index) {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.execute(
            "INSERT INTO docs {title:'hello',body:'world'}",
            &crate::Parameters::new(),
        )
        .unwrap();
        c.create_fulltext_index(
            "docs",
            "text_idx",
            vec![vec!["title".into()], vec!["body".into()]],
            false,
        )
        .unwrap();
        let index = c.catalog("docs").unwrap().indexes.remove(0);
        (db, c, index)
    }
    #[test]
    fn text_metadata_and_owned_schema_cannot_be_weakened() {
        let (db, c, index) = setup();
        let catalog = c.catalog("docs").unwrap();
        assert_eq!(catalog.version, 4);
        let original = serde_json::to_value(catalog).unwrap();
        for (path, value) in [
            ("/version", serde_json::json!(2)),
            ("/version", serde_json::json!(3)),
            ("/indexes/0/fulltext/storage_version", serde_json::json!(99)),
            ("/indexes/0/fulltext/tokenizer", serde_json::json!("other")),
            ("/indexes/0/fulltext", serde_json::Value::Null),
            ("/indexes/0/unique", serde_json::json!(true)),
            (
                "/indexes/0/fulltext/additional_paths",
                serde_json::json!([["title"]]),
            ),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(crate::catalog::decode(&changed.to_string(), "docs").is_err());
        }
        c.run(&format!("DROP INDEX {}", quote(&index.name)), &[])
            .unwrap();
        assert!(db.connect().is_err());
        assert!(c
            .text_search_sql(
                &Value::String("text_idx".into()),
                &Value::String("hello".into()),
                &Value::Integer(10),
                None
            )
            .is_err());
    }
    #[test]
    fn weighted_text_indexes_need_catalog_version_9() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.execute(
            "INSERT INTO docs {title:'hello',body:'world'}",
            &crate::Parameters::new(),
        )
        .unwrap();
        c.create_fulltext_index_with_weights(
            "docs",
            "weighted",
            vec![vec!["title".into()], vec!["body".into()]],
            crate::FullTextOptions::default(),
            Some("title=3"),
            false,
        )
        .unwrap();
        let catalog = c.catalog("docs").unwrap();
        assert_eq!(catalog.version, WEIGHTS_CATALOG_VERSION);
        let index = &catalog.indexes[0];
        assert_eq!(
            index.fulltext.as_ref().unwrap().weights,
            vec![Weight(3.0), Weight(1.0)]
        );
        assert!(index
            .text_index_ddl()
            .ends_with(" WITH (weights='key=3,f1=1')"));
        let original = serde_json::to_value(&catalog).unwrap();
        assert!(crate::catalog::decode(&original.to_string(), "docs").is_ok());
        for (path, value) in [
            ("/version", serde_json::json!(8)),
            ("/indexes/0/fulltext/weights", serde_json::json!([3.0])),
            ("/indexes/0/fulltext/weights", serde_json::json!([0.0, 1.0])),
            (
                "/indexes/0/fulltext/weights",
                serde_json::json!([-1.0, 1.0]),
            ),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(
                crate::catalog::decode(&changed.to_string(), "docs").is_err(),
                "{path}"
            );
        }
        c.create_fulltext_index_with_weights(
            "docs",
            "unweighted",
            vec![vec!["title".into()]],
            crate::FullTextOptions::default(),
            Some("title=1"),
            false,
        )
        .unwrap();
        let unweighted = c.catalog("docs").unwrap().indexes.remove(1);
        assert!(unweighted.fulltext.unwrap().weights.is_empty());
    }
    #[test]
    fn folded_and_stemmed_text_indexes_need_catalog_version_10() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.execute("INSERT INTO docs {body:'Café'}", &crate::Parameters::new())
            .unwrap();
        let options = crate::FullTextOptions {
            fold: true,
            stemmer: Some("English".into()),
            ..Default::default()
        };
        c.create_fulltext_index_with_options(
            "docs",
            "stemmed",
            vec![vec!["body".into()]],
            options,
            false,
        )
        .unwrap();
        let catalog = c.catalog("docs").unwrap();
        assert_eq!(catalog.version, FOLD_STEMMER_CATALOG_VERSION);
        let index = &catalog.indexes[0];
        let config = index.fulltext.as_ref().unwrap();
        assert!(config.fold);
        assert_eq!(config.stemmer.as_deref(), Some("english"));
        assert!(index
            .text_index_ddl()
            .ends_with(" WITH (tokenizer='default',fold=1,stemmer='english')"));
        let original = serde_json::to_value(&catalog).unwrap();
        assert!(crate::catalog::decode(&original.to_string(), "docs").is_ok());
        for (path, value) in [
            ("/version", serde_json::json!(WEIGHTS_CATALOG_VERSION)),
            ("/indexes/0/fulltext/stemmer", serde_json::json!("English")),
            ("/indexes/0/fulltext/stemmer", serde_json::json!("klingon")),
            ("/indexes/0/fulltext/fold", serde_json::json!("yes")),
            (
                "/indexes/0/fulltext/tokenizer",
                serde_json::json!("tantivy-ngram-0.26"),
            ),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(
                crate::catalog::decode(&changed.to_string(), "docs").is_err(),
                "{path}"
            );
        }
        let mut unfolded = original.clone();
        unfolded["indexes"][0]["fulltext"]
            .as_object_mut()
            .unwrap()
            .remove("fold");
        unfolded["indexes"][0]["fulltext"]
            .as_object_mut()
            .unwrap()
            .remove("stemmer");
        assert!(crate::catalog::decode(&unfolded.to_string(), "docs").is_ok());
    }
    #[test]
    fn text_audit_detects_stale_fields_counts_and_missing_entries() {
        for mutation in ["UPDATE {storage} SET f1='changed'", "DELETE FROM {storage}"] {
            let (_db, c, index) = setup();
            c.run(
                &mutation
                    .replace("{storage}", &quote(&index.storage))
                    .replace("{stats}", &quote(&index.text_stats())),
                &[],
            )
            .unwrap();
            assert!(
                c.check_collection_integrity("docs", Default::default())
                    .is_err(),
                "{mutation}"
            );
        }
    }
}
