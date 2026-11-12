use romedb::{Database, Parameters, Value};

fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}
fn hits(c: &romedb::Connection, query: &str, limit: usize) -> Vec<Vec<Value>> {
    c.execute(
        "SELECT id,score FROM search::text('articles_text',$q,$n) ORDER BY score DESC,id",
        &Parameters::from([
            ("$q".into(), Value::String(query.into())),
            ("$n".into(), Value::Integer(limit as i64)),
        ]),
    )
    .unwrap()
    .rows
}

#[test]
fn a_common_word_reads_only_its_best_hits_and_still_breaks_ties_by_id() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE articles");
    q(
        &c,
        "CREATE SEARCH INDEX articles_text ON articles(title,body) USING FULLTEXT",
    );
    q(&c, "BEGIN");
    for n in (0..3000).rev() {
        let body = if n % 100 == 0 {
            "common".to_owned()
        } else {
            format!("common {}", "filler ".repeat(1 + n % 7))
        };
        q(
            &c,
            &format!("INSERT INTO articles {{id:type::record('articles','p{n:04}'),title:'t',body:'{body}'}}"),
        );
    }
    q(&c, "COMMIT");
    let all = hits(&c, "common", 10000);
    assert_eq!(all.len(), 3000);
    for limit in [1, 10, 29, 30, 31, 500] {
        assert_eq!(hits(&c, "common", limit), all[..limit], "limit {limit}");
    }
    assert!(matches!(&all[0][0], Value::Record(r) if r.key == romedb::Key::String("p0000".into())));
    let best = c
        .profile_select(
            "SELECT id,score FROM search::text('articles_text','common',10)",
            &Parameters::new(),
        )
        .unwrap();
    assert_eq!(best.result.rows.len(), 10);
    assert!(best.metrics.rows_read < 200, "{:?}", best.metrics);
}

#[test]
fn text_search_ranks_all_ties_before_limit_and_uses_native_index() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE articles");
    q(
        &c,
        "CREATE SEARCH INDEX articles_text ON articles(title,body) USING FULLTEXT",
    );
    q(&c, "BEGIN");
    for n in (0..40).rev() {
        q(&c,&format!("INSERT INTO articles {{id:type::record('articles','p{n:02}'),title:'same title',body:'same body'}}"));
    }
    q(&c, "COMMIT");
    let all = hits(&c, "same", 100);
    assert_eq!(all.len(), 40);
    assert_eq!(hits(&c, "same", 3), all[..3]);
    assert!(matches!(&all[0][0],Value::Record(r) if r.key == romedb::Key::String("p00".into())));
    assert!(matches!(all[0][1],Value::Number(n) if n.is_finite() && n>0.0));
    let profile = c
        .profile_select(
            "SELECT id,score FROM search::text('articles_text','same',3)",
            &Parameters::new(),
        )
        .unwrap();
    eprintln!("text tie query metrics: {:?}", profile.metrics);
    let plan = q(
        &c,
        "EXPLAIN QUERY PLAN SELECT id,score FROM search::text('articles_text','same',3)",
    );
    assert!(format!("{:?}", plan.rows).contains("QUERY INDEX METHOD fts"));
    assert!(hits(&c, "same", 0).is_empty());
    assert!(hits(&c, "absent", 100).is_empty());
    assert_eq!(hits(&c, "title AND body", 100).len(), 40);
    assert_eq!(hits(&c, "\"same title\"", 100).len(), 40);
    // WHERE is a post-filter over the requested ranked slice.
    assert!(q(
        &c,
        "SELECT id FROM search::text('articles_text','same',3) WHERE id=articles:p39"
    )
    .rows
    .is_empty());
    assert_eq!(q(&c,"SELECT a.id,a.title,h.score FROM search::text('articles_text','same',3) h JOIN articles a ON a.id=h.id ORDER BY h.score DESC,h.id").rows.len(),3);
    assert_eq!(
        c.check_collection_integrity("articles", Default::default())
            .unwrap()
            .index_entries,
        40
    );
    drop(db.connect().expect("full-text storage schema"));
    q(
        &c,
        "INSERT INTO articles {id:articles:needle,title:'uncommonterm'}",
    );
    let selective = c
        .profile_select(
            "SELECT id,score FROM search::text('articles_text','uncommonterm',10)",
            &Parameters::new(),
        )
        .unwrap();
    assert_eq!(selective.result.rows.len(), 1);
    assert!(selective.metrics.rows_read < 5, "{:?}", selective.metrics);
    eprintln!("text selective query metrics: {:?}", selective.metrics);
}

#[test]
fn text_index_transactions_validation_reopen_and_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("text.db");
    let baseline;
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c,"INSERT INTO articles {id:articles:a,title:'Database',body:'atomic database transactions'}");
        q(
            &c,
            "INSERT INTO articles {id:articles:b,title:'Updates',body:'database updates'}",
        );
        q(&c, "INSERT INTO articles {id:articles:c,title:null}");
        q(
            &c,
            "CREATE SEARCH INDEX articles_text ON articles(title,body) USING FULLTEXT",
        );
        baseline = hits(&c, "database", 10);
        assert_eq!(baseline.len(), 2);
        for sql in [
            "INSERT INTO articles {title:42}",
            "UPDATE articles {body:['bad']}",
            "DEFINE FIELD title ON articles TYPE integer",
            "CREATE SEARCH INDEX articles_text ON articles(body) USING FULLTEXT",
        ] {
            assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
            assert_eq!(hits(&c, "database", 10), baseline);
        }
        q(&c,"CREATE SEARCH INDEX IF NOT EXISTS articles_text ON articles(title,body) USING FULLTEXT");
        let other = db.connect().unwrap();
        q(&c, "BEGIN");
        q(&c, "INSERT INTO articles {id:articles:d,title:'database'}");
        assert_eq!(hits(&c, "database", 10).len(), 3);
        assert_eq!(hits(&other, "database", 10), baseline);
        q(&c, "SAVEPOINT change");
        q(
            &c,
            "UPDATE articles SET title='nothing',body=null WHERE id=articles:a",
        );
        assert_eq!(hits(&c, "database", 10).len(), 2);
        q(&c, "ROLLBACK TO change");
        q(&c, "RELEASE change");
        assert_eq!(hits(&c, "database", 10).len(), 3);
        q(&c, "ROLLBACK");
        assert_eq!(hits(&c, "database", 10), baseline);
        q(&c, "BEGIN");
        q(&c, "DROP INDEX articles_text");
        q(&c, "ROLLBACK");
        assert_eq!(hits(&c, "database", 10), baseline);
        c.check_collection_integrity("articles", Default::default())
            .unwrap();
        q(&c, "PRAGMA wal_checkpoint(TRUNCATE)");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(hits(&c, "database", 10), baseline);
    q(&c, "DELETE FROM articles WHERE id=articles:a");
    assert_eq!(hits(&c, "database", 10).len(), 1);
    c.check_collection_integrity("articles", Default::default())
        .unwrap();
    q(&c, "DROP INDEX articles_text");
    drop(db.connect().unwrap());
    q(
        &c,
        "CREATE SEARCH INDEX articles_text ON articles(title,body) USING FULLTEXT",
    );
    assert_eq!(hits(&c, "database", 10).len(), 1);
    q(&c, "DROP TABLE articles");
    drop(db.connect().unwrap());
}

