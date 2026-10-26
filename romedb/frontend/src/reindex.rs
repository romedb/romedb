use crate::{quote, Collection, Connection, Document, Error, IndexKind, Result};

impl Connection {
    pub fn reindex(&self, name: &str) -> Result<()> {
        let name = crate::canonical(name)?;
        self.atomic(|| {
            self.validate_storage_schema()?;
            for mut collection in self.collections()? {
                let Some(position) = collection
                    .indexes
                    .iter()
                    .position(|index| index.name == name)
                else {
                    continue;
                };
                let documents = self.reindex_documents(&collection)?;
                let index = &mut collection.indexes[position];
                self.drop_index_storage(index)?;
                if let Some(config) = &mut index.fulltext {
                    config.storage_version = 2;
                }
                // Upgrade the released latitude format; keep current formats,
                // including region indexes, as they are.
                if index.kind == IndexKind::Spatial && index.spatial.is_none() {
                    index.spatial = Some(crate::spatial::Config::current());
                }
                match index.kind {
                    IndexKind::FullText => self.build_text_storage(index, &documents)?,
                    IndexKind::Vector => self.build_vector_storage(index, &documents)?,
                    IndexKind::Scalar | IndexKind::Spatial | IndexKind::Array => {
                        self.run(&index.table_ddl(), &[])?;
                        self.run(&index.index_ddl(), &[])?;
                        for object in index.extra_schema() {
                            self.run(&object.ddl, &[])?;
                        }
                        for document in &documents {
                            self.insert_index(index, document)?;
                        }
                    }
                }
                self.save_catalog(&collection)?;
                self.validate_storage_schema()?;
                return Ok(());
            }
            Err(Error::NotFound(format!("managed index {name}")))
        })
    }

