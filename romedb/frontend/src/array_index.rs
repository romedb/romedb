use crate::{
    quote, Collection, Connection, Document, EngineValue, Error, Index, IndexKind, Result, Value,
};
use std::collections::BTreeSet;

const MAX_ELEMENTS: usize = 4096;
const MAX_BYTES: usize = 1024 * 1024;
impl Index {
    pub(crate) fn array_keys(&self, document: &Document) -> Result<Vec<Vec<u8>>> {
        let value = crate::path_value(document, &self.path)?.unwrap_or(&Value::Null);
        let values = match value {
            Value::Null => return Ok(Vec::new()),
            Value::Array(values) => values,
            _ => {
                return Err(Error::Validation(
                    "array index requires an array, null or missing field".into(),
                ))
            }
        };
        if values.len() > MAX_ELEMENTS {
            return Err(Error::Limit(
                "array index exceeds 4096 input elements per document".into(),
            ));
        }
        let mut remaining = MAX_BYTES;
        let mut keys = BTreeSet::new();
        for value in values {
            if matches!(value, Value::Object(_) | Value::Array(_) | Value::Vector(_)) {
                return Err(Error::Validation(
                    "array index elements must be scalars or records".into(),
                ));
            }
            value.encode_with_limit(Some(remaining))?;
            let key = crate::collections::identity(value)?;
            remaining = remaining.checked_sub(key.len()).ok_or_else(|| {
                Error::Limit("array index exceeds 1 MiB encoded entries per document".into())
            })?;
            keys.insert(key);
        }
        if !keys.is_empty() {
            let id = document
                .get("id")
                .ok_or_else(|| Error::Storage("array-index document has no id".into()))?
                .encode_with_limit(Some(remaining))?;
            if id
                .len()
                .checked_mul(keys.len())
                .is_none_or(|bytes| bytes > remaining)
            {
                return Err(Error::Limit(
                    "array index exceeds 1 MiB encoded entries per document".into(),
                ));
            }
        }
        Ok(keys.into_iter().collect())
    }
}
impl Connection {
    pub fn create_array_index(
        &self,
        table: &str,
        name: &str,
        path: Vec<String>,
        if_not_exists: bool,
    ) -> Result<()> {
        self.create_index_kind(table, name, path, false, if_not_exists, IndexKind::Array)
    }
    pub(crate) fn insert_array_entries(&self, index: &Index, document: &Document) -> Result<()> {
        let keys = index.array_keys(document)?;
        if keys.is_empty() {
            return Ok(());
        }
        let id = EngineValue::Blob(document["id"].encode()?);
        let sql = format!(
            "INSERT INTO {}(\"key\",id) VALUES(?1,?2)",
            quote(&index.storage)
        );
        for key in keys {
            self.run_index_maintenance(&sql, &[EngineValue::Blob(key), id.clone()])?;
        }
        Ok(())
    }
    pub(crate) fn audit_array_index(&self, collection: &Collection, index: &Index) -> Result<u64> {
        let mut statement =
            self.prepare(format!("SELECT doc FROM {}", quote(&collection.storage)))?;
        let mut failure = None;
        let mut expected_count = 0u64;
        let execution = crate::parser_stack(|| {
            statement.run_with_row_callback(|row| {
                let check = (|| -> Result<()> {
                    let document = crate::decode_document(
                        row.get_values()
                            .next()
                            .ok_or_else(|| Error::Storage("missing array-index source".into()))?,
                    )?;
                    let keys = index.array_keys(&document)?;
                    let id = EngineValue::Blob(document["id"].encode()?);
                    let rows = self.run(
                        &format!("SELECT count(*) FROM {} WHERE id=?1", quote(&index.storage)),
                        std::slice::from_ref(&id),
                    )?;
                    if count(&rows)? != keys.len() as u64 {
                        return Err(Error::Storage("array index entry count mismatch".into()));
                    }
                    for key in &keys {
                        let rows = self.run(
                            &format!(
                                "SELECT count(*) FROM {} INDEXED BY {} WHERE \"key\"=?1 AND id=?2",
                                quote(&index.storage),
                                quote(&index.name)
                            ),
                            &[EngineValue::Blob(key.clone()), id.clone()],
                        )?;
                        if count(&rows)? != 1 {
                            return Err(Error::Storage(
                                "stale or missing array index entry".into(),
                            ));
                        }
                    }
                    expected_count = expected_count
                        .checked_add(keys.len() as u64)
                        .ok_or_else(|| Error::Limit("array index count overflow".into()))?;
                    Ok(())
                })();
                match check {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        failure = Some(error);
                        Err(turso_core::LimboError::Interrupt)
                    }
                }
            })
        });
        drop(statement);
        if let Some(error) = failure {
            return Err(error);
        }
        execution?;
        if count(&self.run(
            &format!("SELECT count(*) FROM {}", quote(&index.storage)),
            &[],
        )?)? != expected_count
        {
            return Err(Error::Storage("orphan array index entry".into()));
        }
        Ok(expected_count)
    }
}
fn count(rows: &[Vec<EngineValue>]) -> Result<u64> {
    match rows {
        [row] => match row.as_slice() {
            [EngineValue::Numeric(turso_core::Numeric::Integer(count))] if *count >= 0 => {
                Ok(*count as u64)
            }
            _ => Err(Error::Storage("invalid array index count".into())),
        },
        _ => Err(Error::Storage("missing array index count".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn array_index_metadata_missing_stale_and_orphan_entries_are_rejected() {
        let c = crate::Database::open(":memory:")
            .unwrap()
            .connect()
            .unwrap();
        c.execute(
            "INSERT INTO docs {id:docs:a,tags:['a','b','a']}",
            &Default::default(),
        )
        .unwrap();
        c.create_array_index("docs", "tags", vec!["tags".into()], false)
            .unwrap();
        let collection = c.catalog("docs").unwrap();
        assert_eq!(collection.version, 5);
        let index = &collection.indexes[0];
        let metadata = serde_json::to_value(&collection).unwrap();
        for (path, value) in [
            ("/version", serde_json::json!(4)),
            ("/indexes/0/unique", serde_json::json!(true)),
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
        c.run(
            &format!("DELETE FROM {} WHERE \"key\"=?1", quote(&index.storage)),
            &[EngineValue::Blob(
                crate::collections::identity(&Value::String("a".into())).unwrap(),
            )],
        )
        .unwrap();
        assert!(c
            .check_collection_integrity("docs", Default::default())
            .unwrap_err()
            .to_string()
            .contains("count mismatch"));
        c.reindex("tags").unwrap();
        c.run(
            &format!(
                "UPDATE {} SET \"key\"=?1 WHERE \"key\"=?2",
                quote(&index.storage)
            ),
            &[
                EngineValue::Blob(
                    crate::collections::identity(&Value::String("stale".into())).unwrap(),
                ),
                EngineValue::Blob(
                    crate::collections::identity(&Value::String("a".into())).unwrap(),
                ),
            ],
        )
        .unwrap();
        assert!(c
            .check_collection_integrity("docs", Default::default())
            .unwrap_err()
            .to_string()
            .contains("stale"));
        c.reindex("tags").unwrap();
        let orphan = Value::Record(crate::Record {
            table: "docs".into(),
            key: crate::Key::String("missing".into()),
        });
        c.run(
            &format!("INSERT INTO {} VALUES(?1,?2)", quote(&index.storage)),
            &[
                EngineValue::Blob(
                    crate::collections::identity(&Value::String("orphan".into())).unwrap(),
                ),
                EngineValue::Blob(orphan.encode().unwrap()),
            ],
        )
        .unwrap();
        assert!(c
            .check_collection_integrity("docs", Default::default())
            .unwrap_err()
            .to_string()
            .contains("orphan"));
        c.reindex("tags").unwrap();
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            2
        );
    }
    #[test]
    fn array_build_cancellation_restores_catalog_entries_and_prior_work() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let c = crate::Database::open(":memory:")
            .unwrap()
            .connect()
            .unwrap();
        c.execute(
            "INSERT INTO docs {id:docs:a,tags:['a','b','c']}",
            &Default::default(),
        )
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
        let result = c.create_array_index("docs", "tags", vec!["tags".into()], false);
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
        c.create_array_index("docs", "tags", vec!["tags".into()], false)
            .unwrap();
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
}