#[test]
fn text_bulk_build_batches_rows_and_preserves_savepoint_rollback_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bulk-text.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE articles");
        q(&c, "CREATE TABLE prior(n INTEGER)");
        q(&c, "BEGIN");
        // Exercise multiple frontend batches and the final partial batch,
        // nullable fields and stable document-to-rowid mapping.
        for n in 0..1001 {
            let body = match n % 3 {
                0 => "",
                1 => ",body:null",
                _ => ",body:'secondary'",
            };
            q(&c, &format!("INSERT INTO articles {{id:type::record('articles','p{n:04}'),title:'common token{n}'{body}}}"));
        }
        q(&c, "COMMIT");
        q(&c, "BEGIN");
        q(&c, "INSERT INTO prior VALUES(9)");
        q(&c, "SAVEPOINT build");
        let create = "CREATE SEARCH INDEX articles_text ON articles(title,body) USING FULLTEXT";
        q(&c, create);
        for n in [0, 999, 1000] {
            let rows = hits(&c, &format!("token{n}"), 10);
            assert_eq!(rows.len(), 1);
            assert!(
                matches!(&rows[0][0], Value::Record(r) if r.key == romedb::Key::String(format!("p{n:04}")))
            );
        }
        assert_eq!(hits(&c, "common", 10000).len(), 1001);
        assert_eq!(hits(&c, "secondary", 10000).len(), 333);
        assert_eq!(
            c.check_collection_integrity("articles", Default::default())
                .unwrap()
                .index_entries,
            1001
        );
        q(&c, "ROLLBACK TO build");
        q(&c, "RELEASE build");
        assert!(c
            .execute(
                "SELECT * FROM search::text('articles_text','common',10)",
                &Parameters::new()
            )
            .is_err());
        q(&c, create);
        q(&c, "COMMIT");
        assert_eq!(
            q(&c, "SELECT n FROM prior").rows,
            vec![vec![Value::Integer(9)]]
        );
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(hits(&c, "common", 10000).len(), 1001);
    assert_eq!(hits(&c, "secondary", 10000).len(), 333);
    assert_eq!(
        c.check_collection_integrity("articles", Default::default())
            .unwrap()
            .index_entries,
        1001
    );
}

#[test]
fn text_bulk_build_supports_one_and_sixteen_fields() {
    for fields in [1, 16] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE articles");
        q(&c, "BEGIN");
        for n in 0..129 {
            let mut members = vec![format!("id:type::record('articles','p{n:03}')")];
            for field in 0..fields {
                if field == 0 || n % 3 == 0 {
                    members.push(format!("f{field}:'common token{n} field{field}'"));
                } else if n % 3 == 1 {
                    members.push(format!("f{field}:null"));
                }
            }
            q(
                &c,
                &format!("INSERT INTO articles {{{}}}", members.join(",")),
            );
        }
        q(&c, "COMMIT");
        let columns = (0..fields)
            .map(|n| format!("f{n}"))
            .collect::<Vec<_>>()
            .join(",");
        q(
            &c,
            &format!("CREATE SEARCH INDEX articles_text ON articles({columns}) USING FULLTEXT"),
        );
        assert_eq!(hits(&c, "common", 10000).len(), 129);
        for n in [0, 127, 128] {
            let rows = hits(&c, &format!("token{n}"), 10);
            assert_eq!(rows.len(), 1);
            assert!(
                matches!(&rows[0][0], Value::Record(r) if r.key == romedb::Key::String(format!("p{n:03}")))
            );
        }
        assert_eq!(
            hits(&c, &format!("field{}", fields - 1), 10000).len(),
            if fields == 1 { 129 } else { 43 }
        );
        assert_eq!(
            c.check_collection_integrity("articles", Default::default())
                .unwrap()
                .index_entries,
            129
        );
    }
}

#[test]
fn a_one_field_build_crosses_its_larger_batches() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE articles");
    q(&c, "BEGIN");
    for n in 0..1801 {
        q(&c, &format!("INSERT INTO articles {{id:type::record('articles','p{n:04}'),title:'common token{n}'}}"));
    }
    q(&c, "COMMIT");
    q(
        &c,
        "CREATE SEARCH INDEX articles_text ON articles(title) USING FULLTEXT",
    );
    assert_eq!(hits(&c, "common", 10000).len(), 1801);
    for n in [0, 899, 900, 1799, 1800] {
        let rows = hits(&c, &format!("token{n}"), 10);
        assert!(
            matches!(&rows[..], [row] if matches!(&row[0], Value::Record(r) if r.key == romedb::Key::String(format!("p{n:04}")))),
            "token{n}"
        );
    }
    assert_eq!(
        c.check_collection_integrity("articles", Default::default())
            .unwrap()
            .index_entries,
        1801
    );
}

#[test]
fn failed_text_builds_leave_no_orphans_and_query_inputs_are_explicit() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE prior(n INTEGER)");
    q(&c, "BEGIN");
    for n in 0..128 {
        q(
            &c,
            &format!(
                "INSERT INTO articles {{id:type::record('articles','p{n:03}'),title:'valid'}}"
            ),
        );
    }
    q(&c, "INSERT INTO articles {id:articles:z_invalid,title:42}");
    q(&c, "COMMIT");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO prior VALUES(9)");
    assert!(c
        .execute(
            "CREATE SEARCH INDEX articles_text ON articles(title) USING FULLTEXT",
            &Parameters::new()
        )
        .is_err());
    q(&c, "COMMIT");
    assert_eq!(
        q(&c, "SELECT n FROM prior").rows,
        vec![vec![Value::Integer(9)]]
    );
    drop(db.connect().unwrap());
    q(
        &c,
        "UPDATE articles SET title='hello world' WHERE id=articles:z_invalid",
    );
    q(
        &c,
        "CREATE SEARCH INDEX articles_text ON articles(title) USING FULLTEXT",
    );
    for (query, limit) in [
        ("title:hello", 10),
        ("hello^2", 10),
        ("hello", -1),
        ("hello", 10001),
        ("\"unclosed", 10),
    ] {
        assert!(c
            .execute(
                "SELECT * FROM search::text('articles_text',$q,$n)",
                &Parameters::from([
                    ("$q".into(), Value::String(query.into())),
                    ("$n".into(), Value::Integer(limit))
                ])
            )
            .is_err());
    }
    for sql in [
        "SELECT * FROM __turso_internal_fts_dir_articles_text",
        "DELETE FROM '__turso_internal_fts_dir_articles_text'",
        "CREATE INDEX raw_text ON __romedb_i_61727469636c65735f74657874 USING fts(key)",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    q(&c, "CREATE TABLE native_text(t TEXT)");
    assert!(c
        .execute(
            "CREATE INDEX raw_text ON native_text USING fts(t)",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(hits(&c, "hello", 10).len(), 1);
}

#[test]
fn text_replacement_upsert_and_failed_insert_select_keep_indexes_atomic() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "INSERT INTO articles {id:articles:a,title:'alpha',code:'x'}",
        "INSERT INTO articles {id:articles:b,title:'beta',code:'y'}",
        "CREATE UNIQUE INDEX article_code ON articles(code)",
        "CREATE SEARCH INDEX articles_text ON articles(title) USING FULLTEXT",
        "INSERT OR REPLACE INTO articles(id,title,code) VALUES (articles:c,'replacement','x')",
    ] {
        q(&c, sql);
    }
    assert!(hits(&c, "alpha", 10).is_empty());
    assert_eq!(hits(&c, "replacement", 10).len(), 1);
    q(&c, "UPSERT articles:c {title:'updated',code:'x'}");
    assert!(hits(&c, "replacement", 10).is_empty());
    assert_eq!(hits(&c, "updated", 10).len(), 1);
    let before = hits(&c, "*", 10);
    q(&c, "CREATE TABLE incoming(title)");
    q(&c, "INSERT INTO incoming VALUES ('valid'),(42)");
    assert!(c
        .execute(
            "INSERT INTO articles(title) SELECT title FROM incoming",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(hits(&c, "*", 10), before);
    assert_eq!(
        c.check_collection_integrity("articles", Default::default())
            .unwrap()
            .documents,
        2
    );
}

#[test]
fn text_query_grammar_preserves_exclusions_and_rejects_negative_only_subclauses() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "INSERT INTO articles {id:articles:a,title:'Fast database systems'}",
        "INSERT INTO articles {id:articles:b,title:'Slow data warehouse'}",
        "INSERT INTO articles {id:articles:c,title:'Fast database guide'}",
        "CREATE SEARCH INDEX articles_text ON articles(title) USING FULLTEXT",
    ] {
        q(&c, sql);
    }
    for (query, count) in [
        ("\"fast database\"", 2),
        ("\"fast data\"*", 2),
        ("fast OR warehouse", 3),
        ("+fast -guide", 1),
        ("fast warehouse", 3),
        ("", 0),
        ("*", 3),
    ] {
        assert_eq!(hits(&c, query, 10).len(), count, "{query}");
    }
    for query in [
        "fast AND NOT guide",
        "fast OR NOT guide",
        "NOT guide",
        "fast AND (-guide)",
    ] {
        let error = c
            .execute(
                "SELECT id FROM search::text('articles_text',$query,10)",
                &Parameters::from([("$query".into(), Value::String(query.into()))]),
            )
            .unwrap_err();
        assert_eq!(error.code(), "RDB_VALIDATION", "{query}: {error}");
        assert!(error.to_string().contains("negative-only"));
    }
}

