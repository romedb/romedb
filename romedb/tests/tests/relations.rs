use romedb::{Database, Parameters, ResultLimits, Value};

fn record(table: &str, key: &str) -> romedb::Record {
    romedb::Record {
        table: table.into(),
        key: romedb::Key::String(key.into()),
    }
}
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}
fn setup(c: &romedb::Connection) {
    for sql in [
        "INSERT INTO users {id:users:u1,name:'Alice'}",
        "INSERT INTO users {id:users:u2,name:'Bob'}",
        "INSERT INTO posts {id:posts:a,author:users:u1,title:'A',link:users:u2}",
        "INSERT INTO posts {id:posts:b,author:users:u1,title:'B'}",
        "INSERT INTO posts {id:posts:c,author:users:u2,title:'C'}",
        "CREATE INDEX posts_author ON posts(author)",
        "DEFINE RELATION authored ON users FROM posts.author",
    ] {
        q(c, sql);
    }
}
fn ids(value: &Value) -> Vec<Value> {
    let Value::Array(docs) = value else {
        panic!("{value:?}");
    };
    docs.iter()
        .map(|v| {
            let Value::Object(doc) = v else { panic!() };
            doc["id"].clone()
        })
        .collect()
}
fn fetch(c: &romedb::Connection, expr: &str) -> Value {
    q(c, &format!("SELECT relation::fetch({expr}) AS posts")).rows[0][0].clone()
}

#[test]
fn inverse_declarations_enforce_dependencies_and_persist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("relations.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE users");
        q(&c, "CREATE TABLE posts");
        assert!(c
            .execute(
                "DEFINE RELATION authored ON users FROM posts.author",
                &Parameters::new()
            )
            .is_err());
        q(&c, "CREATE INDEX posts_author ON posts(author)");
        q(&c, "BEGIN");
        q(&c, "DEFINE RELATION authored ON users FROM posts.author");
        q(&c, "ROLLBACK");
        assert!(c
            .execute("INFO FOR RELATION authored", &Parameters::new())
            .is_err());
        q(&c, "DEFINE RELATION authored ON users FROM posts.author");
        q(&c, "INSERT INTO posts {id:posts:a,author:users:u1}");
        for sql in [
            "DROP INDEX posts_author",
            "DROP TABLE posts",
            "DEFINE RELATION authored ON posts FROM posts.author",
        ] {
            assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
        }
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        ids(&fetch(&c, "users:u1,'authored'")),
        vec![Value::Record(record("posts", "a"))]
    );
    let Value::Object(info) = &q(&c, "INFO FOR RELATION authored").rows[0][0] else {
        panic!()
    };
    assert_eq!(info["index"], Value::String("posts_author".into()));
    q(&c, "BEGIN");
    q(&c, "DROP RELATION authored");
    q(&c, "DROP INDEX posts_author");
    q(&c, "ROLLBACK");
    assert_eq!(ids(&fetch(&c, "users:u1,'authored'")).len(), 1);
    q(&c, "DROP TABLE users"); // Owned declarations disappear; weak references remain.
    q(&c, "DROP INDEX posts_author");
    assert_eq!(q(&c, "SELECT posts:a").rows.len(), 1);
    q(&c, "DROP RELATION IF EXISTS authored");
    assert!(c
        .execute("DROP RELATION authored", &Parameters::new())
        .is_err());
}

#[test]
fn inverse_pages_preserve_typed_identity_and_pending_updates() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    setup(&c);
    let all = fetch(&c, "users:u1,'authored'");
    assert_eq!(
        ids(&all),
        vec![
            Value::Record(record("posts", "a")),
            Value::Record(record("posts", "b"))
        ]
    );
    let Value::Array(docs) = &all else { panic!() };
    let Value::Object(doc) = &docs[0] else {
        panic!()
    };
    assert!(matches!(doc["link"], Value::Record(_)));
    assert_eq!(ids(&fetch(&c, "users:u1,'authored',1")), ids(&all)[..1]);
    assert_eq!(
        ids(&fetch(&c, "users:u1,'authored',1,posts:a")),
        ids(&all)[1..]
    );
    assert_eq!(fetch(&c, "users:u1,'authored',0"), Value::Array(vec![]));
    assert_eq!(fetch(&c, "NULL,'authored'"), Value::Null);
    assert_eq!(fetch(&c, "users:absent,'authored'"), Value::Array(vec![]));
    let params = Parameters::from([
        ("$who".into(), Value::Record(record("users", "u1"))),
        ("$name".into(), Value::String("authored".into())),
        ("$limit".into(), Value::Integer(1)),
        ("$after".into(), Value::Record(record("posts", "a"))),
    ]);
    let bounded = c
        .execute("SELECT relation::fetch($who,$name,$limit,$after)", &params)
        .unwrap();
    assert_eq!(ids(&bounded.rows[0][0]), ids(&all)[1..]);
    q(&c, "INSERT INTO posts {id:posts:1,author:users:u1}");
    q(
        &c,
        "INSERT INTO posts {id:type::record('posts','1'),author:users:u2}",
    );
    assert_eq!(ids(&fetch(&c, "users:u1,'authored'")).len(), 3);
    q(&c, "BEGIN");
    q(&c, "UPDATE posts:a {author:users:u2}");
    q(&c, "DELETE FROM posts:b");
    assert_eq!(ids(&fetch(&c, "users:u1,'authored'")).len(), 1);
    assert!(c
        .execute("UPDATE posts SET author=array::new(1)", &Parameters::new())
        .is_err());
    c.check_collection_integrity("posts", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert_eq!(ids(&fetch(&c, "users:u1,'authored'")).len(), 3);
    // Source-free, ordinary relational and collection outer queries share the same resolver.
    q(&c, "CREATE TABLE native(n INTEGER)");
    q(&c, "INSERT INTO native VALUES(1)");
    assert_eq!(
        q(
            &c,
            "SELECT relation::fetch(users:u1,'authored') FROM native"
        )
        .rows[0][0],
        fetch(&c, "users:u1,'authored'")
    );
    let mixed=q(&c,"SELECT u.id,relation::fetch(u.id,'authored') AS posts,record::fetch(u.id) AS user FROM users u ORDER BY u.id");
    assert_eq!(mixed.rows.len(), 2);
    assert!(matches!(mixed.rows[0][2], Value::Object(_)));
}

