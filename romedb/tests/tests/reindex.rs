use romedb::{Database, Parameters, ResultLimits, SchemaWorkLimits, Value};

fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}
fn seed(c: &romedb::Connection) {
    for sql in [
        "INSERT INTO users {id:users:a,name:'Alice'}",
        "INSERT INTO docs {id:docs:a,n:1,author:users:a,body:'hello',v:vector32('[1,0]'),point:geo::point(1,2)}",
        "INSERT INTO docs {id:docs:b,n:2,author:users:a,body:'world',v:vector32('[0,1]'),point:geo::point(2,3)}",
        "CREATE UNIQUE INDEX scalar ON docs(n)",
        "CREATE INDEX author ON docs(author)",
        "DEFINE RELATION posts ON users FROM docs.author",
        "CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT WITH(tokenizer='ngram',min_gram=2,max_gram=3)",
        "CREATE SEARCH INDEX vec ON docs(v) USING VECTOR WITH(dimensions=2,metric='cosine')",
        "CREATE SEARCH INDEX loc ON docs(point) USING SPATIAL",
    ] { q(c,sql); }
}
fn evidence(c: &romedb::Connection) -> Vec<Vec<Vec<Value>>> {
    [
        "INFO FOR TABLE docs",
        "INFO FOR RELATION posts",
        "SELECT id FROM docs WHERE n=1",
        "SELECT relation::fetch(users:a,'posts')",
        "SELECT id FROM search::text('words','hell',10)",
        "SELECT id FROM search::vector('vec',vector32('[1,0]'),2)",
        "SELECT id FROM search::near('loc',geo::point(1,2),1000)",
    ]
    .into_iter()
    .map(|sql| q(c, sql).rows)
    .collect()
}
#[test]
fn rebuild_preserves_definitions_dependencies_snapshots_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reindex.db");
    let before;
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        seed(&c);
        before = evidence(&c);
        let reader = db.connect().unwrap();
        q(&reader, "BEGIN");
        assert_eq!(evidence(&reader), before);
        q(&c, "BEGIN");
        for name in ["scalar", "author", "words", "vec", "loc"] {
            q(&c, &format!("REINDEX main.{name}"));
        }
        assert_eq!(evidence(&c), before);
        assert_eq!(evidence(&reader), before);
        q(&c, "ROLLBACK");
        assert_eq!(evidence(&c), before);
        for name in ["scalar", "author", "words", "vec", "loc"] {
            c.reindex(name).unwrap();
        }
        assert_eq!(evidence(&reader), before);
        q(&reader, "COMMIT");
        assert_eq!(evidence(&c), before);
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
        assert_eq!(c.reindex("missing").unwrap_err().code(), "FDB_NOT_FOUND");
        q(&c, "CREATE TABLE native(n INTEGER)");
        q(&c, "CREATE INDEX native_n ON native(n)");
        q(&c, "REINDEX native_n");
    }
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(evidence(&c), before);
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}
#[test]
fn rebuild_metering_and_source_buffer_limits_preserve_caller_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    seed(&c);
    let before = evidence(&c);
    let limits = ResultLimits {
        max_rows: 0,
        max_payload_bytes: 65536,
    };
    for name in ["scalar", "author", "words", "vec", "loc"] {
        let measured = c
            .schema_metered(
                &format!("REINDEX {name}"),
                &Parameters::new(),
                limits,
                SchemaWorkLimits::default(),
            )
            .unwrap();
        measured.outcome.unwrap();
        assert_eq!(
            measured.work.rows_read,
            if name == "vec" { 4 } else { 2 },
            "{name}"
        );
        assert_eq!(measured.work.row_mutations, 0, "{name}");
        assert!(measured.work.vm_steps > 0);
        q(&c, "BEGIN");
        q(&c, "INSERT INTO prior {id:prior:a}");
        let measured = c
            .schema_metered(
                &format!("REINDEX {name}"),
                &Parameters::new(),
                limits,
                SchemaWorkLimits {
                    max_rows_read: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(measured.outcome.unwrap_err().code(), "FDB_CANCELLED");
        q(&c, "COMMIT");
        assert_eq!(q(&c, "SELECT * FROM prior").rows.len(), 1);
        q(&c, "DELETE FROM prior");
        assert_eq!(evidence(&c), before);
    }
    let c = c.with_write_buffer_limits(ResultLimits {
        max_rows: 1,
        max_payload_bytes: 65536,
    });
    for name in ["scalar", "words", "vec", "loc"] {
        assert_eq!(c.reindex(name).unwrap_err().code(), "FDB_LIMIT");
    }
    assert_eq!(evidence(&c), before);
}
#[test]
fn committed_and_uncommitted_rebuilds_recover_after_process_exit() {
    const CHILD: &str = "ROMEDB_REINDEX_CRASH_PATH";
    if let Ok(path) = std::env::var(CHILD) {
        let c = Database::open(&path).unwrap().connect().unwrap();
        seed(&c);
        for name in ["scalar", "author", "words", "vec", "loc"] {
            q(&c, &format!("REINDEX {name}"));
        }
        q(&c, "BEGIN");
        q(&c, "UPDATE docs SET body='uncommitted',n=n+10");
        for name in ["scalar", "author", "words", "vec", "loc"] {
            q(&c, &format!("REINDEX {name}"));
        }
        std::process::exit(73);
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crash.db");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "committed_and_uncommitted_rebuilds_recover_after_process_exit",
        ])
        .env(CHILD, &path)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(73),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    assert_eq!(
        q(&c, "SELECT id FROM search::text('words','hell',10)")
            .rows
            .len(),
        1
    );
    assert!(
        q(&c, "SELECT id FROM search::text('words','uncommitted',10)")
            .rows
            .is_empty()
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}