#[test]
fn text_index_recovers_committed_wal_after_process_exit() {
    const CHILD_PATH: &str = "ROMEDB_FTS_CRASH_TEST_PATH";
    if let Ok(path) = std::env::var(CHILD_PATH) {
        let db = Database::open(&path).unwrap();
        let c = db.connect().unwrap();
        q(&c, "INSERT INTO articles {id:articles:a,title:'committed'}");
        q(
            &c,
            "CREATE SEARCH INDEX articles_text ON articles(title) USING FULLTEXT",
        );
        q(&c, "INSERT INTO articles {id:articles:b,title:'durable'}");
        q(&c, "BEGIN");
        q(&c, "UPDATE articles SET title='uncommitted'");
        assert_eq!(hits(&c, "uncommitted", 10).len(), 2);
        // Bypass connection/database destructors and graceful checkpoints.
        std::process::exit(73);
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crash.db");
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "text_index_recovers_committed_wal_after_process_exit",
        ])
        .env(CHILD_PATH, &path)
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(73),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(hits(&c, "committed", 10).len(), 1);
    assert_eq!(hits(&c, "durable", 10).len(), 1);
    assert!(hits(&c, "uncommitted", 10).is_empty());
    assert_eq!(
        c.check_collection_integrity("articles", Default::default())
            .unwrap()
            .documents,
        2
    );
}

fn search_error(c: &romedb::Connection, index: &str, query: &str) -> romedb::Error {
    c.execute(
        &format!("SELECT id FROM search::text('{index}',$query,10)"),
        &Parameters::from([("$query".into(), Value::String(query.into()))]),
    )
    .unwrap_err()
}

#[test]
fn a_prefix_finds_the_words_that_start_with_it() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "INSERT INTO articles {id:articles:a,title:'Hello world'}",
        "INSERT INTO articles {id:articles:b,title:'Help desk'}",
        "INSERT INTO articles {id:articles:c,title:'Shell game'}",
        "INSERT INTO articles {id:articles:d,title:'Fast help'}",
        "CREATE SEARCH INDEX articles_text ON articles(title) USING FULLTEXT",
    ] {
        q(&c, sql);
    }
    for (query, count) in [
        ("hel*", 3),
        ("HEL*", 3),
        ("\"hel\"*", 3),
        ("+fast +hel*", 1),
        ("hel* -desk", 2),
        ("+game +hel*", 0),
        ("xyz*", 0),
        ("\"fast he\"*", 1),
        ("hel", 0),
    ] {
        assert_eq!(hits(&c, query, 10).len(), count, "{query}");
    }
    for (query, message) in [
        ("h*", "at least 2 characters"),
        ("\"h\"*", "at least 2 characters"),
        ("!*", "at least 2 characters"),
        ("hel**", "one * at the end"),
        ("he*l*", "one * at the end"),
    ] {
        let error = search_error(&c, "articles_text", query);
        assert_eq!(error.code(), "RDB_VALIDATION", "{query}: {error}");
        assert!(error.to_string().contains(message), "{query}: {error}");
    }
}

#[test]
fn field_weights_rank_title_matches_first_and_last_through_reopen_reindex_and_export() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("weights.romedb");
    std::fs::File::create(&path).unwrap();
    let db = Database::create_mvcc(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "INSERT INTO articles {id:articles:a,title:'rust guide book',body:'how to write programs well'}",
        "INSERT INTO articles {id:articles:b,title:'other',body:'rust'}",
        "INSERT INTO articles {id:articles:c,title:'more',body:'nothing here'}",
        "CREATE SEARCH INDEX plain ON articles(title,body) USING FULLTEXT",
        "CREATE SEARCH INDEX weighted ON articles(title,body) USING FULLTEXT WITH (weights='title=3')",
    ] {
        q(&c, sql);
    }
    let best = |c: &romedb::Connection, index: &str| {
        let rows = q(
            c,
            &format!("SELECT id FROM search::text('{index}','rust',10) ORDER BY score DESC"),
        )
        .rows;
        rows.iter()
            .map(|row| format!("{:?}", row[0]))
            .collect::<Vec<_>>()
    };
    let plain = best(&c, "plain");
    let weighted = best(&c, "weighted");
    assert_eq!(plain.len(), 2);
    assert!(plain[0].contains('b'), "{plain:?}");
    assert!(weighted[0].contains('a'), "{weighted:?}");

    q(
        &c,
        "CREATE SEARCH INDEX IF NOT EXISTS weighted ON articles(title,body) USING FULLTEXT WITH (weights='title=3,body=1')",
    );
    let error = c
        .execute(
            "CREATE SEARCH INDEX IF NOT EXISTS weighted ON articles(title,body) USING FULLTEXT WITH (weights='title=2')",
            &Parameters::new(),
        )
        .unwrap_err();
    assert_eq!(error.code(), "RDB_ALREADY_EXISTS", "{error}");
    let info = format!("{:?}", q(&c, "INFO FOR TABLE articles"));
    assert!(info.contains("\"weights\""), "{info}");

    q(&c, "REINDEX weighted");
    assert_eq!(best(&c, "weighted"), weighted);
    let copy = directory.path().join("copy.romedb");
    db.start_file_export(
        &copy,
        &Default::default(),
        &romedb::CancellationToken::new(),
    )
    .unwrap()
    .finish()
    .unwrap();
    drop(c);
    drop(db);
    for file in [&path, &copy] {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        assert_eq!(best(&c, "weighted"), weighted, "{}", file.display());
        assert_eq!(best(&c, "plain"), plain, "{}", file.display());
    }
}

