use romedb::{Database, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
#[test]
fn bundled_string_functions_work_in_sql_and_validated_object_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE posts");
    q(&c, "DEFINE FIELD slug ON posts TYPE string REQUIRED");
    q(&c, "CREATE UNIQUE INDEX slugs ON posts(slug)");
    q(
        &c,
        "INSERT INTO posts {id:posts:p1,slug:string::slugify(' Crème brûlée! ')}",
    );
    assert_eq!(
        q(&c, "SELECT slug FROM posts").rows,
        vec![vec![Value::String("creme-brulee".into())]]
    );
    q(
        &c,
        "UPDATE posts SET slug=string::slugify('Other Post') RETURNING slug",
    );
    let normalized = q(&c, "SELECT string::normalize('é','NFC') AS normalized");
    assert_eq!(normalized.rows, vec![vec![Value::String("é".into())]]);
    assert!(c
        .execute(
            "INSERT INTO posts {slug:string::slugify('Other Post')}",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(
        c.lookup_index("posts", "slugs", &Value::String("other-post".into()))
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn bundled_arguments_remain_data_and_invalid_arguments_are_rejected() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let text = "'); throw new Error('executed'); //";
    let params = Parameters::from([("$input".into(), Value::String(text.into()))]);
    assert_eq!(
        c.execute("SELECT string::normalize($input,'NFC') AS value", &params)
            .unwrap()
            .rows,
        vec![vec![Value::String(text.into())]]
    );
    for sql in [
        "SELECT string::normalize('x','invalid')",
        "SELECT string::slugify(1)",
        "SELECT string::eval('1+1')",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err());
    }
    q(&c, "CREATE TABLE posts");
    let params = Parameters::from([("$input".into(), Value::String("x".repeat(65_537)))]);
    assert_eq!(
        c.execute("INSERT INTO posts {slug:string::slugify($input)}", &params)
            .unwrap_err()
            .code(),
        "FDB_LIMIT"
    );
    assert!(q(&c, "SELECT * FROM posts").rows.is_empty());
}

#[test]
fn bundled_output_overflow_matches_native_transaction_disposition() {
    for logical in [false, true] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        q(
            &c,
            if logical {
                "CREATE TABLE posts"
            } else {
                "CREATE TABLE posts(id TEXT PRIMARY KEY,slug TEXT,input TEXT)"
            },
        );
        q(&c, "CREATE UNIQUE INDEX slugs ON posts(slug)");
        for (id, slug, input) in [
            ("a", "first", "safe".to_owned()),
            ("b", "second", "ﷺ".repeat(6000)),
        ] {
            let id = if logical {
                Value::Record(romedb::Record {
                    table: "posts".into(),
                    key: romedb::Key::String(id.into()),
                })
            } else {
                Value::String(id.into())
            };
            c.execute(
                "INSERT INTO posts(id,slug,input) VALUES($id,$slug,$input)",
                &Parameters::from([
                    ("$id".into(), id),
                    ("$slug".into(), Value::String(slug.into())),
                    ("$input".into(), Value::String(input)),
                ]),
            )
            .unwrap();
        }
        q(&c, "CREATE TABLE prior(n)");
        q(&c, "BEGIN");
        q(&c, "INSERT INTO prior VALUES(9)");
        let before = q(&c, "SELECT id,slug FROM posts ORDER BY id").rows;
        let operation = if logical {
            "UPDATE posts SET slug=string::normalize(input,'NFKD') RETURNING slug"
        } else {
            "SELECT string::normalize(input,'NFKD') FROM posts"
        };
        let report = c.execute_report(operation, &Parameters::new());
        let error = report.result.unwrap_err();
        assert!(
            error.to_string().contains("bundled string output exceeds"),
            "{error}"
        );
        assert_eq!(report.transaction_before, romedb::TransactionState::Active);
        assert_eq!(
            report.transaction_after,
            romedb::TransactionState::Active,
            "logical={logical}"
        );
        assert_eq!(q(&c, "SELECT id,slug FROM posts ORDER BY id").rows, before);
        assert_eq!(
            q(&c, "SELECT n FROM prior").rows,
            vec![vec![Value::Integer(9)]]
        );
        q(&c, "ROLLBACK");
        assert!(q(&c, "SELECT n FROM prior").rows.is_empty());
        if logical {
            c.check_collection_integrity("posts", Default::default())
                .unwrap();
        }
        q(&c, "BEGIN");
        q(
            &c,
            "UPDATE posts SET input='second safe' WHERE slug='second'",
        );
        let retried = q(&c, operation);
        assert_eq!(retried.rows.len(), 2);
        if logical {
            assert_eq!(retried.affected, 2);
        }
        if logical {
            c.check_collection_integrity("posts", Default::default())
                .unwrap();
        }
        q(&c, "ROLLBACK");
        assert_eq!(q(&c, "SELECT id,slug FROM posts ORDER BY id").rows, before);
        if logical {
            c.check_collection_integrity("posts", Default::default())
                .unwrap();
        }
    }
}
#[test]
fn romedb_version_reports_the_release_and_cannot_enter_stored_definitions() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        q(&c, "SELECT romedb_version() AS version").rows,
        vec![vec![Value::String(romedb::VERSION.into())]]
    );
    assert_eq!(romedb::VERSION, env!("CARGO_PKG_VERSION"));
    q(&c, "CREATE TABLE posts");
    q(&c, "CREATE TABLE notes(body TEXT)");
    for sql in [
        "SELECT romedb_version(1)",
        "DEFINE FIELD build ON posts TYPE string CHECK (build = romedb_version())",
        "CREATE INDEX by_version ON notes(romedb_version())",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    q(&c, "CREATE INDEX by_length ON notes(length(body))");
}
