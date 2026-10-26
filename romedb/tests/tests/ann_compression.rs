use romedb::{Database, Parameters, Value, VectorIndexOptions};

fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}
fn record(key: &str) -> Value {
    Value::Record(romedb::Record {
        table: "docs".into(),
        key: romedb::Key::String(key.into()),
    })
}

#[test]
fn compressed_graphs_keep_original_values_reranking_snapshots_rebuild_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("compressed.db");
    let vector = Value::vector32(&[1.0001, 0.1234567]).unwrap();
    let params = Parameters::from([("$v".into(), vector.clone())]);
    let search = "SELECT id,distance FROM search::vector('compressed',$v,1)";
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        c.execute("INSERT INTO docs {id:docs:a,v:$v}", &params)
            .unwrap();
        q(
            &c,
            "INSERT INTO docs {id:docs:b,v:vector32('[1.0002,0.1234]')}",
        );
        q(&c,"CREATE SEARCH INDEX compressed ON docs(v) USING VECTOR WITH(dimensions=2,metric='l2',quantization='f16')");
        let mut options = VectorIndexOptions::default();
        options.dimensions = 2;
        options.metric = "cosine".into();
        options.quantization = "F16".into();
        c.create_vector_index_with_options("docs", "cosine", vec!["v".into()], options, false)
            .unwrap();
        let expected = vec![vec![record("a"), Value::Number(0.0)]];
        assert_eq!(c.execute(search, &params).unwrap().rows, expected);
        assert_eq!(
            q(&c, "SELECT v FROM docs WHERE id=docs:a").rows,
            vec![vec![vector.clone()]]
        );
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
        let info = q(&c, "INFO FOR TABLE docs");
        let Value::Object(info) = &info.rows[0][0] else {
            panic!()
        };
        let Value::Array(indexes) = &info["indexes"] else {
            panic!()
        };
        assert!(indexes.iter().all(|index|matches!(index,Value::Object(index) if index["quantization"]==Value::String("f16".into()))));
        let reader = db.connect().unwrap();
        q(&reader, "BEGIN");
        assert_eq!(reader.execute(search, &params).unwrap().rows, expected);
        q(&c, "BEGIN");
        q(&c, "UPDATE docs:a {v:vector32('[2,2]')}");
        assert_eq!(c.execute(search, &params).unwrap().rows[0][0], record("b"));
        q(&c, "REINDEX compressed");
        q(&c, "REINDEX cosine");
        q(&c, "ROLLBACK");
        assert_eq!(c.execute(search, &params).unwrap().rows, expected);
        q(&c, "DELETE FROM docs:a");
        assert_eq!(reader.execute(search, &params).unwrap().rows, expected);
        q(&reader, "COMMIT");
        c.execute("INSERT INTO docs {id:docs:a,v:$v}", &params)
            .unwrap();
        q(&c, "REINDEX compressed");
        q(&c, "REINDEX cosine");
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(
        c.execute(search, &params).unwrap().rows,
        vec![vec![record("a"), Value::Number(0.0)]]
    );
    assert_eq!(
        q(&c, "SELECT v FROM docs WHERE id=docs:a").rows,
        vec![vec![vector]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn compressed_index_validation_boundaries_and_late_failures_are_atomic() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "INSERT INTO docs {id:docs:a,v:vector32('[1,0]')}");
    for options in [
        "quantization='unknown'",
        "quantization='i8'",
        "quantization=16",
        "quantization='f16',quantization='f16'",
    ] {
        assert!(c.execute(&format!("CREATE SEARCH INDEX bad ON docs(v) USING VECTOR WITH(dimensions=2,metric='l2',{options})"),&Parameters::new()).is_err());
    }
    q(&c, "INSERT INTO large {v:vector32('[65505,0]')}");
    assert_eq!(c.execute("CREATE SEARCH INDEX too_large ON large(v) USING VECTOR WITH(dimensions=2,metric='l2',quantization='f16')",&Parameters::new()).unwrap_err().code(),"FDB_VALIDATION");
    q(
        &c,
        "CREATE SEARCH INDEX still_valid ON large(v) USING VECTOR WITH(dimensions=2,metric='l2')",
    );
    q(&c,"CREATE SEARCH INDEX compressed ON docs(v) USING VECTOR WITH(dimensions=2,metric='l2',quantization='f16')");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {id:docs:prior,v:vector32('[2,0]')}");
    assert_eq!(c.execute("INSERT INTO docs(id,v) VALUES(docs:b,vector32('[3,0]')),(docs:c,vector32('[65505,0]'))",&Parameters::new()).unwrap_err().code(),"FDB_VALIDATION");
    assert_eq!(
        q(&c, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(2)]]
    );
    for sql in [
        "UPDATE docs SET v=vector32('[65505,0]')",
        "SELECT * FROM search::vector('compressed',vector32('[65505,0]'),1)",
    ] {
        assert_eq!(
            c.execute(sql, &Parameters::new()).unwrap_err().code(),
            "FDB_VALIDATION"
        );
    }
    q(
        &c,
        "INSERT INTO docs {id:docs:boundary,v:vector32('[65504,-65504]')}",
    );
    q(
        &c,
        "INSERT INTO docs {id:docs:tiny,v:vector32('[0.000000059604645,0.00000001]')}",
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    assert_eq!(
        q(&c, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn compressed_graph_recall_matches_seeded_exact_neighbors() {
    let mut seed = 0x7b92_1a03_19c5_7df1u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        ((seed >> 40) as f32 / 16777216.0) * 2.0 - 1.0
    };
    let points = (0..1000)
        .map(|_| (0..64).map(|_| next()).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let queries = (0..24)
        .map(|_| (0..64).map(|_| next()).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "BEGIN");
    for (i, point) in points.iter().enumerate() {
        c.insert(
            "docs",
            romedb::Document::from([
                (
                    "id".into(),
                    Value::Record(romedb::Record {
                        table: "docs".into(),
                        key: romedb::Key::Integer(i as i64),
                    }),
                ),
                ("v".into(), Value::vector32(point).unwrap()),
            ]),
        )
        .unwrap();
    }
    q(&c, "COMMIT");
    for metric in ["l2", "cosine"] {
        let expected = queries
            .iter()
            .map(|query| {
                let mut distances = points
                    .iter()
                    .enumerate()
                    .map(|(i, point)| {
                        let distance = if metric == "l2" {
                            point
                                .iter()
                                .zip(query)
                                .map(|(a, b)| (*a as f64 - *b as f64).powi(2))
                                .sum::<f64>()
                                .sqrt()
                        } else {
                            let dot = point
                                .iter()
                                .zip(query)
                                .map(|(a, b)| *a as f64 * *b as f64)
                                .sum::<f64>();
                            let norm = |v: &[f32]| {
                                v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt()
                            };
                            (1.0 - dot / (norm(point) * norm(query))).clamp(0.0, 2.0)
                        };
                        (distance, i as i64)
                    })
                    .collect::<Vec<_>>();
                distances.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
                distances.truncate(10);
                distances
            })
            .collect::<Vec<_>>();
        for quantization in ["f32", "f16"] {
            let name = format!("{metric}_{quantization}");
            q(&c,&format!("CREATE SEARCH INDEX {name} ON docs(v) USING VECTOR WITH(dimensions=64,metric='{metric}',quantization='{quantization}')"));
            let start = std::time::Instant::now();
            let mut found = 0;
            for (query, expected) in queries.iter().zip(&expected) {
                let result = c
                    .search_vectors(&name, &Value::vector32(query).unwrap(), 10)
                    .unwrap();
                assert_eq!(result.rows.len(), 10);
                for row in result.rows {
                    let [Value::Record(romedb::Record {
                        key: romedb::Key::Integer(key),
                        ..
                    }), Value::Number(distance)] = row.as_slice()
                    else {
                        panic!("{row:?}")
                    };
                    if let Some((exact, _)) = expected.iter().find(|(_, id)| id == key) {
                        found += 1;
                        assert!((distance - exact).abs() < 1e-12);
                    }
                }
            }
            let recall = found as f64 / (queries.len() * 10) as f64;
            eprintln!(
                "{name}: recall@10={recall:.4}, 24 queries={:?}",
                start.elapsed()
            );
            assert!(recall >= 0.95, "{name}: {recall}");
            q(&c, &format!("DROP INDEX {name}"));
        }
    }
}

#[test]
fn compressed_checkpoint_and_redo_recover_after_process_exit() {
    const ENV: &str = "ROMEDB_F16_CRASH_TEST_PATH";
    if let Ok(path) = std::env::var(ENV) {
        let c = Database::open(&path).unwrap().connect().unwrap();
        q(&c, "INSERT INTO docs {id:docs:a,v:vector32('[1,0]')}");
        q(&c,"CREATE SEARCH INDEX compressed ON docs(v) USING VECTOR WITH(dimensions=2,metric='l2',quantization='f16')");
        q(&c, "BEGIN");
        for n in 0..1030 {
            q(&c, &format!("UPSERT docs:a {{v:vector32('[{n},1]')}}"));
        }
        q(&c, "ROLLBACK");
        assert_eq!(
            c.search_vectors("compressed", &Value::vector32(&[1.0, 0.0]).unwrap(), 1)
                .unwrap()
                .rows[0][1],
            Value::Number(0.0)
        );
        q(&c, "BEGIN");
        for n in 0..1026 {
            q(&c, &format!("UPSERT docs:a {{v:vector32('[{n},0]')}}"));
        }
        q(&c, "COMMIT");
        q(&c, "INSERT INTO docs {id:docs:b,v:vector32('[0,1]')}");
        q(&c, "BEGIN");
        q(&c, "SAVEPOINT branch");
        q(&c, "DELETE FROM docs");
        q(
            &c,
            "INSERT INTO docs {id:docs:old_branch,v:vector32('[0,0]')}",
        );
        assert_eq!(
            c.search_vectors("compressed", &Value::vector32(&[0.0, 0.0]).unwrap(), 1)
                .unwrap()
                .rows[0][0],
            record("old_branch")
        );
        q(&c, "ROLLBACK TO branch");
        q(&c, "DELETE FROM docs");
        q(
            &c,
            "INSERT INTO docs {id:docs:new_branch,v:vector32('[0,0]')}",
        );
        assert_eq!(
            c.search_vectors("compressed", &Value::vector32(&[0.0, 0.0]).unwrap(), 1)
                .unwrap()
                .rows[0][0],
            record("new_branch")
        );
        std::process::exit(73);
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f16-crash.db");
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "compressed_checkpoint_and_redo_recover_after_process_exit",
        ])
        .env(ENV, &path)
        .output()
        .unwrap();
    assert_eq!(
        child.status.code(),
        Some(73),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    let hits = c
        .search_vectors("compressed", &Value::vector32(&[1025.0, 0.0]).unwrap(), 10)
        .unwrap();
    assert_eq!(
        hits.rows
            .iter()
            .map(|row| row[0].clone())
            .collect::<Vec<_>>(),
        vec![record("a"), record("b")]
    );
    assert_eq!(hits.rows[0][1], Value::Number(0.0));
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .documents,
        2
    );
}