#[test]
fn field_weights_must_name_index_paths_with_positive_numbers() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE articles");
    for weights in [
        "title=0",
        "title=-1",
        "title=abc",
        "title=inf",
        "title=NaN",
        "nope=2",
        "title=2,title=3",
        "title",
        "",
        "meta=2",
    ] {
        let error = c
            .execute(
                &format!("CREATE SEARCH INDEX w ON articles(title,meta.summary) USING FULLTEXT WITH (weights='{weights}')"),
                &Parameters::new(),
            )
            .unwrap_err();
        assert_eq!(error.code(), "RDB_VALIDATION", "{weights}: {error}");
    }
    q(
        &c,
        "CREATE SEARCH INDEX w ON articles(title,meta.summary) USING FULLTEXT WITH (weights='meta.summary=0.5', tokenizer='simple')",
    );
    q(
        &c,
        "INSERT INTO articles {title:'Rust',meta:{summary:'Rust'}}",
    );
    assert_eq!(
        q(&c, "SELECT id FROM search::text('w','Rust',10)")
            .rows
            .len(),
        1
    );
}

fn parts(value: &Value) -> Vec<(String, bool)> {
    let Value::Array(parts) = value else {
        panic!("{value:?}")
    };
    parts
        .iter()
        .map(|part| {
            let Value::Object(part) = part else { panic!() };
            let (Value::String(text), Value::Boolean(matched)) = (&part["text"], &part["match"])
            else {
                panic!("{part:?}")
            };
            (text.clone(), *matched)
        })
        .collect()
}
fn snippet(c: &romedb::Connection, index: &str, text: &str, query: &str) -> Vec<(String, bool)> {
    parts(&c.snippet_text(index, text, query, 40).unwrap())
}
fn part(text: &str, matched: bool) -> (String, bool) {
    (text.into(), matched)
}
fn marked(parts: &[(String, bool)]) -> Vec<&str> {
    parts
        .iter()
        .filter(|(_, matched)| *matched)
        .map(|(text, _)| text.as_str())
        .collect()
}

#[test]
fn a_snippet_marks_the_query_words_in_the_best_fragment() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(
        &c,
        "CREATE SEARCH INDEX docs_text ON docs(title,body) USING FULLTEXT",
    );
    let fox = "The quick brown fox jumps over the lazy dog.";
    assert_eq!(
        snippet(&c, "docs_text", fox, "FOX"),
        vec![
            part("The quick brown ", false),
            part("fox", true),
            part(" jumps over the lazy\u{2026}", false)
        ]
    );
    assert_eq!(
        snippet(&c, "docs_text", fox, "\"lazy dog\""),
        vec![
            part("\u{2026}quick brown fox jumps over the ", false),
            part("lazy dog", true),
            part(".", false)
        ]
    );
    for (query, expected) in [
        ("jum*", vec!["jumps"]),
        ("+quick +jum*", vec!["quick", "jumps"]),
        ("\"brown fo\"*", vec!["brown fox"]),
        ("\"quick fox\"~1", vec!["quick", "fox"]),
        ("\"quick fox\"", vec![]),
        ("fox -dog", vec!["fox"]),
        ("(cat OR fox) AND lazy", vec!["fox", "lazy"]),
    ] {
        assert_eq!(
            marked(&snippet(&c, "docs_text", fox, query)),
            expected,
            "{query}"
        );
    }
    let text = format!(
        "alpha {}alpha beta{}",
        "filler ".repeat(30),
        " filler".repeat(30)
    );
    let best = snippet(&c, "docs_text", &text, "alpha beta");
    assert_eq!(marked(&best), vec!["alpha", "beta"]);
    assert!(best[0].0.starts_with('\u{2026}') && best.last().unwrap().0.ends_with('\u{2026}'));
    let early = snippet(&c, "docs_text", &text, "zebra");
    assert_eq!(early.len(), 1);
    assert!(
        !early[0].1 && early[0].0.starts_with("alpha filler") && early[0].0.ends_with('\u{2026}')
    );
    assert_eq!(
        snippet(&c, "docs_text", "alpha\n\n   beta  ", "beta"),
        vec![part("alpha ", false), part("beta", true)]
    );
    assert!(snippet(&c, "docs_text", "", "beta").is_empty());
    assert!(snippet(&c, "docs_text", " \n ", "beta").is_empty());
    let long = "word ".repeat(400);
    for size in [40, 160, 1000] {
        let cut = parts(&c.snippet_text("docs_text", &long, "word", size).unwrap());
        let joined: String = cut.iter().map(|(text, _)| text.as_str()).collect();
        let joined = joined.trim_end_matches('\u{2026}');
        assert!(joined.chars().count() <= size, "{size}");
        assert!(long.starts_with(joined), "{size}");
        assert_eq!(marked(&cut).len(), (size + 1) / 5, "{size}");
    }
}

#[test]
fn a_snippet_follows_the_index_tokenizer_and_unicode_text() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT");
    q(&c, "CREATE SEARCH INDEX grams ON docs(body) USING FULLTEXT WITH(tokenizer='ngram',min_gram=2,max_gram=3)");
    q(
        &c,
        "CREATE SEARCH INDEX exact ON docs(body) USING FULLTEXT WITH(tokenizer='raw')",
    );
    q(
        &c,
        "CREATE SEARCH INDEX cased ON docs(body) USING FULLTEXT WITH(tokenizer='simple')",
    );
    let vietnamese = "Tiếng Việt có dấu: Hà Nội là thủ đô của VIỆT Nam 🇻🇳 và có nhiều hồ đẹp.";
    assert_eq!(
        marked(&snippet(&c, "words", vietnamese, "việt hồ")),
        vec!["VIỆT", "hồ"]
    );
    let found = parts(&c.snippet_text("words", vietnamese, "việt hồ", 160).unwrap());
    assert_eq!(marked(&found), vec!["Việt", "VIỆT", "hồ"]);
    assert_eq!(
        found
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<String>(),
        vietnamese
    );
    let emoji = format!("{}needle{}", "😀 ".repeat(100), " 😀".repeat(100));
    let around = snippet(&c, "words", &emoji, "needle");
    let joined: String = around.iter().map(|(text, _)| text.as_str()).collect();
    let inner = joined.trim_matches('\u{2026}');
    assert!(
        emoji.contains(inner) && inner.chars().count() <= 40,
        "{joined}"
    );
    assert_eq!(marked(&around), vec!["needle"]);
    assert_eq!(
        snippet(&c, "grams", "Database systems store DATA", "data"),
        vec![
            part("Data", true),
            part("base systems store ", false),
            part("DATA", true)
        ]
    );
    assert_eq!(
        marked(&snippet(&c, "grams", "Hello, Hà Nội", "hel* nộ")),
        vec!["Hel", "Nộ"]
    );
    assert!(marked(&snippet(&c, "grams", "abc", "a")).is_empty());
    assert_eq!(
        marked(&snippet(&c, "exact", "Hello World", "\"Hello World\"")),
        vec!["Hello World"]
    );
    assert!(marked(&snippet(&c, "exact", "Hello World", "hello")).is_empty());
    assert!(marked(&snippet(&c, "cased", "Hello hello", "hello")) == vec!["hello"]);
}