#[test]
fn inverse_projection_restrictions_limits_and_deduplication() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    setup(&c);
    let single = c
        .profile_select(
            "SELECT relation::fetch(users:u1,'authored')",
            &Parameters::new(),
        )
        .unwrap();
    let sql="SELECT relation::fetch(users:u1,'authored') AS a,relation::fetch(users:u1,'authored') AS a";
    let doubled = c.profile_select(sql, &Parameters::new()).unwrap();
    assert_eq!(doubled.result.columns, ["a", "a"]);
    assert_eq!(doubled.result.rows[0][0], doubled.result.rows[0][1]);
    assert_eq!(doubled.metrics.fetch_batches, 1);
    assert_eq!(
        single.metrics.fetch_rows_read,
        doubled.metrics.fetch_rows_read
    );
    for limits in [
        ResultLimits {
            max_rows: 0,
            max_payload_bytes: usize::MAX,
        },
        ResultLimits {
            max_rows: 1,
            max_payload_bytes: 20,
        },
    ] {
        assert_eq!(
            c.select_with_limits(sql, &Parameters::new(), limits)
                .unwrap_err()
                .code(),
            "FDB_LIMIT"
        );
    }
    for sql in [
        "SELECT relation::fetch(users:u1,'unknown') WHERE 0",
        "SELECT relation::fetch(posts:a,'authored')",
        "SELECT relation::fetch(1,'authored')",
        "SELECT relation::fetch(users:u1,'authored',1001)",
        "SELECT relation::fetch(users:u1,'authored',1.5)",
        "SELECT relation::fetch(users:u1,'authored',-1)",
        "SELECT relation::fetch(users:u1,'authored',1,users:u1)",
        "SELECT relation::fetch(users:u1,'authored',1,'a')",
        "SELECT relation::fetch(users:u1,name) FROM users",
        "SELECT doc::get(relation::fetch(users:u1,'authored'),'id')",
        "SELECT DISTINCT relation::fetch(users:u1,'authored')",
        "SELECT relation::fetch(id,'authored') AS p FROM users ORDER BY p",
        "SELECT * FROM (SELECT relation::fetch(id,'authored') FROM users)",
        "WITH x AS (SELECT relation::fetch(id,'authored') FROM users) SELECT * FROM x",
        "SELECT relation::fetch(users:u1,'authored') UNION ALL SELECT NULL",
        "UPDATE posts {title:'Rejected'} RETURNING relation::fetch(author,'authored')",
        "INSERT INTO posts(x) SELECT relation::fetch(id,'authored') FROM users",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert_eq!(
        q(&c, "SELECT title FROM posts WHERE id=posts:a").rows[0][0],
        Value::String("A".into())
    );
}

#[test]
fn inverse_selective_lookup_uses_reference_index() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    setup(&c);
    q(&c, "BEGIN");
    for i in 0..300 {
        q(
            &c,
            &format!("INSERT INTO posts {{id:posts:p{i},author:users:u2}}"),
        );
    }
    q(&c, "COMMIT");
    let profile = c
        .profile_select(
            "SELECT relation::fetch(id,'authored',1) FROM users WHERE id=users:u1",
            &Parameters::new(),
        )
        .unwrap();
    assert_eq!(profile.metrics.fullscan_steps, 0);
    assert!(
        profile.metrics.fetch_rows_read < 20,
        "{:?}",
        profile.metrics
    );
    assert_eq!(ids(&profile.result.rows[0][0]).len(), 1);
    eprintln!("inverse selective metrics: {:?}", profile.metrics);
}
