use crate::{Connection, Error, IndexKind, Result};

impl Connection {
    pub fn reindex(&self, name: &str) -> Result<()> {
        let name = crate::canonical(name)?;
        self.atomic(|| {
            self.validate_storage_schema()?;
            self.rebuild_index(&name)?;
            self.validate_storage_schema()
        })
    }

    /// Build one index again from its collection's rows. The caller checks the
    /// schema: a file export rebuilds indexes that have no state yet.
    pub(crate) fn rebuild_index(&self, name: &str) -> Result<()> {
        for mut collection in self.collections()? {
            let Some(position) = collection
                .indexes
                .iter()
                .position(|index| index.name == name)
            else {
                continue;
            };
            let source = collection.clone();
            let mut budget = self.write_buffer_budget()?;
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
                IndexKind::FullText => {
                    self.build_text_storage(index, &source, Some(&mut budget))?
                }
                IndexKind::Vector => {
                    self.build_vector_storage(index, &source, Some(&mut budget))?
                }
                IndexKind::Scalar | IndexKind::Spatial | IndexKind::Array => {
                    self.run(&index.table_ddl(), &[])?;
                    self.run(&index.index_ddl(), &[])?;
                    for object in index.extra_schema() {
                        self.run(&object.ddl, &[])?;
                    }
                    self.document_batches(
                        &source,
                        crate::INDEX_BUILD_ROWS,
                        Some(&mut budget),
                        |documents| {
                            documents
                                .iter()
                                .try_for_each(|document| self.insert_index(index, document))
                        },
                    )?;
                }
            }
            self.save_catalog(&collection)?;
            return Ok(());
        }
        Err(Error::NotFound(format!("managed index {name}")))
    }
}

#[cfg(test)]
mod tests {
    use crate::{quote, Database, Parameters, Value};
    // Past a batch of each kind of build, with gaps in the rowids where rows were
    // deleted: a build reads the table in batches and must see every document.
    #[test]
    fn a_build_reads_every_document_across_its_batches() {
        builds_every_document(false);
    }
    // On a private copy each batch commits, and the result is the same.
    #[test]
    fn an_offline_build_commits_its_batches_with_the_same_result() {
        builds_every_document(true);
    }
    fn builds_every_document(offline: bool) {
        let c = Database::open(":memory:").unwrap().connect().unwrap();
        let none = Parameters::new();
        c.set_offline_build(offline);
        let rows = crate::INDEX_BUILD_ROWS as i64 + 300;
        c.execute("BEGIN", &none).unwrap();
        for i in 0..rows {
            c.execute(
                &format!(
                    "INSERT INTO docs {{id:docs:a{i},n:{i},body:'common word{i}',tags:['t{}'],\
                     v:vector32('[{i},1]'),point:geo::point({}, {})}}",
                    i % 3,
                    i % 90,
                    i % 45
                ),
                &none,
            )
            .unwrap();
        }
        c.execute("COMMIT", &none).unwrap();
        // 200 documents gone from the middle of the first batch.
        for i in 100..300 {
            c.execute(&format!("DELETE docs:a{i}"), &none).unwrap();
        }
        let kept = rows - 200;
        for sql in [
            "CREATE INDEX scalar ON docs(n)",
            "CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT",
            "CREATE SEARCH INDEX vec ON docs(v) USING VECTOR WITH(dimensions=2,metric='l2')",
            "CREATE SEARCH INDEX loc ON docs(point) USING SPATIAL",
            "CREATE SEARCH INDEX tag ON docs(tags) USING ARRAY",
        ] {
            c.execute(sql, &none).expect(sql);
        }
        let count = |c: &crate::Connection, sql: &str| -> Value {
            c.execute(sql, &none).unwrap().rows[0][0].clone()
        };
        let check = |c: &crate::Connection| {
            for sql in [
                "SELECT count(*) FROM search::text('words','common',10000)",
                "SELECT count(*) FROM search::vector('vec',vector32('[0,1]'),10000)",
            ] {
                assert_eq!(count(c, sql), Value::Integer(kept), "{sql}");
            }
            assert_eq!(
                count(c, "SELECT count(*) FROM search::text('words','word1299',5)"),
                Value::Integer(1)
            );
            assert_eq!(
                count(c, "SELECT count(*) FROM docs WHERE n > 1200"),
                Value::Integer(rows - 1201)
            );
            let first = (0..rows)
                .filter(|i| !(100..300).contains(i) && i % 3 == 0)
                .count() as i64;
            assert_eq!(
                count(
                    c,
                    "SELECT count(*) FROM docs WHERE array::contains(tags,'t0')"
                ),
                Value::Integer(first)
            );
        };
        check(&c);
        // A rebuild reads the same way, and changes nothing.
        for name in ["scalar", "words", "vec", "loc", "tag"] {
            c.reindex(name).expect(name);
        }
        check(&c);
    }
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
    use crate::{quote, Database, Parameters, Value};
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
        assert_eq!(c.reindex("unique_n").unwrap_err().code(), "RDB_CONSTRAINT");
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
            assert_eq!(result.unwrap_err().code(),"RDB_CANCELLED","{name}");
            c.check_collection_integrity("docs",Default::default()).unwrap();
            c.execute("COMMIT",&Parameters::new()).unwrap();
            assert_eq!(c.execute("SELECT n FROM prior",&Parameters::new()).unwrap().rows,vec![vec![Value::Integer(7)]]);
            c.reindex(name).unwrap(); c.check_collection_integrity("docs",Default::default()).unwrap();
        }
    }
}
