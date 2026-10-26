use romedb::{Database, FullTextOptions, Parameters, Value};

fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}
fn texts(value: &Value) -> Vec<String> {
    let Value::Array(tokens) = value else {
        panic!("{value:?}")
    };
    tokens
        .iter()
        .map(|v| {
            let Value::Object(token) = v else { panic!() };
            let Value::String(text) = &token["text"] else {
                panic!()
            };
            text.clone()
        })
        .collect()
}

#[test]
fn analyzer_options_match_inspection_indexed_queries_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("analyzers.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(
            &c,
            "INSERT INTO docs {id:docs:a,title:'HELLO Café',body:'database'}",
        );
        for (name, expected) in [
            ("default", vec!["hello", "café"]),
            ("simple", vec!["HELLO", "Café"]),
            ("whitespace", vec!["HELLO", "Café"]),
            ("raw", vec!["HELLO Café"]),
            (
                "ngram",
                vec!["he", "el", "ll", "lo", "o ", " c", "ca", "af", "fé"],
            ),
        ] {
            let sizes = if name == "ngram" {
                ",min_gram=2,max_gram=2"
            } else {
                ""
            };
            let ddl = format!("CREATE SEARCH INDEX i_{name} ON docs(title,body) USING FULLTEXT WITH(tokenizer='{name}'{sizes})");
            q(&c, &ddl);
            q(
                &c,
                &ddl.replace("CREATE SEARCH INDEX", "CREATE SEARCH INDEX IF NOT EXISTS"),
            );
            let value = c.analyze_text(&format!("i_{name}"), "HELLO Café").unwrap();
            assert_eq!(texts(&value), expected, "{name}");
            assert_eq!(
                q(
                    &c,
                    &format!("SELECT search::analyze('i_{name}',title) FROM docs")
                )
                .rows,
                vec![vec![value]]
            );
            let query = if name == "raw" {
                "database"
            } else if name == "ngram" {
                "data"
            } else {
                "Café"
            };
            let sql = format!("SELECT id FROM search::text('i_{name}','{query}',10)");
            assert_eq!(q(&c, &sql).rows.len(), 1, "{name}");
            assert!(
                q(&c, &format!("EXPLAIN QUERY PLAN {sql}"))
                    .rows
                    .iter()
                    .flatten()
                    .any(
                        |v| matches!(v,Value::String(s) if s.contains("fts") || s.contains("INDEX"))
                    ),
                "{name}"
            );
        }
        let value = c.analyze_text("i_default", "HELLO, Café!").unwrap();
        let Value::Array(tokens) = value else {
            panic!()
        };
        let Value::Object(cafe) = &tokens[1] else {
            panic!()
        };
        assert_eq!(cafe["offset_from"], Value::Integer(7));
        assert_eq!(cafe["offset_to"], Value::Integer(12));
        assert_eq!(cafe["position"], Value::Integer(1));
        assert_eq!(cafe["position_length"], Value::Integer(1));
        q(&c, "UPDATE docs:a {body:'updated'}");
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    for name in ["default", "raw", "simple", "whitespace", "ngram"] {
        assert_eq!(
            q(
                &c,
                &format!("SELECT id FROM search::text('i_{name}','updated',10)")
            )
            .rows
            .len(),
            1
        );
        assert!(!texts(&c.analyze_text(&format!("i_{name}"), "updated").unwrap()).is_empty());
        q(&c, &format!("REINDEX i_{name}"));
    }
    let info = format!("{:?}", q(&c, "INFO FOR TABLE docs"));
    assert!(info.contains("tantivy-ngram-0.26") && info.contains("min_gram"));
}