#[test]
fn a_snippet_works_in_queries_writes_and_rejects_bad_arguments() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(
        &c,
        "CREATE SEARCH INDEX docs_text ON docs(title,body) USING FULLTEXT",
    );
    q(&c, "CREATE INDEX docs_title ON docs(title)");
    q(
        &c,
        "INSERT INTO docs {id:docs:a,title:'Hello',body:'Say hello to the world'}",
    );
    q(&c, "INSERT INTO docs {id:docs:b,title:'Bye',body:null}");
    let query = |sql: &str| {
        c.execute(
            sql,
            &Parameters::from([("$q".into(), Value::String("hello".into()))]),
        )
    };
    let result = query("SELECT h.id,search::snippet('docs_text',d.body,$q,40) AS snippet FROM search::text('docs_text',$q,10) h JOIN docs d ON d.id=h.id").unwrap();
    assert_eq!(result.columns[1], "snippet");
    assert_eq!(
        parts(&result.rows[0][1]),
        vec![
            part("Say ", false),
            part("hello", true),
            part(" to the world", false)
        ]
    );
    let result =
        query("SELECT search::snippet('docs_text',body,$q,40) FROM docs ORDER BY id").unwrap();
    assert!(
        !result.columns[0].contains("__romedb"),
        "{}",
        result.columns[0]
    );
    assert_eq!(result.rows[1][0], Value::Null);
    assert_eq!(
        q(&c, "SELECT search::snippet('docs_text','hello',null,40)").rows,
        vec![vec![Value::Null]]
    );
    q(&c, "INSERT INTO results {id:results:a,parts:search::snippet('docs_text','hello world','world',40)}");
    q(
        &c,
        "UPDATE results SET parts=search::snippet('docs_text','bye world','bye',40)",
    );
    assert_eq!(
        parts(&q(&c, "SELECT parts FROM results").rows[0][0]),
        vec![part("bye", true), part(" world", false)]
    );
    for (sql, code) in [
        (
            "SELECT search::snippet('docs_text',body,$q,39) FROM docs",
            "RDB_VALIDATION",
        ),
        (
            "SELECT search::snippet('docs_text',body,$q,1001) FROM docs",
            "RDB_VALIDATION",
        ),
        (
            "SELECT search::snippet('docs_text',body,$q,'40') FROM docs",
            "RDB_VALIDATION",
        ),
        (
            "SELECT search::snippet('docs_text',body,'h*',40) FROM docs WHERE $q=$q",
            "RDB_VALIDATION",
        ),
        (
            "SELECT search::snippet('docs_text',body,'title:hello',40) FROM docs WHERE $q=$q",
            "RDB_VALIDATION",
        ),
        (
            "SELECT search::snippet('docs_text',7,$q,40)",
            "RDB_VALIDATION",
        ),
        (
            "SELECT search::snippet('docs_text',body,7,40) FROM docs WHERE $q=$q",
            "RDB_VALIDATION",
        ),
        (
            "SELECT search::snippet('docs_title',body,$q,40) FROM docs",
            "RDB_VALIDATION",
        ),
        (
            "SELECT search::snippet('missing',body,$q,40) FROM docs",
            "RDB_NOT_FOUND",
        ),
    ] {
        assert_eq!(query(sql).unwrap_err().code(), code, "{sql}");
    }
    assert!(query("SELECT search::snippet($q,body,$q,40) FROM docs").is_err());
    let error = c
        .snippet_text("docs_text", &"x".repeat(1024 * 1024 + 1), "hello", 40)
        .unwrap_err();
    assert_eq!(error.code(), "RDB_LIMIT");
    assert_eq!(
        parts(&c.snippet_text("docs_text", "hello", "hello", 41).unwrap()),
        vec![part("hello", true)]
    );
}

fn ids(c: &romedb::Connection, index: &str, query: &str) -> Vec<String> {
    c.execute(
        &format!("SELECT id FROM search::text('{index}',$query,10) ORDER BY id"),
        &Parameters::from([("$query".into(), Value::String(query.into()))]),
    )
    .unwrap_or_else(|e| panic!("{index} {query}: {e}"))
    .rows
    .into_iter()
    .map(|row| match &row[0] {
        Value::Record(romedb::Record {
            key: romedb::Key::String(key),
            ..
        }) => key.clone(),
        other => panic!("{other:?}"),
    })
    .collect()
}

#[test]
fn folding_and_stemming_match_accents_and_word_forms_through_reindex_and_export() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("stemming.romedb");
    std::fs::File::create(&path).unwrap();
    let db = Database::create_mvcc(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "INSERT INTO articles {id:articles:a,title:'Café crème',body:'running shoes for the trail'}",
        "INSERT INTO articles {id:articles:b,title:'Runs',body:'he runs daily'}",
        "INSERT INTO articles {id:articles:c,title:'Résumé tips',body:'information about the agreed plan'}",
        "INSERT INTO articles {id:articles:d,title:'Cart',body:'a cartoon show'}",
        "CREATE SEARCH INDEX plain ON articles(title,body) USING FULLTEXT",
        "CREATE SEARCH INDEX stemmed ON articles(title,body) USING FULLTEXT WITH (fold=true, stemmer='English')",
    ] {
        q(&c, sql);
    }
    let checks = |c: &romedb::Connection| {
        for (index, query, expected) in [
            ("stemmed", "cafe", vec!["a"]),
            ("stemmed", "CAFÉ", vec!["a"]),
            ("stemmed", "resume", vec!["c"]),
            ("stemmed", "run", vec!["a", "b"]),
            ("stemmed", "\"running shoes\"", vec!["a"]),
            ("stemmed", "\"run shoe\"", vec!["a"]),
            ("stemmed", "\"shoes running\"", vec![]),
            ("stemmed", "runni*", vec!["a", "b"]),
            ("stemmed", "informatio*", vec!["c"]),
            ("stemmed", "agr*", vec!["c"]),
            ("stemmed", "carto*", vec!["d"]),
            ("stemmed", "+show +carto*", vec!["d"]),
            ("plain", "cafe", vec![]),
            ("plain", "run", vec![]),
            ("plain", "runs", vec!["b"]),
        ] {
            assert_eq!(ids(c, index, query), expected, "{index} {query}");
        }
    };
    checks(&c);
    assert_eq!(
        q(
            &c,
            "SELECT search::analyze('stemmed','Cafés RUNNING agreed')"
        )
        .rows[0][0],
        c.analyze_text("stemmed", "Cafés RUNNING agreed").unwrap()
    );
    let tokens = c.analyze_text("stemmed", "Cafés RUNNING agreed").unwrap();
    assert_eq!(
        format!("{tokens:?}").matches("\"cafe\"").count(),
        1,
        "{tokens:?}"
    );
    for word in ["\"run\"", "\"agre\""] {
        assert!(format!("{tokens:?}").contains(word), "{tokens:?}");
    }
    assert_eq!(
        marked(&snippet(
            &c,
            "stemmed",
            "He was running to the café",
            "run cafe"
        )),
        vec!["running", "café"]
    );
    assert_eq!(
        marked(&snippet(&c, "stemmed", "Information desks", "informatio*")),
        vec!["Information"]
    );
    assert_eq!(
        marked(&snippet(&c, "stemmed", "running and runs", "runni*")),
        vec!["running", "runs"]
    );
    assert_eq!(
        marked(&snippet(&c, "stemmed", "a cart and a cartoon", "carto*")),
        vec!["cartoon"]
    );
    assert_eq!(
        marked(&snippet(
            &c,
            "stemmed",
            "running shoes, shoes running",
            "\"run shoe\""
        )),
        vec!["running shoes"]
    );
    assert_eq!(
        marked(&snippet(
            &c,
            "plain",
            "He was running to the café",
            "run cafe"
        )),
        Vec::<&str>::new()
    );
    let info = format!("{:?}", q(&c, "INFO FOR TABLE articles"));
    assert!(info.contains("\"fold\": Boolean(true)"), "{info}");
    assert!(info.contains("\"stemmer\": String(\"english\")"), "{info}");
    q(
        &c,
        "CREATE SEARCH INDEX IF NOT EXISTS stemmed ON articles(title,body) USING FULLTEXT WITH (stemmer='english',fold=true)",
    );
    for options in [
        "fold=true",
        "stemmer='english'",
        "fold=true,stemmer='french'",
    ] {
        let error = c
            .execute(
                &format!("CREATE SEARCH INDEX IF NOT EXISTS stemmed ON articles(title,body) USING FULLTEXT WITH ({options})"),
                &Parameters::new(),
            )
            .unwrap_err();
        assert_eq!(error.code(), "RDB_ALREADY_EXISTS", "{options}: {error}");
    }
    q(&c, "REINDEX stemmed");
    checks(&c);
    let copy = directory.path().join("copy.romedb");
    db.start_file_export(
        &copy,
        &Default::default(),
        &romedb::CancellationToken::new(),
    )
    .unwrap()
    .finish()
    .unwrap();
    drop(c);
    drop(db);
    for file in [&path, &copy] {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        checks(&c);
        q(
            &c,
            "INSERT INTO articles {id:articles:e,title:'Naïve runners'}",
        );
        assert_eq!(ids(&c, "stemmed", "naive runner"), vec!["e"]);
    }
}

