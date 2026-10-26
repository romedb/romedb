use crate::{
    quote, Collection, Connection, Document, EngineValue, Error, Index, IndexKind, Result, Value,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub additional_paths: Vec<Vec<String>>,
}

impl Index {
    pub(crate) fn validate_scalar_config(&self) -> Result<()> {
        let Some(config) = &self.scalar else {
            return Ok(());
        };
        if self.kind != IndexKind::Scalar
            || config.additional_paths.is_empty()
            || config.additional_paths.len() > 15
        {
            return Err(Error::Validation(
                "compound scalar index requires 2..16 paths".into(),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for path in std::iter::once(&self.path).chain(&config.additional_paths) {
            crate::validate_path(path)?;
            if !seen.insert(path) {
                return Err(Error::Validation("duplicate compound index path".into()));
            }
        }
        Ok(())
    }
    pub(crate) fn scalar_columns(&self) -> Vec<String> {
        std::iter::once("\"key\"".to_owned())
            .chain((1..self.paths().count()).map(|i| format!("f{i}")))
            .collect()
    }
    pub(crate) fn scalar_keys(&self, doc: &Document) -> Result<Vec<EngineValue>> {
        self.paths()
            .map(|path| crate::index_scalar(crate::path_value(doc, path)?.unwrap_or(&Value::Null)))
            .collect()
    }
}

impl Connection {
    pub fn create_compound_index(
        &self,
        table: &str,
        name: &str,
        paths: Vec<Vec<String>>,
        unique: bool,
        if_not_exists: bool,
    ) -> Result<()> {
        self.create_index_paths(table, name, paths, unique, if_not_exists, IndexKind::Scalar)
    }
    pub fn lookup_compound_index(
        &self,
        table: &str,
        name: &str,
        values: &[Value],
    ) -> Result<Vec<Document>> {
        self.atomic(|| {
            let collection = self.catalog(table)?;
            let index = collection
                .indexes
                .iter()
                .find(|index| index.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| Error::NotFound(name.into()))?;
            if index.kind != IndexKind::Scalar
                || index.scalar.is_none()
                || values.len() != index.paths().count()
            {
                return Err(Error::Validation(
                    "compound lookup requires one value per compound scalar index path".into(),
                ));
            }
            let keys = values
                .iter()
                .map(crate::index_scalar)
                .collect::<Result<Vec<_>>>()?;
            self.lookup_scalar_keys(&collection, index, &keys)
        })
    }
    pub(crate) fn lookup_scalar_keys(
        &self,
        collection: &Collection,
        index: &Index,
        keys: &[EngineValue],
    ) -> Result<Vec<Document>> {
        if keys.len() != index.paths().count() {
            return Err(Error::Storage("scalar index key count mismatch".into()));
        }
        let predicate = index
            .scalar_columns()
            .iter()
            .enumerate()
            .map(|(i, column)| format!("i.{column}=?{}", i + 1))
            .collect::<Vec<_>>()
            .join(" AND ");
        let rows=self.run_customer(&format!("SELECT c.doc FROM {} AS i INDEXED BY {} JOIN {} AS c ON c.id=i.id WHERE {predicate}",quote(&index.storage),quote(&index.name),quote(&collection.storage)),keys)?;
        rows.iter()
            .map(|row| crate::decode_document(&row[0]))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compound_metadata_and_every_key_component_are_audited() {
        let c = crate::Database::open(":memory:")
            .unwrap()
            .connect()
            .unwrap();
        c.execute("INSERT INTO docs {id:docs:a,a:1,b:2}", &Default::default())
            .unwrap();
        c.execute("CREATE UNIQUE INDEX pair ON docs(a,b)", &Default::default())
            .unwrap();
        let collection = c.catalog("docs").unwrap();
        assert_eq!(collection.version, 5);
        let index = &collection.indexes[0];
        let metadata = serde_json::to_value(&collection).unwrap();
        for (path, value) in [
            ("/version", serde_json::json!(4)),
            ("/indexes/0/scalar/additional_paths", serde_json::json!([])),
            (
                "/indexes/0/scalar/additional_paths",
                serde_json::json!([["a"]]),
            ),
            ("/indexes/0/kind", serde_json::json!("vector")),
        ] {
            let mut changed = metadata.clone();
            if path == "/indexes/0/kind" {
                changed["indexes"][0]["kind"] = value;
            } else {
                *changed.pointer_mut(path).unwrap() = value;
            }
            assert_eq!(
                crate::catalog::decode(&changed.to_string(), "docs")
                    .unwrap_err()
                    .code(),
                "FDB_STORAGE"
            );
        }
        c.run(&format!("UPDATE {} SET f1=999", quote(&index.storage)), &[])
            .unwrap();
        assert!(c
            .check_collection_integrity("docs", Default::default())
            .unwrap_err()
            .to_string()
            .contains("stale"));
        c.reindex("pair").unwrap();
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
        c.run(&format!("DELETE FROM {}", quote(&index.storage)), &[])
            .unwrap();
        assert!(c
            .check_collection_integrity("docs", Default::default())
            .is_err());
        c.reindex("pair").unwrap();
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
    #[test]
    fn compound_build_cancellation_preserves_prior_work_and_catalog() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let c = crate::Database::open(":memory:")
            .unwrap()
            .connect()
            .unwrap();
        c.execute("INSERT INTO docs {a:1,b:2}", &Default::default())
            .unwrap();
        c.execute("INSERT INTO docs {a:2,b:3}", &Default::default())
            .unwrap();
        c.execute("BEGIN", &Default::default()).unwrap();
        c.execute("INSERT INTO prior {n:7}", &Default::default())
            .unwrap();
        let baseline = c.engine.total_changes();
        let engine = Arc::downgrade(&c.engine);
        let fired = Arc::new(AtomicBool::new(false));
        let delivered = fired.clone();
        c.engine.set_progress_handler(
            1,
            Some(Box::new(move || {
                engine
                    .upgrade()
                    .is_some_and(|engine| engine.total_changes() > baseline)
                    && !delivered.swap(true, Ordering::SeqCst)
            })),
        );
        let result = c.create_compound_index(
            "docs",
            "pair",
            vec![vec!["a".into()], vec!["b".into()]],
            true,
            false,
        );
        c.engine.set_progress_handler(0, None);
        assert!(fired.load(Ordering::SeqCst));
        assert_eq!(result.unwrap_err().code(), "FDB_CANCELLED");
        assert!(c.catalog("docs").unwrap().indexes.is_empty());
        c.validate_storage_schema().unwrap();
        c.execute("COMMIT", &Default::default()).unwrap();
        assert_eq!(
            c.execute("SELECT n FROM prior", &Default::default())
                .unwrap()
                .rows,
            vec![vec![Value::Integer(7)]]
        );
        c.create_compound_index(
            "docs",
            "pair",
            vec![vec!["a".into()], vec!["b".into()]],
            true,
            false,
        )
        .unwrap();
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
}