#[test]
fn analyzer_validation_limits_typed_results_and_transaction_preservation() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE SEARCH INDEX text ON docs(body) USING FULLTEXT");
    for options in [
        "tokenizer='unknown'",
        "tokenizer='default',min_gram=1",
        "tokenizer='ngram',min_gram=0",
        "tokenizer='ngram',min_gram=4,max_gram=3",
        "tokenizer='ngram',max_gram=65",
    ] {
        assert_eq!(
            c.execute(
                &format!(
                    "CREATE SEARCH INDEX invalid ON docs(body) USING FULLTEXT WITH({options})"
                ),
                &Parameters::new()
            )
            .unwrap_err()
            .code(),
            "FDB_VALIDATION"
        );
    }
    for options in [
        "",
        "tokenizer='raw',tokenizer='simple'",
        "min_gram=2,min_gram=3",
        "extra=2",
        "max_gram=-1",
        "tokenizer=raw",
    ] {
        assert_eq!(
            c.execute(
                &format!(
                    "CREATE SEARCH INDEX invalid ON docs(body) USING FULLTEXT WITH({options})"
                ),
                &Parameters::new()
            )
            .unwrap_err()
            .code(),
            "FDB_SYNTAX"
        );
    }
    let mut options = FullTextOptions::default();
    options.tokenizer = "NGRAM".into();
    c.create_fulltext_index_with_options(
        "docs",
        "grams",
        vec![vec!["body".into()]],
        options,
        false,
    )
    .unwrap();
    assert_eq!(
        texts(&c.analyze_text("grams", "ABC").unwrap()),
        vec!["ab", "abc", "bc"]
    );
    assert_eq!(
        q(&c, "SELECT search::analyze('text',null)").rows,
        vec![vec![Value::Null]]
    );
    assert!(texts(&c.analyze_text("text", &"x".repeat(40)).unwrap()).is_empty());
    assert_eq!(
        texts(&c.analyze_text("text", &"x".repeat(39)).unwrap()).len(),
        1
    );
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {id:docs:a,body:'retained'}");
    for (text, code) in [
        (Value::Integer(7), "FDB_VALIDATION"),
        (Value::String("x".repeat(1024 * 1024 + 1)), "FDB_LIMIT"),
        (Value::String("x ".repeat(16_385)), "FDB_LIMIT"),
    ] {
        let error = c
            .execute(
                "SELECT search::analyze('text',$text)",
                &Parameters::from([("$text".into(), text)]),
            )
            .unwrap_err();
        assert_eq!(error.code(), code, "{error}");
    }
    q(&c, "COMMIT");
    assert_eq!(q(&c, "SELECT * FROM docs").rows.len(), 1);
    q(
        &c,
        "INSERT INTO results {id:results:a,tokens:search::analyze('text','Hello')}",
    );
    q(
        &c,
        "UPDATE results SET tokens=search::analyze('text','Bye') RETURNING tokens",
    );
    assert_eq!(
        texts(&q(&c, "SELECT tokens FROM results").rows[0][0]),
        vec!["bye"]
    );
    assert!(c
        .execute(
            "SELECT search::analyze($index,'text')",
            &Parameters::from([("$index".into(), Value::String("text".into()))])
        )
        .is_err());
    assert_eq!(
        c.analyze_text("missing", "text").unwrap_err().code(),
        "FDB_NOT_FOUND"
    );
}

#[test]
fn analyzer_metadata_and_index_replacement_follow_transaction_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshots.db");
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let reader = db.connect().unwrap();
    let writer = db.connect().unwrap();
    q(&writer, "INSERT INTO docs {id:docs:a,body:'HELLO'}");
    q(
        &writer,
        "CREATE SEARCH INDEX text ON docs(body) USING FULLTEXT",
    );
    q(&reader, "BEGIN");
    assert_eq!(
        texts(&reader.analyze_text("text", "HELLO").unwrap()),
        vec!["hello"]
    );
    q(&writer, "BEGIN");
    q(&writer, "DROP INDEX text");
    q(
        &writer,
        "CREATE SEARCH INDEX text ON docs(body) USING FULLTEXT WITH(tokenizer='raw')",
    );
    assert_eq!(
        texts(&writer.analyze_text("text", "HELLO").unwrap()),
        vec!["HELLO"]
    );
    assert_eq!(
        texts(&reader.analyze_text("text", "HELLO").unwrap()),
        vec!["hello"]
    );
    q(&writer, "ROLLBACK");
    assert_eq!(
        texts(&writer.analyze_text("text", "HELLO").unwrap()),
        vec!["hello"]
    );
    q(&writer, "BEGIN");
    q(&writer, "DROP INDEX text");
    q(
        &writer,
        "CREATE SEARCH INDEX text ON docs(body) USING FULLTEXT WITH(tokenizer='raw')",
    );
    q(&writer, "COMMIT");
    assert_eq!(
        texts(&reader.analyze_text("text", "HELLO").unwrap()),
        vec!["hello"]
    );
    assert_eq!(
        q(&reader, "SELECT id FROM search::text('text','hello',10)")
            .rows
            .len(),
        1
    );
    q(&reader, "COMMIT");
    assert_eq!(
        texts(&reader.analyze_text("text", "HELLO").unwrap()),
        vec!["HELLO"]
    );
    assert!(q(&reader, "SELECT id FROM search::text('text','hello',10)")
        .rows
        .is_empty());
}