#[test]
fn fold_and_stemmer_options_are_checked() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE articles");
    for (options, code) in [
        ("stemmer='klingon'", "RDB_VALIDATION"),
        ("tokenizer='ngram',fold=true", "RDB_VALIDATION"),
        ("tokenizer='raw',fold=true", "RDB_VALIDATION"),
        ("tokenizer='simple',stemmer='english'", "RDB_VALIDATION"),
        ("tokenizer='whitespace',stemmer='english'", "RDB_VALIDATION"),
        ("tokenizer='ngram',stemmer='english'", "RDB_VALIDATION"),
        ("fold=1", "RDB_SYNTAX"),
        ("fold='true'", "RDB_SYNTAX"),
        ("fold=yes", "RDB_SYNTAX"),
        ("stemmer=english", "RDB_SYNTAX"),
        ("fold=true,fold=false", "RDB_SYNTAX"),
    ] {
        let error = c
            .execute(
                &format!(
                    "CREATE SEARCH INDEX bad ON articles(title) USING FULLTEXT WITH ({options})"
                ),
                &Parameters::new(),
            )
            .unwrap_err();
        assert_eq!(error.code(), code, "{options}: {error}");
    }
    let error = c
        .execute(
            "CREATE SEARCH INDEX bad ON articles(title) USING FULLTEXT WITH (stemmer='klingon')",
            &Parameters::new(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("english"), "{error}");
    for (index, options) in [
        ("simple_folded", "tokenizer='simple',fold=true"),
        ("whitespace_folded", "tokenizer='whitespace',fold=true"),
        ("unfolded", "fold=false"),
        ("german", "stemmer='german'"),
    ] {
        q(
            &c,
            &format!(
                "CREATE SEARCH INDEX {index} ON articles(title) USING FULLTEXT WITH ({options})"
            ),
        );
    }
    q(
        &c,
        "INSERT INTO articles {id:articles:a,title:'CAFÉ Häuser'}",
    );
    for (index, query, count) in [
        ("simple_folded", "CAFE", 1),
        ("simple_folded", "cafe", 0),
        ("whitespace_folded", "CAFE", 1),
        ("unfolded", "cafe", 0),
        ("german", "haus", 1),
    ] {
        assert_eq!(ids(&c, index, query).len(), count, "{index} {query}");
    }
    let info = format!("{:?}", q(&c, "INFO FOR TABLE articles"));
    assert_eq!(info.matches("\"fold\": Boolean(true)").count(), 2, "{info}");
    assert!(info.contains("\"stemmer\": String(\"german\")"), "{info}");
}

fn filtered(
    c: &romedb::Connection,
    query: &str,
    limit: i64,
    allowed: Value,
    filter: Value,
) -> romedb::Result<Vec<(String, f64)>> {
    let rows = c
        .execute(
            "SELECT id,score FROM search::text('cms_text',$q,$n,$allowed,$filter) ORDER BY score DESC,id",
            &Parameters::from([
                ("$q".into(), Value::String(query.into())),
                ("$n".into(), Value::Integer(limit)),
                ("$allowed".into(), allowed),
                ("$filter".into(), filter),
            ]),
        )?
        .rows;
    Ok(rows
        .into_iter()
        .map(|row| match (&row[0], &row[1]) {
            (
                Value::Record(romedb::Record {
                    key: romedb::Key::String(key),
                    ..
                }),
                Value::Number(score),
            ) => (key.clone(), *score),
            other => panic!("{other:?}"),
        })
        .collect())
}
fn filter(fields: &[(&str, Value)]) -> Value {
    Value::Object(
        fields
            .iter()
            .map(|(path, value)| (path.to_string(), value.clone()))
            .collect(),
    )
}
fn strings(values: &[&str]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|v| Value::String(v.to_string()))
            .collect(),
    )
}
fn keys(hits: &[(String, f64)]) -> Vec<&str> {
    hits.iter().map(|(key, _)| key.as_str()).collect()
}

#[test]
fn a_filter_finds_the_best_hits_inside_it_whenever_they_exist() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE cms");
    q(&c, "BEGIN");
    for n in 0..10_000 {
        let (locale, text, collection) = if n % 250 == 0 {
            (
                "vi",
                format!("common {}", "filler ".repeat(20 + n % 7)),
                ["posts", "pages"][(n / 250) % 2],
            )
        } else {
            (
                ["en", "fr", "de"][n % 3],
                "common common".to_owned(),
                ["posts", "pages"][n % 2],
            )
        };
        q(
            &c,
            &format!("INSERT INTO cms {{id:type::record('cms','d{n:05}'),title:'t',text:'{text}',locale:'{locale}',collection:'{collection}'}}"),
        );
    }
    q(&c, "COMMIT");
    q(
        &c,
        "CREATE SEARCH INDEX cms_text ON cms(title,text) USING FULLTEXT WITH (filters='locale,collection')",
    );
    let vi: Vec<String> = (0..10_000)
        .step_by(250)
        .map(|n| format!("d{n:05}"))
        .collect();
    let everything = filtered(&c, "common", 10_000, Value::Null, Value::Null).unwrap();
    assert_eq!(everything.len(), 10_000);
    let best_vi: Vec<(String, f64)> = everything
        .iter()
        .filter(|(key, _)| vi.contains(key))
        .cloned()
        .collect();
    assert_eq!(best_vi.len(), 40);
    let top = filtered(&c, "common", 20, Value::Null, Value::Null).unwrap();
    assert!(keys(&top).iter().all(|key| !vi.iter().any(|v| v == key)));

    let only_vi = filter(&[("locale", Value::String("vi".into()))]);
    assert_eq!(
        filtered(&c, "common", 20, Value::Null, only_vi.clone()).unwrap(),
        best_vi[..20]
    );
    assert_eq!(
        filtered(&c, "common", 50, Value::Null, only_vi.clone()).unwrap(),
        best_vi
    );
    assert_eq!(
        filtered(
            &c,
            "common",
            20,
            Value::Null,
            filter(&[("locale", strings(&["vi", "xx"]))])
        )
        .unwrap(),
        best_vi[..20]
    );
    let vi_pages = filtered(
        &c,
        "common",
        50,
        Value::Null,
        filter(&[
            ("locale", Value::String("vi".into())),
            ("collection", Value::String("pages".into())),
        ]),
    )
    .unwrap();
    assert_eq!(vi_pages.len(), 20);
    assert!(vi_pages.iter().all(|hit| best_vi.contains(hit)));
    assert_eq!(
        filtered(
            &c,
            "common",
            20,
            Value::Null,
            filter(&[("locale", strings(&[]))])
        )
        .unwrap(),
        []
    );
    assert_eq!(
        filtered(
            &c,
            "common",
            20,
            Value::Null,
            filter(&[("locale", Value::String("xx".into()))])
        )
        .unwrap(),
        []
    );
    assert_eq!(
        filtered(&c, "*", 50, Value::Null, only_vi.clone())
            .unwrap()
            .len(),
        40
    );
    let allowed = Value::Array(
        ["d00000", "d00001", "d00250"]
            .iter()
            .map(|key| {
                Value::Record(romedb::Record {
                    table: "cms".into(),
                    key: romedb::Key::String(key.to_string()),
                })
            })
            .collect(),
    );
    let both = filtered(&c, "common", 20, allowed.clone(), only_vi.clone()).unwrap();
    assert_eq!(keys(&both), ["d00000", "d00250"]);
    assert!(both.iter().all(|hit| best_vi.contains(hit)));
    assert_eq!(
        filtered(&c, "common", 20, allowed, Value::Null)
            .unwrap()
            .len(),
        3
    );
    let literal = q(
        &c,
        "SELECT id FROM search::text('cms_text','common',5,NULL,NULL)",
    );
    assert_eq!(literal.rows.len(), 5);
}

