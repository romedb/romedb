use turso_core::index_method::fts::analyze_text;

fn tokens(
    name: &str,
    window: (usize, usize),
    input: &str,
) -> Vec<(String, usize, usize, usize, usize)> {
    let mut values = Vec::new();
    analyze_text::<turso_core::LimboError>(name, window, input, |token| {
        values.push((
            token.text.into(),
            token.offset_from,
            token.offset_to,
            token.position,
            token.position_length,
        ));
        Ok(())
    })
    .unwrap();
    values
}

#[test]
fn shared_analyzers_preserve_case_unicode_offsets_and_long_token_rules() {
    let text = "HELLO, Café!";
    assert_eq!(
        tokens("default", (2, 3), text),
        vec![("hello".into(), 0, 5, 0, 1), ("café".into(), 7, 12, 1, 1)]
    );
    assert_eq!(
        tokens("simple", (2, 3), text),
        vec![("HELLO".into(), 0, 5, 0, 1), ("Café".into(), 7, 12, 1, 1)]
    );
    assert_eq!(
        tokens("whitespace", (2, 3), text),
        vec![("HELLO,".into(), 0, 6, 0, 1), ("Café!".into(), 7, 13, 1, 1)]
    );
    assert_eq!(
        tokens("raw", (2, 3), text),
        vec![(text.into(), 0, text.len(), 0, 1)]
    );
    for text in ["a".repeat(40), "é".repeat(20)] {
        assert!(tokens("default", (2, 3), &text).is_empty());
        assert_eq!(tokens("simple", (2, 3), &text).len(), 1);
    }
    assert_eq!(tokens("default", (2, 3), &"a".repeat(39)).len(), 1);
    assert!(tokens("default", (2, 3), "").is_empty());
    let grams = tokens("ngram", (2, 2), "CAFÉ");
    assert_eq!(
        grams.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(),
        vec!["ca", "af", "fé"]
    );
    assert_eq!(
        grams.iter().map(|t| (t.1, t.2)).collect::<Vec<_>>(),
        vec![(0, 2), (1, 3), (2, 5)]
    );
}

#[test]
fn token_visitors_stop_on_the_callers_error_and_reject_unknown_configurations() {
    let mut seen = 0;
    let error = analyze_text::<romedb::Error>("ngram", (1, 3), "database", |_| {
        seen += 1;
        if seen == 3 {
            Err(romedb::Error::Limit("token budget".into()))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.code(), "FDB_LIMIT");
    assert_eq!(seen, 3);
    for (name, window) in [("missing", (2, 3)), ("ngram", (0, 3)), ("ngram", (3, 2))] {
        assert!(
            analyze_text::<turso_core::LimboError>(name, window, "x", |_| panic!(
                "invalid configuration emitted a token"
            ))
            .is_err()
        );
    }
}

#[test]
fn inspection_matches_native_indexed_queries_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("native-analysis.db");
    let path = file.to_str().unwrap();
    for reopen in [false, true] {
        let engine = turso_core::Database::open_file_with_flags(
            turso_core::Database::io_for_path(path).unwrap(),
            path,
            turso_core::OpenFlags::default(),
            turso_core::DatabaseOpts::new().with_index_method(true),
            None,
            std::sync::Arc::new(turso_core::SqliteDialect),
        )
        .unwrap();
        let c = engine.connect().unwrap();
        for (name, text, query, hits) in [
            ("default", "CAFÉ", "café", 1),
            ("simple", "CAFÉ", "café", 0),
            ("whitespace", "CAFÉ", "café", 0),
            ("raw", "CAFÉ", "CAFÉ", 1),
            ("ngram", "database", "data", 1),
        ] {
            if !reopen {
                c.execute(format!("CREATE TABLE t_{name}(content TEXT)"))
                    .unwrap();
                c.execute(format!("CREATE INDEX i_{name} ON t_{name} USING fts(content) WITH (tokenizer='{name}')")).unwrap();
                c.execute(format!("INSERT INTO t_{name} VALUES('{text}')"))
                    .unwrap();
            }
            let mut rows = 0;
            c.prepare(format!(
                "SELECT rowid FROM t_{name} WHERE fts_match(content,'{query}') LIMIT -1"
            ))
            .unwrap()
            .run_with_row_callback(|_| {
                rows += 1;
                Ok(())
            })
            .unwrap();
            assert_eq!(rows, hits, "{name}, reopen={reopen}");
            assert!(!tokens(name, (2, 3), text).is_empty());
        }
    }
}