    fn reindex_documents(&self, collection: &Collection) -> Result<Vec<Document>> {
        let mut statement =
            self.prepare(format!("SELECT doc FROM {}", quote(&collection.storage)))?;
        let mut documents = Vec::new();
        let mut budget = self.write_buffer_budget()?;
        let mut failure = None;
        let result = self.meter_statement(&mut statement, |statement| {
            crate::parser_stack(|| {
                statement.run_with_row_callback(|row| {
                    let result = (|| -> Result<()> {
                        let value = row
                            .get_values()
                            .next()
                            .ok_or_else(|| Error::Storage("missing source document".into()))?;
                        let document = crate::decode_document(value)?;
                        budget.document(&document)?;
                        documents.push(document);
                        Ok(())
                    })();
                    if let Err(error) = result {
                        failure = Some(error);
                        return Err(turso_core::LimboError::Interrupt);
                    }
                    Ok(())
                })
            })?;
            Ok(())
        });
        if let Err(error @ Error::Rollback { .. }) = result {
            return Err(error);
        }
        if let Some(error) = failure {
            return Err(error);
        }
        result?;
        Ok(documents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Database, Parameters, Value};
    #[test]
    fn rebuild_repairs_missing_auxiliary_entries_from_documents() {
        let c = Database::open(":memory:").unwrap().connect().unwrap();
        for sql in [
            "INSERT INTO docs {id:docs:a,n:7,body:'hello',tags:['a','b'],v:vector32('[1,0]'),point:geo::point(1,2)}",
            "CREATE INDEX scalar ON docs(n)",
            "CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT",
            "CREATE SEARCH INDEX vec ON docs(v) USING VECTOR WITH(dimensions=2,metric='cosine')",
            "CREATE SEARCH INDEX loc ON docs(point) USING SPATIAL",
        ] { c.execute(sql,&Parameters::new()).unwrap(); }
        let indexes = c.catalog("docs").unwrap().indexes;
        for index in indexes {
            c.run(&format!("DELETE FROM {}", quote(&index.storage)), &[])
                .unwrap();
            assert!(c
                .check_collection_integrity("docs", Default::default())
                .is_err());
            c.execute(
                &format!("REINDEX {}", quote(&index.name)),
                &Parameters::new(),
            )
            .unwrap();
            c.check_collection_integrity("docs", Default::default())
                .unwrap();
        }
        assert_eq!(
            c.lookup_index("docs", "scalar", &Value::Integer(7))
                .unwrap()
                .len(),
            1
        );
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    use crate::{Database, Parameters, Value};
    #[test]
    fn failed_unique_rebuild_restores_prior_index_after_inserting_a_prefix() {
        let c = Database::open(":memory:").unwrap().connect().unwrap();
        for sql in [
            "INSERT INTO docs {id:docs:a,n:1}",
            "INSERT INTO docs {id:docs:b,n:2}",
            "CREATE UNIQUE INDEX unique_n ON docs(n)",
        ] {
            c.execute(sql, &Parameters::new()).unwrap();
        }
        let collection = c.catalog("docs").unwrap();
        let mut docs = c.documents(&collection).unwrap();
        docs[1].insert("n".into(), Value::Integer(1));
        c.run(
            &format!(
                "UPDATE {} SET doc=?1 WHERE id=?2",
                quote(&collection.storage)
            ),
            &[
                crate::EngineValue::Blob(Value::Object(docs[1].clone()).encode().unwrap()),
                crate::EngineValue::Blob(docs[1]["id"].encode().unwrap()),
            ],
        )
        .unwrap();
        c.execute("BEGIN", &Parameters::new()).unwrap();
        c.execute("INSERT INTO prior {id:prior:a,n:7}", &Parameters::new())
            .unwrap();
        assert_eq!(c.reindex("unique_n").unwrap_err().code(), "FDB_CONSTRAINT");
        let rows = c
            .run(
                &format!(
                    "SELECT key FROM {} ORDER BY key",
                    quote(&collection.indexes[0].storage)
                ),
                &[],
            )
            .unwrap();
        assert_eq!(
            rows,
            vec![
                vec![crate::EngineValue::from_i64(1)],
                vec![crate::EngineValue::from_i64(2)]
            ]
        );
        c.execute("COMMIT", &Parameters::new()).unwrap();
        assert_eq!(
            c.execute("SELECT * FROM prior", &Parameters::new())
                .unwrap()
                .rows
                .len(),
            1
        );
    }
}

#[cfg(test)]
mod cancellation_tests {
    use crate::{Database, Parameters, Value};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    #[test]
    fn cancellation_after_rebuild_writes_restores_indexes_and_caller_scope() {
        for (name,ddl) in [
            ("scalar","CREATE INDEX scalar ON docs(n)"),
            ("words","CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT"),
            ("vec","CREATE SEARCH INDEX vec ON docs(v) USING VECTOR WITH(dimensions=2,metric='cosine')"),
            ("compressed","CREATE SEARCH INDEX compressed ON docs(v) USING VECTOR WITH(dimensions=2,metric='cosine',quantization='f16')"),
            ("loc","CREATE SEARCH INDEX loc ON docs(point) USING SPATIAL"),
            ("tags","CREATE SEARCH INDEX tags ON docs(tags) USING ARRAY"),
        ] {
            let c=Database::open(":memory:").unwrap().connect().unwrap();
            c.execute("INSERT INTO docs {id:docs:a,n:1,body:'hello',tags:['a','b'],v:vector32('[1,0]'),point:geo::point(1,2)}",&Parameters::new()).unwrap();
            c.execute("INSERT INTO docs {id:docs:b,n:2,body:'world',tags:['b'],v:vector32('[0,1]'),point:geo::point(2,3)}",&Parameters::new()).unwrap();
            c.execute(ddl,&Parameters::new()).unwrap();
            c.execute("BEGIN",&Parameters::new()).unwrap();
            c.execute("INSERT INTO prior {id:prior:a,n:7}",&Parameters::new()).unwrap();
            let baseline=c.engine.total_changes(); let engine=Arc::downgrade(&c.engine);
            let fired=Arc::new(AtomicBool::new(false)); let delivered=fired.clone();
            c.engine.set_progress_handler(1,Some(Box::new(move|| {
                engine.upgrade().is_some_and(|engine| engine.total_changes()>baseline) && !delivered.swap(true,Ordering::SeqCst)
            })));
            let result=c.reindex(name); c.engine.set_progress_handler(0,None);
            assert!(fired.load(Ordering::SeqCst),"{name}");
            assert_eq!(result.unwrap_err().code(),"FDB_CANCELLED","{name}");
            c.check_collection_integrity("docs",Default::default()).unwrap();
            c.execute("COMMIT",&Parameters::new()).unwrap();
            assert_eq!(c.execute("SELECT n FROM prior",&Parameters::new()).unwrap().rows,vec![vec![Value::Integer(7)]]);
            c.reindex(name).unwrap(); c.check_collection_integrity("docs",Default::default()).unwrap();
        }
    }
}
