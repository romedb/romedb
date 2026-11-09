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
