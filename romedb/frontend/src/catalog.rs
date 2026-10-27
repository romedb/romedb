//! Transactional lifecycle of logical collections, fields, and managed indexes.
use crate::{
    canonical, quote, text, Collection, Connection, Document, Error, FieldType, Parameters,
    QueryResult, Result, Value,
};
use turso_core::Value as EngineValue;
use turso_parser::ast::{Cmd, Stmt};

// Compare our fixed generated DDL lexically, avoiding recursive parsing of
// externally modified schema SQL. Whitespace, keyword case and trailing
// semicolons are immaterial; constraints and identifier quoting remain exact.
pub(crate) fn schema_tokens(sql: &str) -> Result<Vec<(romeql_parser::Kind, String)>> {
    use romeql_parser::Kind;
    let mut tokens = romeql_parser::tokenize(sql)
        .map_err(|e| Error::Storage(format!("invalid managed schema SQL: {e}")))?
        .into_iter()
        .filter(|t| !(t.kind == Kind::Symbol && t.text == ";"))
        .map(|t| {
            let text = if t.kind == Kind::Word {
                t.text.to_ascii_lowercase()
            } else {
                t.text
            };
            (t.kind, text)
        })
        .collect::<Vec<_>>();
    // SQLite may omit the creation-time IF NOT EXISTS flag in stored DDL.
    if tokens.len() >= 5
        && tokens[0].1 == "create"
        && tokens[1].1 == "table"
        && tokens[2..5]
            .iter()
            .map(|t| t.1.as_str())
            .eq(["if", "not", "exists"])
    {
        tokens.drain(2..5);
    }
    Ok(tokens)
}