#[test]
fn filters_are_checked_and_kept_through_updates_reindex_reopen_and_export() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("filters.romedb");
    std::fs::File::create(&path).unwrap();
    let db = Database::create_mvcc(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE cms");
    let tricky = "a\"b\\c (d)";
    let doc = |key: &str, text: &str, locale: Value| {
        let mut fields = romedb::Document::new();
        fields.insert(
            "id".into(),
            Value::Record(romedb::Record {
                table: "cms".into(),
                key: romedb::Key::String(key.into()),
            }),
        );
        fields.insert("title".into(), Value::String("news".into()));
        fields.insert("text".into(), Value::String(text.into()));
        fields.insert("meta".into(), {
            let mut meta = romedb::Document::new();
            meta.insert("locale".into(), locale);
            Value::Object(meta)
        });
        Parameters::from([("$doc".into(), Value::Object(fields))])
    };
    for (key, text, locale) in [
        ("a", "rust release", Value::String("en".into())),
        ("b", "rust guide", Value::String("vi".into())),
        ("c", "rust notes", Value::String(tricky.into())),
        ("d", "rust en route", Value::Null),
    ] {
        c.execute("INSERT INTO cms DOCUMENT $doc", &doc(key, text, locale))
            .unwrap();
    }
    q(
        &c,
        "CREATE SEARCH INDEX cms_text ON cms(title,text) USING FULLTEXT WITH (filters='meta.locale')",
    );
    let by_locale = |c: &romedb::Connection, locale: &str| {
        keys(
            &filtered(
                c,
                "rust",
                10,
                Value::Null,
                filter(&[("meta.locale", Value::String(locale.into()))]),
            )
            .unwrap(),
        )
        .iter()
        .map(|key| key.to_string())
        .collect::<Vec<_>>()
    };
    assert_eq!(by_locale(&c, "en"), ["a"]);
    assert_eq!(by_locale(&c, tricky), ["c"]);
    assert_eq!(by_locale(&c, "EN"), Vec::<String>::new());
    assert_eq!(ids(&c, "cms_text", "en"), ["d"]);

    let error = c
        .execute(
            "INSERT INTO cms DOCUMENT $doc",
            &doc("e", "rust", Value::Integer(5)),
        )
        .unwrap_err();
    assert_eq!(error.code(), "RDB_VALIDATION", "{error}");
    q(&c, "UPDATE cms:b SET meta.locale='en'");
    assert_eq!(by_locale(&c, "en"), ["a", "b"]);

    for (value, code) in [
        (
            filter(&[("locale", Value::String("en".into()))]),
            "RDB_VALIDATION",
        ),
        (
            filter(&[("title", Value::String("news".into()))]),
            "RDB_VALIDATION",
        ),
        (
            filter(&[("meta.locale", Value::Integer(5))]),
            "RDB_VALIDATION",
        ),
        (filter(&[("meta.locale", Value::Null)]), "RDB_VALIDATION"),
        (
            filter(&[("meta.locale", Value::Array(vec![Value::Integer(5)]))]),
            "RDB_VALIDATION",
        ),
        (Value::String("en".into()), "RDB_VALIDATION"),
        (
            filter(&[("meta.locale", strings(&["en"; 33]))]),
            "RDB_LIMIT",
        ),
        (
            filter(&[("meta.locale", Value::String("x".repeat(257)))]),
            "RDB_LIMIT",
        ),
    ] {
        let error = filtered(&c, "rust", 10, Value::Null, value.clone()).unwrap_err();
        assert_eq!(error.code(), code, "{value:?}: {error}");
    }
    assert_eq!(
        filtered(
            &c,
            "rust",
            10,
            Value::Null,
            filter(&[("meta.locale", strings(&["en"; 32]))])
        )
        .unwrap()
        .len(),
        2
    );

    for options in [
        "filters='title'",
        "filters='meta.locale,meta.locale'",
        "filters='a,b,c,d,e'",
        "filters='meta..locale'",
        "filters=''",
        "filters='meta.locale',weights='meta.locale=2'",
    ] {
        let error = c
            .execute(
                &format!(
                    "CREATE SEARCH INDEX bad ON cms(title,text) USING FULLTEXT WITH ({options})"
                ),
                &Parameters::new(),
            )
            .unwrap_err();
        assert_eq!(error.code(), "RDB_VALIDATION", "{options}: {error}");
    }
    q(
        &c,
        "CREATE SEARCH INDEX IF NOT EXISTS cms_text ON cms(title,text) USING FULLTEXT WITH (filters='meta.locale')",
    );
    let error = c
        .execute(
            "CREATE SEARCH INDEX IF NOT EXISTS cms_text ON cms(title,text) USING FULLTEXT",
            &Parameters::new(),
        )
        .unwrap_err();
    assert_eq!(error.code(), "RDB_ALREADY_EXISTS", "{error}");
    let info = format!("{:?}", q(&c, "INFO FOR TABLE cms"));
    assert!(
        info.contains("\"filters\": Array([Array([String(\"meta\"), String(\"locale\")])])"),
        "{info}"
    );
    assert!(
        info.contains("\"paths\": Array([Array([String(\"title\")]), Array([String(\"text\")])])"),
        "{info}"
    );

    q(&c, "REINDEX cms_text");
    assert_eq!(by_locale(&c, "en"), ["a", "b"]);
    let copy = directory.path().join("copy.romedb");
    db.start_file_export(
        &copy,
        &Default::default(),
        &romedb::CancellationToken::new(),
    )
    .unwrap()
    .finish()
    .unwrap();
    drop(c);
    drop(db);
    for file in [&path, &copy] {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        assert_eq!(by_locale(&c, "en"), ["a", "b"], "{}", file.display());
        assert_eq!(by_locale(&c, tricky), ["c"], "{}", file.display());
    }
}

