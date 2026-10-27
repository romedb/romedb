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
        assert_eq!(error.code(), "FDB_VALIDATION", "{query}: {error}");
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