pub(crate) const fn version() -> u32 {
    2
}
pub(crate) const fn legacy_version() -> u32 {
    1
}
pub(crate) fn validate_version(collection: &Collection) -> Result<()> {
    if !matches!(collection.version, 1..=7) {
        return Err(Error::Storage(format!(
            "unsupported collection metadata version {}",
            collection.version
        )));
    }
    if (collection.strict || !collection.field_policies.is_empty()) && collection.version < 5 {
        return Err(Error::Storage(
            "field rules require catalog version 5".into(),
        ));
    }
    if collection.version < 5
        && collection.indexes.iter().any(|index| {
            index
                .fulltext
                .as_ref()
                .is_some_and(|config| config.is_custom())
        })
    {
        return Err(Error::Storage(
            "custom analyzers require catalog version 5".into(),
        ));
    }
    if collection.version < 5
        && collection.indexes.iter().any(|index| {
            index
                .vector
                .as_ref()
                .is_some_and(|config| config.compressed())
        })
    {
        return Err(Error::Storage(
            "compressed vector indexes require catalog version 5".into(),
        ));
    }
    if collection.version < 5
        && collection
            .indexes
            .iter()
            .any(|index| index.scalar.is_some())
    {
        return Err(Error::Storage(
            "compound indexes require catalog version 5".into(),
        ));
    }
    if collection.version < 5
        && collection
            .indexes
            .iter()
            .any(|index| index.kind == crate::IndexKind::Array)
    {
        return Err(Error::Storage(
            "array indexes require catalog version 5".into(),
        ));
    }
    if !collection.relations.is_empty() && collection.version < 3 {
        return Err(Error::Storage("relations require catalog version 3".into()));
    }
    if collection
        .indexes
        .iter()
        .any(|i| i.kind != crate::IndexKind::Scalar && (collection.version < 3 || i.unique))
    {
        return Err(Error::Storage(
            "search indexes require catalog version 3 and cannot be unique".into(),
        ));
    }
    if collection.version < 4
        && collection.indexes.iter().any(|index| {
            index
                .fulltext
                .as_ref()
                .is_some_and(|config| config.storage_version == 2)
        })
    {
        return Err(Error::Storage(
            "FTS storage version 2 requires catalog version 4".into(),
        ));
    }
    for field in &collection.fields {
        if let FieldType::Vector(dims) = field.kind {
            crate::vectors::validate_dimension(dims)?;
        }
    }
    crate::graph::validate_version(collection)?;
    crate::spatial::validate_version(collection)?;

    if collection.version == 1 && collection.fields.iter().any(|f| f.check.is_some()) {
        return Err(Error::Storage(
            "CHECK metadata requires catalog version 2".into(),
        ));
    }
    Ok(())
}
pub(crate) fn decode(metadata: &str, name: &str) -> Result<Collection> {
    let c: Collection = serde_json::from_str(metadata)
        .map_err(|e| Error::Storage(format!("invalid collection metadata: {e}")))?;
    let validate = || -> Result<()> {
        validate_version(&c)?;
        if canonical(name)? != name || c.name != name {
            return Err(Error::Storage("collection metadata name mismatch".into()));
        }
        let storage = |prefix: &str, name: &str| {
            format!(
                "{prefix}{}",
                name.as_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            )
        };
        if c.storage != storage("__romedb_c_", name) {
            return Err(Error::Storage(
                "collection storage identity mismatch".into(),
            ));
        }
        let mut paths = std::collections::BTreeSet::new();
        for field in &c.fields {
            crate::validate_path(&field.path)?;
            if let Some(check) = &field.check {
                romeql_parser::validate_delimiter_depth(&romeql_parser::tokenize(check)?)?;
            }
            if field.path[0] == "id" || !paths.insert(&field.path) {
                return Err(Error::Storage(
                    "invalid or duplicate field definition".into(),
                ));
            }
            if let FieldType::Record(target) = &field.kind {
                // The Rust field API has always accepted case-insensitive
                // record targets; validate the name without rejecting that form.
                canonical(target)?;
            }
        }
        crate::field_rules::validate_catalog(&c)?;
        let mut names = std::collections::BTreeSet::new();
        for index in &c.indexes {
            crate::validate_path(&index.path)?;
            if canonical(&index.name)? != index.name
                || !names.insert(&index.name)
                || index.storage != storage("__romedb_i_", &index.name)
            {
                return Err(Error::Storage("invalid index identity".into()));
            }
            index.validate_scalar_config()?;
            index.validate_text_config()?;
            index.validate_vector_config(&c)?;
            for path in index.paths() {
                compatible_index(&c, path, index.kind)?;
            }
        }
        for relation in &c.relations {
            relation.validate()?;
        }
        Ok(())
    };
    validate().map_err(|e| Error::Storage(format!("invalid collection metadata: {e}")))?;
    Ok(c)
}
pub(crate) fn compatible_index(
    c: &Collection,
    path: &[String],
    kind: crate::IndexKind,
) -> Result<()> {
    for field in &c.fields {
        let incompatible = if field.path == path {
            match kind {
                crate::IndexKind::Scalar => matches!(
                    field.kind,
                    FieldType::Object | FieldType::Array | FieldType::Vector(_)
                ),
                crate::IndexKind::Array => !matches!(field.kind, FieldType::Array),
                crate::IndexKind::Spatial => !matches!(field.kind, FieldType::Object),
                crate::IndexKind::FullText => !matches!(field.kind, FieldType::String),
                crate::IndexKind::Vector => !matches!(field.kind, FieldType::Vector(_)),
            }
        } else if path.starts_with(&field.path) {
            !matches!(field.kind, FieldType::Object)
        } else {
            kind != crate::IndexKind::Spatial && field.path.starts_with(path)
        };
        if incompatible {
            return Err(Error::Validation(format!(
                "field {} is incompatible with index path {}",
                field.path.join("."),
                path.join(".")
            )));
        }
    }
    if kind == crate::IndexKind::Array
        && c.field_policies.iter().any(|policy| {
            policy.path == path
                && matches!(
                    policy.options.element_type,
                    Some(FieldType::Object | FieldType::Array | FieldType::Vector(_))
                )
        })
    {
        return Err(Error::Validation(
            "array index requires scalar or record element types".into(),
        ));
    }
    Ok(())
}
fn strings(values: &[String]) -> Value {
    Value::Array(values.iter().cloned().map(Value::String).collect())
}
fn object(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Object(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
fn index_info(index: &crate::Index, table: &str) -> Value {
    let mut fields = vec![
        ("name", Value::String(index.name.clone())),
        ("table", Value::String(table.into())),
        ("model", Value::String("document".into())),
        ("path", strings(&index.path)),
        ("unique", Value::Boolean(index.unique)),
        (
            "kind",
            Value::String(
                if index.kind == crate::IndexKind::Array {
                    "array"
                } else if index.kind == crate::IndexKind::Spatial {
                    "spatial"
                } else if index.kind == crate::IndexKind::FullText {
                    "fulltext"
                } else if index.kind == crate::IndexKind::Vector {
                    "vector"
                } else {
                    "scalar"
                }
                .into(),
            ),
        ),
    ];
    if index.scalar.is_some() {
        fields.push(("paths", Value::Array(index.paths().map(strings).collect())));
    }
    if let Some(config) = &index.fulltext {
        fields.push(("paths", Value::Array(index.paths().map(strings).collect())));
        fields.push(("tokenizer", Value::String(config.tokenizer.clone())));
        if let Ok(options) = config.options() {
            fields.push(("analyzer", Value::String(options.tokenizer)));
            fields.push((
                "min_gram",
                options
                    .min_gram
                    .map_or(Value::Null, |n| Value::Integer(n as i64)),
            ));
            fields.push((
                "max_gram",
                options
                    .max_gram
                    .map_or(Value::Null, |n| Value::Integer(n as i64)),
            ));
        }
    }
    if index.kind == crate::IndexKind::Spatial {
        let geometry = if index.region_index() {
            "region"
        } else {
            "point"
        };
        let format = if index.spatial.is_some() {
            "two_dimensional"
        } else {
            "latitude"
        };
        fields.push(("geometry", Value::String(geometry.into())));
        fields.push(("format", Value::String(format.into())));
    }
    if let Some(config) = &index.vector {
        fields.push(("dimensions", Value::Integer(config.dimensions as i64)));
        fields.push(("metric", Value::String(config.metric.clone())));
        fields.push(("implementation", Value::String(config.format.clone())));
        fields.push(("quantization", Value::String(config.quantization().into())));
    }
    object(fields)
}
// One connection-validation transaction observes one schema snapshot. Retain
// every row so duplicate names and unexpected dependencies still reject.
struct SchemaSnapshot {
    rows: Vec<Vec<EngineValue>>,
}
impl SchemaSnapshot {
    fn schema_object(&self, name: &str, kind: &str, table: &str, sql: &str) -> Result<()> {
        let rows: Vec<_> = self.rows.iter().filter(|row| {
            matches!(row.get(1), Some(EngineValue::Text(actual)) if actual.as_str() == name)
        }).collect();
        let valid = match rows.as_slice() {
            [row] => match row.as_slice() {
                [EngineValue::Text(actual_kind), _, EngineValue::Text(actual_table), EngineValue::Text(actual_sql)] => {
                    actual_kind.as_str() == kind
                        && actual_table.as_str() == table
                        && schema_tokens(actual_sql.as_str())? == schema_tokens(sql)?
                }
                _ => false,
            },
            _ => false,
        };
        if !valid {
            return Err(Error::Storage(format!(
                "missing or incompatible managed schema object {name}"
            )));
        }
        Ok(())
    }
    fn managed_index_set(&self, table: &str, expected: &[&str]) -> Result<()> {
        for row in &self.rows {
            let [EngineValue::Text(kind), name, EngineValue::Text(actual_table), sql] =
                row.as_slice()
            else {
                continue;
            };
            if actual_table.as_str() != table
                || !(kind.as_str() == "trigger"
                    || (kind.as_str() == "index" && !matches!(sql, EngineValue::Null)))
            {
                continue;
            }
            if !matches!(name, EngineValue::Text(name) if kind.as_str() == "index" && expected.contains(&name.as_str()))
            {
                return Err(Error::Storage(format!(
                    "unexpected dependency on managed table {table}"
                )));
            }
        }
        Ok(())
    }
    fn managed_dependencies(&self, table: &str, expected_index: Option<&str>) -> Result<()> {
        self.managed_index_dependencies(table, expected_index.as_slice())
    }
    fn managed_index_dependencies(&self, table: &str, expected: &[&str]) -> Result<()> {
        for row in &self.rows {
            let [EngineValue::Text(kind), name, EngineValue::Text(actual_table), sql] =
                row.as_slice()
            else {
                continue;
            };
            if actual_table.as_str() != table
                || !(kind.as_str() == "trigger"
                    || (kind.as_str() == "index" && !matches!(sql, EngineValue::Null)))
            {
                continue;
            }
            if !matches!(name, EngineValue::Text(name) if kind.as_str() == "index" && expected.contains(&name.as_str()))
            {
                return Err(Error::Storage(format!(
                    "unexpected dependency on managed table {table}"
                )));
            }
        }
        Ok(())
    }
}
impl Connection {
    pub(crate) fn validate_storage_schema(&self) -> Result<()> {
        let schema = SchemaSnapshot {
            rows: self.run("SELECT type,name,tbl_name,sql FROM sqlite_schema", &[])?,
        };
        schema.schema_object("__romedb_catalog", "table", "__romedb_catalog", "CREATE TABLE IF NOT EXISTS __romedb_catalog (name TEXT PRIMARY KEY, metadata TEXT NOT NULL)")?;
        schema.managed_dependencies("__romedb_catalog", None)?;
        schema.schema_object(
            "__romedb_functions",
            "table",
            "__romedb_functions",
            crate::udf::DDL,
        )?;
        schema.managed_dependencies("__romedb_functions", None)?;
        self.validate_functions()?;
        let mut storage = std::collections::BTreeSet::new();
        let collections = self.collections()?;
        crate::graph::validate_catalog(&collections, self.graph_marker_present()?)?;
        for c in collections {
            storage.insert(c.storage.clone());
            if let Some(edge) = &c.edge {
                for (name, kind, ddl) in edge.schema() {
                    if !storage.insert(name.clone()) {
                        return Err(Error::Storage(format!(
                            "graph storage {name} has multiple owners"
                        )));
                    }
                    schema.schema_object(&name, kind, &edge.adjacency, &ddl)?;
                }
                schema.managed_index_set(
                    &edge.adjacency,
                    &[&edge.outgoing_index(), &edge.incoming_index()],
                )?;
            }
            schema.schema_object(
                &c.storage,
                "table",
                &c.storage,
                &format!(
                    "CREATE TABLE {} (id BLOB PRIMARY KEY, doc BLOB NOT NULL)",
                    quote(&c.storage)
                ),
            )?;
            schema.managed_dependencies(&c.storage, None)?;
            for index in &c.indexes {
                if !storage.insert(index.storage.clone()) {
                    return Err(Error::Storage(format!(
                        "managed index storage {} has multiple owners",
                        index.storage
                    )));
                }
                schema.schema_object(
                    &index.storage,
                    "table",
                    &index.storage,
                    &index.table_ddl(),
                )?;
                schema.schema_object(&index.name, "index", &index.storage, &index.index_ddl())?;
                let mut expected = vec![index.name.clone()];
                for object in index.extra_schema() {
                    if object.kind == "table" && !storage.insert(object.name.clone()) {
                        return Err(Error::Storage(format!(
                            "managed index storage {} has multiple owners",
                            object.name
                        )));
                    }
                    schema.schema_object(&object.name, object.kind, &object.table, &object.ddl)?;
                    if object.kind == "index" && object.table == index.storage {
                        expected.push(object.name.clone());
                    } else if object.kind == "table" {
                        schema.managed_dependencies(&object.name, None)?;
                    }
                }
                let expected = expected.iter().map(String::as_str).collect::<Vec<_>>();
                schema.managed_index_dependencies(&index.storage, &expected)?;
                if index.kind == crate::IndexKind::Vector {
                    for (name, ddl) in [
                        (index.ann_state(), index.ann_state_ddl()),
                        (index.ann_log(), index.ann_log_ddl()),
                    ] {
                        if !storage.insert(name.clone()) {
                            return Err(Error::Storage("multiple vector storage owners".into()));
                        }
                        schema.schema_object(&name, "table", &name, &ddl)?;
                        schema.managed_dependencies(&name, None)?;
                    }
                    self.ann_generation(index)?;
                }
                if index.kind == crate::IndexKind::FullText {
                    let stats = index.text_stats();
                    let dir = index.text_directory();
                    let dir_index = format!("{dir}_key");
                    let legacy = index
                        .fulltext
                        .as_ref()
                        .is_some_and(|c| c.storage_version == 1);
                    let names = if legacy {
                        vec![&stats, &dir, &dir_index]
                    } else {
                        vec![&dir, &dir_index]
                    };
                    for name in names {
                        if !storage.insert(name.clone()) {
                            return Err(Error::Storage("multiple full-text storage owners".into()));
                        }
                    }
                    if legacy {
                        schema.schema_object(&stats, "table", &stats, &index.text_stats_ddl())?;
                        schema.managed_dependencies(&stats, None)?;
                        self.text_count(index)?;
                    }
                    schema.schema_object(&dir, "table", &dir, &index.text_directory_ddl())?;
                    schema.schema_object(
                        &dir_index,
                        "index",
                        &dir,
                        &index.text_directory_index_ddl(),
                    )?;
                    schema.managed_dependencies(&dir, Some(&dir_index))?;
                }
            }
        }
        for row in &schema.rows {
            let [_, EngineValue::Text(name), _, _] = row.as_slice() else {
                return Err(Error::Storage("invalid schema name".into()));
            };
            let canonical = name.as_str().to_ascii_lowercase();
            if (canonical.starts_with("__romedb_c_")
                || canonical.starts_with("__romedb_i_")
                || canonical.starts_with("__romedb_g_")
                || canonical.starts_with("__turso_internal_fts_dir_"))
                && !storage.contains(name.as_str())
            {
                return Err(Error::Storage(format!(
                    "orphan managed storage object {}",
                    name.as_str()
                )));
            }
        }
        crate::relations::validate_dependencies(&self.collections()?)?;
        Ok(())
    }
    pub(super) fn schema_object(
        &self,
        name: &str,
        kind: &str,
        table: &str,
        sql: &str,
    ) -> Result<()> {
        let rows = self.run(
            "SELECT type,tbl_name,sql FROM sqlite_schema WHERE name=?1",
            &[text(name)],
        )?;
        let valid = match rows.as_slice() {
            [row] => match row.as_slice() {
                [EngineValue::Text(actual_kind), EngineValue::Text(actual_table), EngineValue::Text(actual_sql)] => {
                    actual_kind.as_str() == kind
                        && actual_table.as_str() == table
                        && schema_tokens(actual_sql.as_str())? == schema_tokens(sql)?
                }
                _ => false,
            },
            _ => false,
        };
        if !valid {
            return Err(Error::Storage(format!(
                "missing or incompatible managed schema object {name}"
            )));
        }
        Ok(())
    }
    pub(super) fn managed_dependencies(
        &self,
        table: &str,
        expected_index: Option<&str>,
    ) -> Result<()> {
        for row in self.run("SELECT name,type FROM sqlite_schema WHERE tbl_name=?1 AND (type='trigger' OR (type='index' AND sql IS NOT NULL))", &[text(table)])? {
            if !matches!(row.as_slice(), [EngineValue::Text(name), EngineValue::Text(kind)] if kind.as_str()=="index" && Some(name.as_str())==expected_index) {
                return Err(Error::Storage(format!("unexpected dependency on managed table {table}")));
            }
        }
        Ok(())
    }
    pub(crate) fn collections(&self) -> Result<Vec<Collection>> {
        self.run(
            "SELECT name,metadata FROM __romedb_catalog WHERE name != ?1 ORDER BY name",
            &[text(crate::graph::MARKER)],
        )?
        .into_iter()
        .map(|row| match row.as_slice() {
            [EngineValue::Text(name), EngineValue::Text(metadata)] => {
                decode(metadata.as_str(), name.as_str())
            }
            _ => Err(Error::Storage("invalid catalog entry".into())),
        })
        .collect()
    }
    pub fn upsert(&self, table: &str, mut doc: Document) -> Result<Document> {
        self.atomic(|| {
            let c = self.collection_for_write(table)?;
            crate::normalize_document_id(&c, &mut doc)?;
            let Value::Record(id) = &doc["id"] else {
                unreachable!("normalized id");
            };
            if let Some(mut existing) = self.get_in(&c, id)? {
                existing.extend(doc);
                self.replace_document(&c, &mut existing)?;
                Ok(existing)
            } else {
                self.insert(&c.name, doc)
            }
        })
    }
    pub fn remove_field(&self, table: &str, path: &[String]) -> Result<()> {
        crate::validate_path(path)?;
        self.atomic(|| {
            let mut c = self.catalog(table)?;
            let position = c
                .fields
                .iter()
                .position(|f| f.path == path)
                .ok_or_else(|| Error::NotFound(path.join(".")))?;
            c.fields.remove(position);
            c.field_policies.retain(|policy| policy.path != path);
            if c.strict {
                crate::field_rules::validate_catalog(&c)?;
                for document in self.documents(&c)? {
                    self.validate_candidate(&c, &document)?;
                    self.validate_computed_fields(&c, &document)?;
                }
            }
            self.save_catalog(&c)
        })
    }
    pub fn drop_collection(&self, name: &str, if_exists: bool) -> Result<()> {
        self.atomic(|| {
            let c = match self.catalog(name) {
                Ok(c) => c,
                Err(Error::NotFound(_)) if if_exists => return Ok(()),
                Err(e) => return Err(e),
            };
            for owner in self.collections()? {
                if owner.name != c.name && owner.relations.iter().any(|r| r.source == c.name) {
                    return Err(Error::Validation(
                        "drop dependent relations before their source collection".into(),
                    ));
                }
            }
            self.drop_edge_metadata(&c)?;
            for index in &c.indexes {
                self.drop_index_storage(index)?;
            }
            self.run(&format!("DROP TABLE {}", quote(&c.storage)), &[])?;
            self.run(
                "DELETE FROM __romedb_catalog WHERE name=?1",
                &[text(&c.name)],
            )?;
            Ok(())
        })
    }
    /// Returns false when the name is not a managed index, allowing SQL dispatch
    /// to preserve the baseline operation for an ordinary relational index.
    pub fn drop_managed_index(&self, name: &str) -> Result<bool> {
        let name = canonical(name)?;
        self.atomic(|| {
            for mut c in self.collections()? {
                if let Some(position) = c.indexes.iter().position(|i| i.name == name) {
                    if self
                        .collections()?
                        .iter()
                        .any(|owner| owner.relations.iter().any(|r| r.index == name))
                    {
                        return Err(Error::Validation(
                            "drop dependent relations before their index".into(),
                        ));
                    }
                    let index = c.indexes.remove(position);
                    self.drop_index_storage(&index)?;
                    self.save_catalog(&c)?;
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }
    pub(crate) fn catalog_statement(&self, sql: &str) -> Result<Option<QueryResult>> {
        let Ok(Cmd::Stmt(stmt)) = crate::select::parsed(sql) else {
            return Ok(None);
        };
        match stmt {
            Stmt::Reindex { name: Some(name) }
                if name
                    .db_name
                    .as_ref()
                    .is_none_or(|db| db.as_str().eq_ignore_ascii_case("main")) =>
            {
                let managed = self.collections()?.iter().any(|collection| {
                    collection
                        .indexes
                        .iter()
                        .any(|index| index.name.eq_ignore_ascii_case(name.name.as_str()))
                });
                if managed {
                    self.reindex(name.name.as_str())?;
                    return Ok(Some(QueryResult::command(0)));
                }
                Ok(None)
            }
            Stmt::DropTable {
                tbl_name,
                if_exists,
            } => {
                if tbl_name
                    .db_name
                    .as_ref()
                    .is_some_and(|n| !n.as_str().eq_ignore_ascii_case("main"))
                {
                    return Ok(None);
                }
                match self.catalog(tbl_name.name.as_str()) {
                    Ok(_) => {
                        self.drop_collection(tbl_name.name.as_str(), if_exists)?;
                        Ok(Some(QueryResult::command(0)))
                    }
                    Err(Error::NotFound(_)) => Ok(None),
                    Err(e) => Err(e),
                }
            }
            Stmt::DropIndex { idx_name, .. } => {
                if idx_name
                    .db_name
                    .as_ref()
                    .is_some_and(|n| !n.as_str().eq_ignore_ascii_case("main"))
                {
                    return Ok(None);
                }
                if self.drop_managed_index(idx_name.name.as_str())? {
                    Ok(Some(QueryResult::command(0)))
                } else {
                    Ok(None)
                }
            }
            Stmt::CreateTable {
                if_not_exists,
                temporary,
                tbl_name,
                ..
            } if !temporary
                && tbl_name
                    .db_name
                    .as_ref()
                    .is_none_or(|n| n.as_str().eq_ignore_ascii_case("main")) =>
            {
                match self.catalog(tbl_name.name.as_str()) {
                    Ok(_) if if_not_exists => Ok(Some(QueryResult::command(0))),
                    Ok(_) => Err(Error::AlreadyExists(tbl_name.name.as_str().into())),
                    Err(Error::NotFound(_)) => Ok(None),
                    Err(e) => Err(e),
                }
            }
            _ => Ok(None),
        }
    }
    pub fn info(&self, scope: &str, name: Option<&str>) -> Result<QueryResult> {
        self.atomic(|| {
            let info=match (scope,name) {
                ("db",None)=>{
                    let mut entries=Vec::new();
                    for c in self.collections()? {
                    let mut entry=vec![("name",Value::String(c.name)),("model",Value::String("document".into()))];
                    if let Some(edge)=c.edge {entry.push(("kind",Value::String("edge".into())));entry.push(("source",Value::String(edge.source)));entry.push(("target",Value::String(edge.target)));}
                    entries.push(object(entry));
                }
                    for row in self.run("SELECT name FROM sqlite_schema WHERE type='table' AND substr(lower(name),1,9) != '__romedb_' AND substr(lower(name),1,7) != 'sqlite_' AND substr(lower(name),1,17) != '__turso_internal_' ORDER BY name",&[])? {
                        if let EngineValue::Text(t)=&row[0] {entries.push(object([("name",Value::String(t.as_str().into())),("model",Value::String("relational".into()))]));}
                    }
                    let views = self.run("SELECT name FROM sqlite_schema WHERE type='view' AND substr(lower(name),1,9) != '__romedb_' AND substr(lower(name),1,7) != 'sqlite_' AND substr(lower(name),1,17) != '__turso_internal_' ORDER BY name", &[])?.into_iter().map(|row| object([("name", crate::from_engine(row[0].clone())), ("model", Value::String("relational".into()))])).collect();
                    object([("catalog_version",Value::Integer(i64::from(version()))),("tables",Value::Array(entries)),("views",Value::Array(views))])
                }
                ("table",Some(name))=>self.table_info(name)?,
                ("index",Some(name))=>self.logical_index_info(name)?,
                ("relation",Some(name))=>self.relation_info(name)?,
                ("function",Some(name))=>self.function_info(name)?,
                _=>return Err(Error::Validation("INFO requires DB, TABLE name, INDEX name, or RELATION name".into())),
            };
            Ok(QueryResult {columns:vec!["info".into()],rows:vec![vec![info]],affected:0})
        })
    }
    fn table_info(&self, name: &str) -> Result<Value> {
        let name = canonical(name)?;
        match self.catalog(&name) {
            Ok(c) => {
                let fields = c
                    .fields
                    .iter()
                    .map(|f| {
                        let options = c
                            .field_policies
                            .iter()
                            .find(|policy| policy.path == f.path)
                            .map(|policy| &policy.options);
                        object([
                            ("path", strings(&f.path)),
                            (
                                "type",
                                Value::String(match &f.kind {
                                    FieldType::String => "string".into(),
                                    FieldType::Integer => "integer".into(),
                                    FieldType::Number => "number".into(),
                                    FieldType::Boolean => "boolean".into(),
                                    FieldType::Object => "object".into(),
                                    FieldType::Array => "array".into(),
                                    FieldType::Record(target) => format!("record<{target}>"),
                                    FieldType::Vector(dims) => format!("vector<{dims}>"),
                                }),
                            ),
                            (
                                "computed",
                                options
                                    .and_then(|options| options.computed.as_ref())
                                    .map(|sql| Value::String(sql.clone()))
                                    .unwrap_or(Value::Null),
                            ),
                            ("required", Value::Boolean(f.required)),
                            ("nullable", Value::Boolean(f.nullable)),
                            (
                                "has_default",
                                Value::Boolean(
                                    options.is_some_and(|options| options.default.is_some()),
                                ),
                            ),
                            (
                                "default",
                                options
                                    .and_then(|options| options.default.clone())
                                    .unwrap_or(Value::Null),
                            ),
                            (
                                "readonly",
                                Value::Boolean(options.is_some_and(|options| options.readonly)),
                            ),
                            (
                                "flexible",
                                Value::Boolean(options.is_some_and(|options| options.flexible)),
                            ),
                            (
                                "element_type",
                                options
                                    .and_then(|options| options.element_type.as_ref())
                                    .map(|kind| Value::String(crate::field_rules::type_name(kind)))
                                    .unwrap_or(Value::Null),
                            ),
                            (
                                "element_nullable",
                                Value::Boolean(
                                    options.is_some_and(|options| options.element_nullable),
                                ),
                            ),
                            (
                                "check",
                                f.check
                                    .as_ref()
                                    .map_or(Value::Null, |s| Value::String(s.clone())),
                            ),
                        ])
                    })
                    .collect();
                let indexes = c.indexes.iter().map(|i| index_info(i, &c.name)).collect();
                let relations = c
                    .relations
                    .iter()
                    .map(|r| Value::String(r.name.clone()))
                    .collect();
                let mut info = vec![
                    ("name", Value::String(c.name)),
                    ("model", Value::String("document".into())),
                    ("strict", Value::Boolean(c.strict)),
                    ("fields", Value::Array(fields)),
                    ("indexes", Value::Array(indexes)),
                    ("relations", Value::Array(relations)),
                    (
                        "capabilities",
                        strings(&[
                            "typed_ids".into(),
                            "field_validation".into(),
                            "scalar_indexes".into(),
                            "transactions".into(),
                        ]),
                    ),
                ];
                if let Some(edge) = &c.edge {
                    info.push(("kind", Value::String("edge".into())));
                    info.push((
                        "edge",
                        object([
                            ("source", Value::String(edge.source.clone())),
                            ("target", Value::String(edge.target.clone())),
                            (
                                "on_delete",
                                Value::String(
                                    if edge.cascade { "cascade" } else { "restrict" }.into(),
                                ),
                            ),
                            (
                                "adjacency_indexes",
                                strings(&["outgoing".into(), "incoming".into()]),
                            ),
                        ]),
                    ));
                }
                if !c.edge_dependents.is_empty() {
                    info.push(("edge_tables", strings(&c.edge_dependents)));
                }
                Ok(object(info))
            }
            Err(Error::NotFound(_)) => {
                let schema = self.run("SELECT type FROM sqlite_schema WHERE type IN ('table','view') AND name=?1 COLLATE NOCASE", &[text(&name)])?;
                let Some(row) = schema.first() else {
                    return Err(Error::NotFound(name));
                };
                Ok(object([
                    ("name", Value::String(name.clone())),
                    ("model", Value::String("relational".into())),
                    ("kind", crate::from_engine(row[0].clone())),
                    ("columns", self.pragma_info("table_xinfo", &name)?),
                    ("indexes", self.pragma_info("index_list", &name)?),
                ]))
            }
            Err(e) => Err(e),
        }
    }
    fn pragma_info(&self, pragma: &str, name: &str) -> Result<Value> {
        let result = self.sql(
            &format!("PRAGMA {pragma}({})", quote(name)),
            &Parameters::new(),
        )?;
        Ok(Value::Array(
            result
                .rows
                .into_iter()
                .map(|row| object_row(&result.columns, row))
                .collect(),
        ))
    }
    fn logical_index_info(&self, name: &str) -> Result<Value> {
        let name = canonical(name)?;
        for c in self.collections()? {
            if let Some(index) = c.indexes.iter().find(|i| i.name == name) {
                return Ok(index_info(index, &c.name));
            }
        }
        let rows = self.run(
            "SELECT tbl_name, sql FROM sqlite_schema WHERE type='index' AND name=?1 COLLATE NOCASE",
            &[text(&name)],
        )?;
        let Some(row) = rows.first() else {
            return Err(Error::NotFound(name));
        };
        Ok(object([
            ("name", Value::String(name.clone())),
            ("model", Value::String("relational".into())),
            ("table", crate::from_engine(row[0].clone())),
            ("sql", crate::from_engine(row[1].clone())),
            ("columns", self.pragma_info("index_xinfo", &name)?),
        ]))
    }
}
fn object_row(names: &[String], row: Vec<Value>) -> Value {
    Value::Object(names.iter().cloned().zip(row).collect())
}

#[cfg(test)]
mod metadata_tests {
    use super::*;
    #[test]
    fn malformed_metadata_cannot_redirect_storage_or_weaken_definitions() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let q = |sql: &str| c.execute(sql, &Parameters::new()).unwrap();
        q("CREATE TABLE docs");
        q("DEFINE FIELD value ON docs TYPE integer REQUIRED");
        q("CREATE UNIQUE INDEX value_idx ON docs(value)");
        q("INSERT INTO docs {value:1}");
        q("CREATE TABLE sentinel(value INTEGER)");
        q("INSERT INTO sentinel VALUES (99)");
        let original = serde_json::to_value(c.catalog("docs").unwrap()).unwrap();
        let mutations: Vec<(&str, serde_json::Value)> = vec![
            ("/name", "other".into()),
            ("/storage", "sentinel".into()),
            ("/storage", "__romedb_c_6f74686572".into()),
            ("/fields/0/path", serde_json::json!([])),
            ("/fields/0/path", serde_json::json!(["id"])),
            (
                "/fields/0/kind",
                serde_json::json!({"Record":"__romedb_bad"}),
            ),
            ("/fields/0/kind", serde_json::json!({"Vector":0})),
            ("/fields/0/kind", "Object".into()),
            (
                "/fields",
                serde_json::json!([original["fields"][0], original["fields"][0]]),
            ),
            ("/indexes/0/name", "VALUE_IDX".into()),
            ("/indexes/0/storage", "sentinel".into()),
            ("/indexes/0/path", serde_json::json!([""])),
            (
                "/indexes",
                serde_json::json!([original["indexes"][0], original["indexes"][0]]),
            ),
        ];
        let mut invalid = vec!["{".to_owned(), "null".into()];
        for (path, value) in mutations {
            let mut metadata = original.clone();
            *metadata.pointer_mut(path).unwrap() = value;
            invalid.push(metadata.to_string());
        }
        let mut nested = original.clone();
        nested["fields"][0]["check"] =
            format!("{}value>0{}", "(".repeat(128), ")".repeat(128)).into();
        invalid.push(nested.to_string());
        for metadata in invalid {
            c.run(
                "UPDATE __romedb_catalog SET metadata=?1 WHERE name='docs'",
                &[text(&metadata)],
            )
            .unwrap();
            assert_eq!(
                c.catalog("docs").unwrap_err().code(),
                "FDB_STORAGE",
                "{metadata}"
            );
            assert_eq!(
                c.collections().unwrap_err().code(),
                "FDB_STORAGE",
                "{metadata}"
            );
            assert_eq!(
                c.execute("DROP TABLE docs", &Parameters::new())
                    .unwrap_err()
                    .code(),
                "FDB_STORAGE",
                "{metadata}"
            );
            assert_eq!(
                c.run("SELECT value FROM sentinel", &[]).unwrap(),
                vec![vec![EngineValue::from_i64(99)]]
            );
        }
        c.run(
            "UPDATE __romedb_catalog SET metadata=?1 WHERE name='docs'",
            &[text(&original.to_string())],
        )
        .unwrap();
        assert_eq!(
            c.lookup_index("docs", "value_idx", &Value::Integer(1))
                .unwrap()
                .len(),
            1
        );
        assert!(c
            .execute("INSERT INTO docs {value:1.5}", &Parameters::new())
            .is_err());
        let mut legacy = original;
        legacy.as_object_mut().unwrap().remove("version");
        c.run(
            "UPDATE __romedb_catalog SET metadata=?1 WHERE name='docs'",
            &[text(&legacy.to_string())],
        )
        .unwrap();
        assert_eq!(c.catalog("docs").unwrap().version, 1);
        assert_eq!(
            c.lookup_index("docs", "value_idx", &Value::Integer(1))
                .unwrap()
                .len(),
            1
        );
        let reference = |target: &str| crate::Field {
            path: vec!["parent".into()],
            kind: FieldType::Record(target.into()),
            required: false,
            nullable: true,
            check: None,
        };
        assert_eq!(
            c.define_field("docs", reference("__romedb_bad"), false)
                .unwrap_err()
                .code(),
            "FDB_VALIDATION"
        );
        c.define_field("docs", reference("Docs"), false).unwrap();
        assert!(
            matches!(&c.catalog("docs").unwrap().fields[1].kind,FieldType::Record(target) if target=="Docs")
        );
    }
}

#[cfg(test)]
mod storage_schema_tests {
    use super::*;
    #[test]
    fn new_connections_reject_missing_or_modified_managed_schema() {
        for mutation in [
            "DROP TABLE __romedb_c_646f6373",
            "ALTER TABLE __romedb_c_646f6373 ADD COLUMN extra TEXT",
            "DROP TABLE __romedb_i_76616c75655f696478",
            "DROP INDEX value_idx",
            "CREATE UNIQUE INDEX extra ON __romedb_c_646f6373(doc)",
            "CREATE TRIGGER extra AFTER INSERT ON __romedb_c_646f6373 BEGIN INSERT INTO sentinel VALUES (100); END",
            "ALTER TABLE __romedb_catalog ADD COLUMN extra TEXT",
            "CREATE TRIGGER extra AFTER UPDATE ON __romedb_catalog BEGIN INSERT INTO sentinel VALUES (100); END",
        ] {
            let db=crate::Database::open(":memory:").unwrap();
            let c=db.connect().unwrap();
            for sql in ["CREATE TABLE docs", "CREATE UNIQUE INDEX value_idx ON docs(value)", "INSERT INTO docs {value:1}", "CREATE TABLE sentinel(value INTEGER)", "INSERT INTO sentinel VALUES (99)"] {
                c.execute(sql,&Parameters::new()).unwrap();
            }
            drop(db.connect().expect("valid managed schema"));
            c.run(mutation,&[]).unwrap();
            let error=db.connect().err().expect("reject modified schema");
            assert_eq!(error.code(),"FDB_STORAGE","{mutation}: {error}");
            assert_eq!(c.execute("SELECT * FROM sentinel",&Parameters::new()).unwrap().rows,vec![vec![Value::Integer(99)]]);
        }
    }
    #[test]
    fn changed_unique_index_definition_is_rejected() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE docs", &Parameters::new()).unwrap();
        c.execute(
            "CREATE UNIQUE INDEX value_idx ON docs(value)",
            &Parameters::new(),
        )
        .unwrap();
        c.run("DROP INDEX value_idx", &[]).unwrap();
        c.run(
            "CREATE INDEX value_idx ON __romedb_i_76616c75655f696478(key)",
            &[],
        )
        .unwrap();
        assert_eq!(db.connect().err().unwrap().code(), "FDB_STORAGE");
        c.run("DROP INDEX value_idx", &[]).unwrap();
        c.run(
            "CREATE UNIQUE INDEX \"value_idx\" ON \"__romedb_i_76616c75655f696478\" (key)",
            &[],
        )
        .unwrap();
        drop(db.connect().expect("restored schema"));
    }
}

#[cfg(test)]
mod orphan_schema_tests {
    use super::*;
    #[test]
    fn managed_index_storage_cannot_be_shared_by_collections() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE docs",
            "CREATE TABLE other",
            "CREATE INDEX value_idx ON docs(value)",
            "INSERT INTO docs {value:1}",
        ] {
            c.execute(sql, &Parameters::new()).unwrap();
        }
        let mut other = c.catalog("other").unwrap();
        other.indexes = c.catalog("docs").unwrap().indexes;
        c.save_catalog(&other).unwrap();
        let error = db
            .connect()
            .err()
            .expect("shared index storage must reject connection");
        assert_eq!(error.code(), "FDB_STORAGE");
        assert!(error.to_string().contains("multiple owners"), "{error}");
        other.indexes.clear();
        c.save_catalog(&other).unwrap();
        drop(db.connect().expect("restored ownership"));
    }

    #[test]
    fn removed_metadata_and_orphan_reserved_objects_fail_connection_validation() {
        for mutation in [
            "DELETE FROM __romedb_catalog WHERE name='docs'",
            "UPDATE __romedb_catalog SET metadata=json_set(metadata,'$.indexes',json('[]')) WHERE name='docs'",
            "CREATE TABLE __romedb_c_orphan(value)",
            "CREATE VIEW __romedb_i_orphan AS SELECT 1",
            "CREATE TABLE __ROMEDB_C_ORPHAN(value)",
        ] {
            let db=crate::Database::open(":memory:").unwrap();
            let c=db.connect().unwrap();
            for sql in ["CREATE TABLE docs", "CREATE INDEX value_idx ON docs(value)", "INSERT INTO docs {value:1}"] {
                c.execute(sql,&Parameters::new()).unwrap();
            }
            c.run(mutation,&[]).unwrap();
            let error=db.connect().err().expect("orphan must reject connection");
            assert_eq!(error.code(),"FDB_STORAGE","{error}");
            assert!(error.to_string().contains("orphan managed storage"),"{mutation}: {error}");
            assert_eq!(c.run("SELECT count(*) FROM __romedb_c_646f6373",&[]).unwrap()[0][0], crate::scalar(&Value::Integer(1)).unwrap());
        }
    }
}