fn count(c: &romedb::Connection, query: &str, max: i64, filter: Value) -> romedb::Result<i64> {
    let rows = c
        .execute(
            "SELECT count FROM search::count('cms_text',$q,$max,$filter)",
            &Parameters::from([
                ("$q".into(), Value::String(query.into())),
                ("$max".into(), Value::Integer(max)),
                ("$filter".into(), filter),
            ]),
        )?
        .rows;
    match rows.as_slice() {
        [row] => match row.as_slice() {
            [Value::Integer(n)] => Ok(*n),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_count_is_exact_below_max_capped_at_max_and_sees_its_transaction() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE cms");
    q(&c, "BEGIN");
    for n in 0..600 {
        let text = if n % 2 == 0 { "common rare" } else { "common" };
        let locale = ["en", "vi", "fr"][n % 3];
        q(
            &c,
            &format!("INSERT INTO cms {{id:type::record('cms','d{n:04}'),title:'t',text:'{text}',locale:'{locale}'}}"),
        );
    }
    q(&c, "COMMIT");
    q(
        &c,
        "CREATE SEARCH INDEX cms_text ON cms(title,text) USING FULLTEXT WITH (filters='locale')",
    );
    let vi = filter(&[("locale", Value::String("vi".into()))]);
    for (query, max, filter, expected) in [
        ("common", 1000, Value::Null, 600),
        ("common", 600, Value::Null, 600),
        ("common", 599, Value::Null, 599),
        ("common", 1, Value::Null, 1),
        ("rare", 100_000, Value::Null, 300),
        ("+common -rare", 1000, Value::Null, 300),
        ("missing", 10, Value::Null, 0),
        ("comm*", 1000, Value::Null, 600),
        ("common", 1000, vi.clone(), 200),
        ("rare", 1000, vi.clone(), 100),
        ("rare", 50, vi.clone(), 50),
        ("*", 1000, vi.clone(), 200),
    ] {
        assert_eq!(
            count(&c, query, max, filter.clone()).unwrap(),
            expected,
            "{query} {max} {filter:?}"
        );
    }
    assert_eq!(
        q(
            &c,
            "SELECT count FROM search::count('cms_text','rare',1000)"
        )
        .rows,
        [[Value::Integer(300)]]
    );
    let profile = c
        .profile_select(
            "SELECT count FROM search::count('cms_text','common',100000)",
            &Parameters::new(),
        )
        .unwrap();
    eprintln!("count metrics {:?}", profile.metrics);

    q(&c, "BEGIN");
    q(
        &c,
        "INSERT INTO cms {id:cms:new,title:'t',text:'rare',locale:'vi'}",
    );
    q(&c, "DELETE cms:d0000");
    assert_eq!(count(&c, "rare", 1000, Value::Null).unwrap(), 300);
    assert_eq!(count(&c, "rare", 1000, vi.clone()).unwrap(), 101);
    assert_eq!(count(&c, "common", 1000, Value::Null).unwrap(), 599);
    q(&c, "ROLLBACK");
    assert_eq!(count(&c, "rare", 1000, vi.clone()).unwrap(), 100);
    assert_eq!(count(&c, "common", 1000, Value::Null).unwrap(), 600);

    for (max, code) in [
        (0, "RDB_VALIDATION"),
        (100_001, "RDB_VALIDATION"),
        (-1, "RDB_VALIDATION"),
    ] {
        let error = count(&c, "common", max, Value::Null).unwrap_err();
        assert_eq!(error.code(), code, "{max}: {error}");
    }
    for (sql, code) in [
        (
            "SELECT count FROM search::count('cms_text','common',1.5)",
            "RDB_VALIDATION",
        ),
        (
            "SELECT count FROM search::count('cms_text','title:x',10)",
            "RDB_VALIDATION",
        ),
        (
            "SELECT count FROM search::count('nothing','common',10)",
            "RDB_NOT_FOUND",
        ),
        (
            "SELECT count FROM search::count('cms_text','common')",
            "RDB_VALIDATION",
        ),
        (
            "SELECT count FROM search::count('cms_text','common',10,NULL,NULL)",
            "RDB_VALIDATION",
        ),
    ] {
        let error = c.execute(sql, &Parameters::new()).unwrap_err();
        assert_eq!(error.code(), code, "{sql}: {error}");
    }
}

#[test]
fn a_typo_finds_nearby_words_ranks_below_the_word_and_marks_snippets() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    for sql in [
        "INSERT INTO articles {id:articles:a,title:'Database systems',body:'notes'}",
        "INSERT INTO articles {id:articles:b,title:'Databse',body:'a common typo'}",
        "INSERT INTO articles {id:articles:c,title:'Rust',body:'lang'}",
        "INSERT INTO articles {id:articles:d,title:'Bust',body:'tales'}",
        "INSERT INTO articles {id:articles:e,title:'Café crème',body:'running shoes'}",
        "INSERT INTO articles {id:articles:f,title:'Performance',body:'tuning'}",
        "CREATE SEARCH INDEX plain ON articles(title,body) USING FULLTEXT",
        "CREATE SEARCH INDEX folded ON articles(title,body) USING FULLTEXT WITH (fold=true, stemmer='english')",
        "CREATE SEARCH INDEX grams ON articles(title) USING FULLTEXT WITH (tokenizer='ngram')",
    ] {
        q(&c, sql);
    }
    for (index, query, expected) in [
        ("plain", "databse~1", vec!["a", "b"]),
        ("plain", "DATABSE~1", vec!["a", "b"]),
        ("plain", "rust~1", vec!["c", "d"]),
        ("plain", "rus~1", vec![]),
        ("plain", "perfromance~2", vec!["f"]),
        ("plain", "+databse~1 +notes", vec!["a"]),
        ("plain", "databse~1 -typo", vec!["a"]),
        ("plain", "\"common typo\"~1", vec!["b"]),
        ("folded", "kafe~1", vec!["e"]),
        ("folded", "shoez~1", vec!["e"]),
        ("grams", "rust~1", vec!["c"]),
    ] {
        assert_eq!(ids(&c, index, query), expected, "{index} {query}");
    }
    let ranked = q(
        &c,
        "SELECT id FROM search::text('plain','rust~1',10) ORDER BY score DESC",
    )
    .rows;
    assert!(
        matches!(&ranked[0][0], Value::Record(r) if r.key == romedb::Key::String("c".into())),
        "{ranked:?}"
    );
    for query in ["rust~3", "rust~0", "ru*~1", "~1", "rust~12"] {
        assert_eq!(
            search_error(&c, "plain", query).code(),
            "RDB_VALIDATION",
            "{query}"
        );
    }
    assert_eq!(
        marked(&snippet(
            &c,
            "plain",
            "We compare the databse with a database and a datum.",
            "databse~1"
        )),
        ["databse", "database"]
    );
    assert_eq!(
        marked(&snippet(
            &c,
            "plain",
            "rust bust just crust trust",
            "rust~1"
        )),
        ["rust", "bust", "just", "crust", "trust"]
    );
    assert_eq!(
        marked(&snippet(&c, "plain", "rus bus us", "rus~1")),
        ["rus"]
    );
    assert_eq!(
        marked(&snippet(&c, "folded", "Le café est bon", "kafe~1")),
        ["café"]
    );
}
