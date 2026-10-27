use romedb::{Database, Parameters, ResultLimits, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
fn setup(c: &romedb::Connection) {
    q(
        c,
        "INSERT INTO users {id:users:u1,name:'Alice',manager:users:u2}",
    );
    q(c, "INSERT INTO users {id:users:u2,name:'Bob'}");
    q(c,"INSERT INTO posts {id:posts:p1,title:'Hello',author:users:u1,profile:{city:'Bangkok'},missing_author:users:none,tags:[1,2]}");
}

#[test]
fn omit_returns_typed_documents_without_changing_sources() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    setup(&c);
    q(&c, "UPDATE posts:p1 {embedding:[1,2],internal_notes:'private',profile:{city:'Bangkok',private:true},\"odd.key\":7}");
    let kept = Value::Object(std::collections::BTreeMap::from([
        ("integer".into(), Value::Integer(i64::MAX)),
        ("binary".into(), Value::Binary(vec![0, 255])),
        ("vector".into(), Value::vector32(&[1.0, 2.0]).unwrap()),
        ("null".into(), Value::Null),
        ("boolean".into(), Value::Boolean(true)),
    ]));
    c.execute(
        "UPDATE posts:p1 {kept:$kept}",
        &Parameters::from([("$kept".into(), kept.clone())]),
    )
    .unwrap();
    let original = q(&c, "SELECT * FROM posts");
    let Value::Object(mut expected) = original.rows[0][0].clone() else {
        panic!()
    };
    expected.remove("embedding");
    expected.remove("internal_notes");
    expected.remove("odd.key");
    let Value::Object(profile) = expected.get_mut("profile").unwrap() else {
        panic!()
    };
    profile.remove("private");
    let sql = "SELECT * OMIT embedding,internal_notes,profile.private,\"odd.key\",missing,author.name,tags.name FROM posts p WHERE p.title=$title ORDER BY p.id LIMIT 1 OFFSET 0";
    let params = Parameters::from([("$title".into(), Value::String("Hello".into()))]);
    let actual = c.execute(sql, &params).unwrap();
    assert_eq!(actual.columns, vec!["document"]);
    assert_eq!(actual.rows, vec![vec![Value::Object(expected)]]);
    assert_eq!(
        c.profile_select(sql, &params).unwrap().result.rows,
        actual.rows
    );
    assert_eq!(q(&c, "SELECT * FROM posts").rows, original.rows);
    let without_profile = q(&c, "SELECT * OMIT profile,profile.city,profile FROM posts");
    let Value::Object(doc) = &without_profile.rows[0][0] else {
        panic!()
    };
    assert!(!doc.contains_key("profile"));
    assert!(matches!(&doc["author"], Value::Record(_)));
    assert_eq!(doc["kept"], kept);
    assert_eq!(
        doc["tags"],
        Value::Array(vec![Value::Integer(1), Value::Integer(2)])
    );
    assert!(q(&c, "SELECT * OMIT id FROM posts WHERE false")
        .rows
        .is_empty());
    assert_eq!(
        c.select_with_limits(
            sql,
            &params,
            ResultLimits {
                max_rows: 1,
                max_payload_bytes: 1
            }
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    assert_eq!(q(&c, "SELECT * FROM posts").rows, original.rows);
}

#[test]
fn omit_rejects_ambiguous_forms_and_preserves_sql_names() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    setup(&c);
    q(&c, "CREATE TABLE native(omit INTEGER)");
    q(&c, "INSERT INTO native VALUES(7)");
    assert_eq!(
        q(&c, "SELECT omit FROM native").rows,
        vec![vec![Value::Integer(7)]]
    );
    assert_eq!(
        q(&c, "SELECT 1 AS omit").rows,
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(
        q(&c, "SELECT '*' omit FROM native").rows,
        vec![vec![Value::String("*".into())]]
    );
    for sql in [
        "SELECT * OMIT FROM posts",
        "SELECT * OMIT title, FROM posts",
        "SELECT title OMIT profile FROM posts",
        "SELECT p.* OMIT title FROM posts p",
        "SELECT *,title OMIT profile FROM posts",
        "SELECT * OMIT title FROM posts p JOIN users u ON p.author=u.id",
        "SELECT * OMIT omit FROM native",
        "SELECT * OMIT tags[0] FROM posts",
        "SELECT * OMIT profile.* FROM posts",
        "SELECT * OMIT $field FROM posts",
        "UPDATE posts:p1 {title:'bad'} RETURNING * OMIT profile",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    let depth = format!("SELECT * OMIT {} FROM posts", vec!["a"; 65].join("."));
    assert_eq!(
        c.execute(&depth, &Parameters::new()).unwrap_err().code(),
        "FDB_LIMIT"
    );
    let width = format!("SELECT * OMIT {} FROM posts", vec!["a"; 1025].join(","));
    assert_eq!(
        c.execute(&width, &Parameters::new()).unwrap_err().code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        q(&c, "SELECT title FROM posts").rows,
        vec![vec![Value::String("Hello".into())]]
    );
}

#[test]
fn omit_preserves_index_use_snapshot_and_pending_work() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("omit.db");
    let sql = "SELECT * OMIT secret FROM docs WHERE group_no=$group ORDER BY id LIMIT 3 OFFSET 1";
    let params = Parameters::from([("$group".into(), Value::Integer(1))]);
    {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let a = db.connect().unwrap();
        let b = db.connect().unwrap();
        q(&a, "BEGIN");
        for n in 0..100 {
            q(&a,&format!("INSERT INTO docs {{id:type::record('docs',{n}),group_no:{},secret:'private',name:'Old'}}",n%10));
        }
        q(&a, "COMMIT");
        let scan = a.profile_select(sql, &params).unwrap();
        q(&a, "CREATE INDEX docs_group ON docs(group_no)");
        let indexed = a.profile_select(sql, &params).unwrap();
        assert_eq!(indexed.result.rows, scan.result.rows);
        assert_eq!(indexed.result.rows.len(), 3);
        assert!(indexed.metrics.rows_read < scan.metrics.rows_read);
        assert!(indexed.metrics.fullscan_steps < scan.metrics.fullscan_steps);
        q(&a, "BEGIN");
        assert_eq!(a.execute(sql, &params).unwrap().rows, indexed.result.rows);
        q(&b, "UPDATE docs SET name='New'");
        assert_eq!(a.execute(sql, &params).unwrap().rows, indexed.result.rows);
        q(&a, "COMMIT");
        assert_ne!(a.execute(sql, &params).unwrap().rows, indexed.result.rows);
        q(&a, "BEGIN");
        q(&a, "UPDATE docs SET secret='Pending'");
        assert_eq!(
            a.select_with_limits(
                sql,
                &params,
                ResultLimits {
                    max_rows: 0,
                    max_payload_bytes: 1000
                }
            )
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        let token = romedb::CancellationToken::new();
        token.cancel();
        assert_eq!(
            a.execute_cancellable(sql, &params, &token)
                .unwrap_err()
                .code(),
            "FDB_CANCELLED"
        );
        assert_eq!(a.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(
            q(&a, "SELECT secret FROM docs LIMIT 1").rows,
            vec![vec![Value::String("Pending".into())]]
        );
        q(&a, "COMMIT");
        a.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
    let db = Database::open(file.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    let rows = c.execute(sql, &params).unwrap().rows;
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| matches!(&row[0],Value::Object(doc) if !doc.contains_key("secret") && doc["name"]==Value::String("New".into()))));
    assert_eq!(
        q(&c, "SELECT secret FROM docs LIMIT 1").rows,
        vec![vec![Value::String("Pending".into())]]
    );
}
#[test]
fn ordinary_reference_wildcard_aliases_match_fetch() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "INSERT INTO writers {id:writers:w1,name:'Alice'}");
    q(
        &c,
        "INSERT INTO articles {id:articles:a1,title:'Hello',author:writers:w1}",
    );
    q(&c, "CREATE INDEX articles_author ON articles(author)");
    let params = Parameters::from([("$writer".into(), Value::String("w1".into()))]);
    for (sql, equivalent) in [
        (
            "SELECT id, title, author.* AS writer FROM articles WHERE author = type::record('writers', $writer) ORDER BY id",
            "SELECT id, title, record::fetch(author) AS writer FROM articles WHERE author = type::record('writers', $writer) ORDER BY id",
        ),
        (
            "SELECT a.id, a.title, a.author.* AS writer FROM articles AS a WHERE a.author = type::record('writers', $writer) ORDER BY a.id LIMIT 20",
            "SELECT a.id, a.title, record::fetch(a.author) AS writer FROM articles AS a WHERE a.author = type::record('writers', $writer) ORDER BY a.id LIMIT 20",
        ),
    ] {
        let expected = c.execute(equivalent, &params).unwrap();
        let actual = c.execute(sql, &params).unwrap();
        assert_eq!(actual.columns, expected.columns);
        assert_eq!(actual.rows, expected.rows);
        let profile = c.profile_select(sql, &params).unwrap();
        assert_eq!(profile.result.rows, expected.rows);
        assert_eq!(profile.metrics.fetch_batches, 1);
    }
}

#[test]
fn ordinary_wildcard_aliases_preserve_values_and_source_precedence() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    setup(&c);
    q(&c, "UPDATE posts:p1 {authors:[users:u1,users:missing,null,7,{name:'Embedded'}],nothing:null,\"odd.key\":{flag:true}}");
    for (path, expected) in [
        ("author", "record::fetch(author)"),
        ("profile", "profile"),
        ("missing_author", "NULL"),
        ("missing", "NULL"),
        ("\"nothing\"", "NULL"),
        ("title", "NULL"),
        ("authors", "p.authors.*"),
        ("\"odd.key\"", "p.\"odd.key\".*"),
        ("[odd.key]", "p.\"odd.key\".*"),
        ("`odd.key`", "p.\"odd.key\".*"),
    ] {
        let actual = q(&c, &format!("SELECT {path} . /* gap */ * AS \"same name\",{path}.* AS \"same name\" FROM posts p"));
        let expected = q(&c, &format!("SELECT {expected} FROM posts p")).rows[0][0].clone();
        assert_eq!(actual.columns, vec!["same name", "same name"]);
        assert_eq!(
            actual.rows,
            vec![vec![expected.clone(), expected]],
            "{path}"
        );
    }
    assert_eq!(
        q(&c, "SELECT author.* FROM posts author").rows,
        q(&c, "SELECT * FROM posts").rows
    );
    assert_eq!(
        q(&c, "SELECT author.author.* AS writer FROM posts author").rows,
        q(&c, "SELECT record::fetch(author) FROM posts").rows
    );
    q(&c, "CREATE TABLE native(n INTEGER)");
    q(&c, "INSERT INTO native VALUES(9)");
    assert_eq!(
        q(&c, "SELECT author.* FROM native author").rows,
        vec![vec![Value::Integer(9)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT a.author.* AS writer FROM posts a JOIN users b ON a.author=b.id"
        )
        .rows,
        q(&c, "SELECT record::fetch(author) FROM posts").rows
    );
    for sql in [
        "SELECT author.* AS writer FROM posts author",
        "SELECT author.* AS writer FROM native author",
        "SELECT author.* AS writer FROM posts a JOIN users b ON a.author=b.id",
        "SELECT DISTINCT author.* AS writer FROM posts",
        "SELECT coalesce(author.* AS writer,NULL) FROM posts",
        "SELECT author.* AS writer FROM posts ORDER BY writer",
        "SELECT author.* AS writer FROM posts WHERE writer IS NOT NULL",
        "SELECT author.* AS writer FROM posts GROUP BY writer",
        "UPDATE posts SET title='bad' RETURNING author.* AS writer",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert_eq!(
        q(&c, "SELECT title FROM posts").rows,
        vec![vec![Value::String("Hello".into())]]
    );
}

#[test]
fn ordinary_wildcard_aliases_keep_index_selection_and_batched_fetches() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "INSERT INTO writers {id:writers:w1,name:'Alice'}");
    q(&c, "BEGIN");
    for n in 0..100 {
        q(&c, &format!("INSERT INTO articles {{id:type::record('articles',{n}),title:'Article',author:writers:{}}}", if n % 10 == 0 { "w1" } else { "other" }));
    }
    q(&c, "COMMIT");
    let sql = "SELECT id,author.* AS writer,author.* AS writer FROM articles WHERE author=type::record('writers',$key) ORDER BY id LIMIT 3 OFFSET 1";
    let params = Parameters::from([("$key".into(), Value::String("w1".into()))]);
    let scan = c.profile_select(sql, &params).unwrap();
    q(&c, "CREATE INDEX article_author ON articles(author)");
    let indexed = c.profile_select(sql, &params).unwrap();
    assert_eq!(indexed.result.rows, scan.result.rows);
    assert_eq!(indexed.result.rows.len(), 3);
    assert_eq!(indexed.result.columns, vec!["id", "writer", "writer"]);
    assert!(indexed.metrics.fullscan_steps < scan.metrics.fullscan_steps);
    assert!(indexed.metrics.rows_read < scan.metrics.rows_read);
    assert_eq!(indexed.metrics.fetch_batches, 1);
    let single = c.profile_select("SELECT author.* AS writer FROM articles WHERE author=type::record('writers',$key) LIMIT 1", &params).unwrap();
    assert_eq!(
        indexed.metrics.fetch_rows_read,
        single.metrics.fetch_rows_read
    );
    for row in &indexed.result.rows {
        assert_eq!(row[1], row[2]);
    }
}

#[test]
fn ordinary_wildcard_aliases_preserve_snapshot_limits_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wildcard.db");
    let sql = "SELECT author.* AS writer FROM posts";
    let params = Parameters::new();
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let a = db.connect().unwrap();
        let b = db.connect().unwrap();
        setup(&a);
        q(&a, "BEGIN");
        let old = q(&a, sql).rows;
        q(&b, "UPDATE users:u1 {name:'New'}");
        assert_eq!(q(&a, sql).rows, old);
        assert_eq!(a.profile_select(sql, &params).unwrap().result.rows, old);
        q(&a, "COMMIT");
        assert_ne!(q(&a, sql).rows, old);
        q(&a, "BEGIN");
        q(&a, "UPDATE users:u1 {name:'Pending'}");
        let pending = q(&a, "SELECT record::fetch(author) FROM posts").rows;
        assert_eq!(q(&a, sql).rows, pending);
        assert_eq!(
            a.select_with_limits(
                sql,
                &params,
                ResultLimits {
                    max_rows: 1,
                    max_payload_bytes: 1
                }
            )
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        let token = romedb::CancellationToken::new();
        token.cancel();
        assert_eq!(
            a.execute_cancellable(sql, &params, &token)
                .unwrap_err()
                .code(),
            "FDB_CANCELLED"
        );
        assert_eq!(a.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(q(&a, sql).rows, pending);
        q(&a, "COMMIT");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        q(&c, sql).rows,
        q(&c, "SELECT record::fetch(author) FROM posts").rows
    );
    assert!(
        matches!(&q(&c, sql).rows[0][0], Value::Object(doc) if doc["name"] == Value::String("Pending".into()))
    );
}
#[test]
fn brace_fields_preserve_typed_positions_and_one_hop_objects() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    setup(&c);
    let result=q(&c,"SELECT posts:p1 {title,author.*,profile.*,profile.city AS city,missing_author.*,unknown,author AS author}");
    assert_eq!(
        result.columns,
        vec![
            "title",
            "author",
            "profile",
            "city",
            "missing_author",
            "unknown",
            "author"
        ]
    );
    let author = q(&c, "SELECT users:u1").rows[0][0].clone();
    let fields = q(
        &c,
        "SELECT p.title,p.profile,p.profile.city,p.author FROM posts p WHERE p.id=posts:p1",
    )
    .rows
    .remove(0);
    assert_eq!(
        result.rows,
        vec![vec![
            fields[0].clone(),
            author,
            fields[1].clone(),
            fields[2].clone(),
            Value::Null,
            Value::Null,
            fields[3].clone()
        ]]
    );
    // Expansion does not follow the manager link a second time.
    let Value::Object(author) = &result.rows[0][1] else {
        panic!()
    };
    assert!(matches!(author.get("manager"), Some(Value::Record(_))));
    let all = q(&c, "SELECT posts:p1 {*}");
    assert_eq!(all.columns, vec!["document"]);
    assert_eq!(all.rows, q(&c, "SELECT posts:p1").rows);
    let missing = q(&c, "SELECT posts:no_such_record {title,author.* AS writer}");
    assert_eq!(missing.columns, vec!["title", "writer"]);
    assert!(missing.rows.is_empty());
    assert_eq!(
        q(&c, "SELECT posts:p1 { title, }").rows,
        vec![vec![Value::String("Hello".into())]]
    );
    assert_eq!(
        q(&c, "SELECT posts:p1 { title AS same,author AS same }").columns,
        vec!["same", "same"]
    );
}
#[test]
fn brace_fetches_share_snapshots_limits_and_indexed_lookup() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    setup(&c);
    let sql = "SELECT posts:p1 {title,author.* AS first,author.* AS again}";
    let profile = c.profile_select(sql, &Parameters::new()).unwrap();
    assert_eq!(profile.result.rows[0][1], profile.result.rows[0][2]);
    assert_eq!(profile.metrics.fetch_batches, 1);
    let single = c
        .profile_select("SELECT posts:p1 {author.*}", &Parameters::new())
        .unwrap();
    assert_eq!(
        profile.metrics.fetch_rows_read,
        single.metrics.fetch_rows_read
    );
    assert_eq!(profile.metrics.fullscan_steps, 0);
    assert!(profile.metrics.rows_read <= 2);
    for limits in [
        ResultLimits {
            max_rows: 0,
            max_payload_bytes: usize::MAX,
        },
        ResultLimits {
            max_rows: 1,
            max_payload_bytes: 30,
        },
    ] {
        assert_eq!(
            c.select_with_limits(sql, &Parameters::new(), limits)
                .unwrap_err()
                .code(),
            "FDB_LIMIT"
        );
    }
    // Exact final payload limit includes each fetched occurrence and column label.
    let single = "SELECT posts:p1 {author.* AS a}";
    let Value::Object(author) = &profile.result.rows[0][1] else {
        panic!()
    };
    // id (users/u1), name Alice, manager(users/u2) plus object keys and label a.
    let bytes = 1
        + "id".len()
        + "users".len()
        + "u1".len()
        + "name".len()
        + "Alice".len()
        + "manager".len()
        + "users".len()
        + "u2".len();
    assert!(author.contains_key("id"));
    assert!(c
        .select_with_limits(
            single,
            &Parameters::new(),
            ResultLimits {
                max_rows: 1,
                max_payload_bytes: bytes
            }
        )
        .is_ok());
    assert_eq!(
        c.select_with_limits(
            single,
            &Parameters::new(),
            ResultLimits {
                max_rows: 1,
                max_payload_bytes: bytes - 1
            }
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        c.select_with_limits(
            "SELECT posts:p1 {missing_author.* AS a}",
            &Parameters::new(),
            ResultLimits {
                max_rows: 1,
                max_payload_bytes: 2
            }
        )
        .unwrap()
        .rows,
        vec![vec![Value::Null]]
    );
    q(&c, "BEGIN");
    q(&c, "UPDATE users:u1 {name:'Pending'}");
    let pending = q(&c, "SELECT posts:p1 {author.*}");
    assert!(format!("{:?}", pending.rows).contains("Pending"));
    assert!(c
        .execute("SELECT posts:p1 {title.*}", &Parameters::new())
        .is_err());
    q(&c, "ROLLBACK");
    assert!(format!("{:?}", q(&c, "SELECT posts:p1 {author.*}").rows).contains("Alice"));
}
#[test]
fn brace_quoted_paths_native_links_and_invalid_forms() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("braces.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE native(id TEXT PRIMARY KEY,name TEXT)");
        q(&c, "INSERT INTO native VALUES('n','Native')");
        q(
            &c,
            r#"INSERT INTO `odd table` {id:`odd table`:1, "odd.field":{link:native:n}, count:7}"#,
        );
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    let result = q(
        &c,
        "SELECT `odd table`:1 { `odd.field`.link.* AS related, count AS `odd label` }",
    );
    assert_eq!(result.columns, vec!["related", "odd label"]);
    let Value::Object(native) = &result.rows[0][0] else {
        panic!()
    };
    assert_eq!(native.get("name"), Some(&Value::String("Native".into())));
    assert_eq!(result.rows[0][1], Value::Integer(7));
    for sql in [
        "SELECT `odd table`:1 {}",
        "SELECT `odd table`:1 {count.*.name}",
        "SELECT `odd table`:1 {count {name}}",
        "SELECT `odd table`:1 {count+1}",
        "SELECT `odd table`:1 {count} LIMIT 1",
        "SELECT `odd table`:1 {count,count.*}",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert!(c
        .execute(
            "SELECT `odd table`:1 {count}",
            &Parameters::from([("$unused".into(), Value::Integer(1))])
        )
        .is_err());
    // Ordinary record expressions and SQL meanings remain intact.
    assert!(matches!(
        q(&c, "SELECT `odd table`:1 AS ref").rows[0][0],
        Value::Record(_)
    ));
}

#[test]
fn nested_object_and_array_dot_projections() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "INSERT INTO users {id:users:a,post:{title:'one'},posts:[{title:'first',meta:{n:1}},{title:'last',meta:{n:2}}],mixed:[1,null,{x:true},[]],nested:[[{x:1}],[{x:2}]],empty:[],scalar:4}");
    let plain = q(&c, "SELECT post,posts FROM users");
    assert_eq!(
        q(&c, "SELECT post.* FROM users").rows,
        vec![vec![plain.rows[0][0].clone()]]
    );
    let Value::Array(posts) = &plain.rows[0][1] else {
        panic!()
    };
    let all = q(
        &c,
        "SELECT posts.*.*,posts.0.*,posts.-1.*,posts.-2.* FROM users",
    );
    assert_eq!(all.columns, vec!["posts", "posts", "posts", "posts"]);
    assert_eq!(
        all.rows,
        vec![vec![
            plain.rows[0][1].clone(),
            posts[0].clone(),
            posts[1].clone(),
            posts[0].clone()
        ]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT u.posts.*.title AS titles,u.posts.1.meta.* AS meta FROM users u"
        )
        .rows,
        vec![vec![
            Value::Array(vec![
                Value::String("first".into()),
                Value::String("last".into())
            ]),
            Value::Object(std::collections::BTreeMap::from([(
                "n".into(),
                Value::Integer(2)
            )]))
        ]]
    );
    assert_eq!(
        q(&c, "SELECT nested.0.0.*,nested.*.*.* FROM users").rows[0][1],
        q(&c, "SELECT nested FROM users").rows[0][0]
    );
    assert_eq!(q(&c, "SELECT posts.99.*,posts.-99.*,posts.-9223372036854775808.*,empty.-1.*,missing.*,scalar.* FROM users").rows,vec![vec![Value::Null;6]]);
    let mixed = q(&c, "SELECT mixed.*.*,empty.*.* FROM users");
    assert_eq!(
        mixed.rows[0][0],
        Value::Array(vec![
            Value::Null,
            Value::Null,
            Value::Object(std::collections::BTreeMap::from([(
                "x".into(),
                Value::Boolean(true)
            )])),
            Value::Array(vec![])
        ])
    );
    assert_eq!(mixed.rows[0][1], Value::Array(vec![]));
    assert!(c
        .execute(
            "SELECT posts.9223372036854775808.* FROM users",
            &Parameters::new()
        )
        .is_err());
    let depth = format!("SELECT posts{} FROM users", ".*".repeat(65));
    assert_eq!(
        c.execute(&depth, &Parameters::new()).unwrap_err().code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        q(
            &c,
            "SELECT 'posts.0.*' AS literal,1.25 AS decimal FROM users"
        )
        .rows,
        vec![vec![Value::String("posts.0.*".into()), Value::Number(1.25)]]
    );
    // Aliases still select their entire source, including a same-named field.
    assert_eq!(
        q(&c, "SELECT post.* FROM users post").rows,
        q(&c, "SELECT * FROM users").rows
    );
    q(&c, "CREATE TABLE native (n INTEGER)");
    q(&c, "INSERT INTO native VALUES(7)");
    assert_eq!(
        q(&c, "SELECT post.* FROM native post").rows,
        vec![vec![Value::Integer(7)]]
    );
    assert!(c
        .execute("SELECT unknown.* FROM native", &Parameters::new())
        .is_err());
    assert!(c
        .execute("SELECT posts [0] FROM users", &Parameters::new())
        .is_err());
    // Expanded objects are included in output limits, including prior transaction work.
    q(&c, "BEGIN");
    q(&c, "INSERT INTO native VALUES(8)");
    assert_eq!(
        c.select_with_limits(
            "SELECT posts.*.* FROM users",
            &Parameters::new(),
            ResultLimits {
                max_rows: 1,
                max_payload_bytes: 1
            }
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        q(&c, "SELECT count(*) FROM native").rows,
        vec![vec![Value::Integer(2)]]
    );
    q(&c, "ROLLBACK");
}

#[test]
fn nested_paths_preserve_types_reopen_and_insert_select() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("nested.db");
    let item = Value::Object(std::collections::BTreeMap::from([
        ("n".into(), Value::Integer(i64::MAX)),
        ("blob".into(), Value::Binary(vec![0, 255])),
        (
            "ref".into(),
            Value::Record(romedb::Record {
                table: "users".into(),
                key: romedb::Key::String("a".into()),
            }),
        ),
        (
            "with.dot".into(),
            Value::Object(std::collections::BTreeMap::new()),
        ),
    ]));
    {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        c.execute(
            "INSERT INTO users {id:users:a,posts:$items}",
            &Parameters::from([("$items".into(), Value::Array(vec![item.clone()]))]),
        )
        .unwrap();
        assert_eq!(
            q(&c, "SELECT posts.-1.* FROM users").rows,
            vec![vec![item.clone()]]
        );
        assert_eq!(
            q(&c, "SELECT posts.0.\"with.dot\".* AS payload FROM users").rows,
            vec![vec![Value::Object(std::collections::BTreeMap::new())]]
        );
        assert_eq!(
            q(
                &c,
                "WITH u AS (SELECT posts FROM users) SELECT u.posts.0.* FROM u"
            )
            .rows,
            vec![vec![item.clone()]]
        );
        q(&c, "CREATE TABLE copies");
        q(
            &c,
            "INSERT INTO copies(payload) SELECT posts.0.* FROM users",
        );
        assert_eq!(
            q(&c, "SELECT payload FROM copies").rows,
            vec![vec![item.clone()]]
        );
    }
    let db = Database::open(file.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(q(&c, "SELECT posts.0.* FROM users").rows, vec![vec![item]]);
}

#[test]
fn linked_projection_paths_fetch_in_batches_and_keep_indexing_separate() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "INSERT INTO people {id:people:a,name:'Alice'}");
    q(
        &c,
        "INSERT INTO posts {id:posts:p1,title:'One',author:people:a}",
    );
    q(
        &c,
        "INSERT INTO posts {id:posts:p2,title:'Two',author:people:a}",
    );
    q(&c, "INSERT INTO users {id:users:u,posts:[posts:p1,posts:p2,posts:p1],post:posts:p1,mixed:[1,posts:p1,null,posts:missing]}");
    let plain = q(&c, "SELECT posts FROM users").rows[0][0].clone();
    let Value::Array(ids) = &plain else { panic!() };
    assert!(ids.iter().all(|v| matches!(v, Value::Record(_))));
    for sql in [
        "SELECT posts[0],posts.0,posts[-1],posts[$],posts.-1 FROM users",
        "SELECT u.posts[0],u.posts.0,u.posts[-1],u.posts[$],u.posts.-1 FROM users u",
    ] {
        assert_eq!(q(&c, sql).rows, vec![vec![ids[0].clone(); 5]]);
    }
    for sql in [
        "SELECT coalesce(posts[0],null) FROM users",
        "SELECT posts FROM users WHERE posts[0]=posts:p1",
    ] {
        assert_eq!(
            c.execute(sql, &Parameters::new()).unwrap_err().code(),
            "FDB_UNSUPPORTED"
        );
    }
    let first = q(&c, "SELECT * FROM posts WHERE id=posts:p1").rows[0][0].clone();
    let second = q(&c, "SELECT * FROM posts WHERE id=posts:p2").rows[0][0].clone();
    let expected = Value::Array(vec![first.clone(), second.clone(), first.clone()]);
    for sql in [
        "SELECT posts.* FROM users",
        "SELECT posts.*.* FROM users",
        "SELECT u.posts.* FROM users u",
        "SELECT posts . /* comment */ * FROM users",
    ] {
        assert_eq!(q(&c, sql).rows, vec![vec![expected.clone()]], "{sql}");
    }
    let profile = c
        .profile_select(
            "SELECT posts.*.*,posts[0].*,posts[-2].*,post.* FROM users",
            &Parameters::new(),
        )
        .unwrap();
    assert_eq!(
        profile.result.rows,
        vec![vec![expected, first.clone(), second, first.clone()]]
    );
    assert_eq!(profile.metrics.fetch_batches, 1);
    assert_eq!(
        q(&c, "SELECT posts.*.author.*.name FROM users").rows,
        vec![vec![Value::Array(vec![Value::String("Alice".into()); 3])]]
    );
    assert_eq!(
        q(&c, "SELECT posts.author.*.name FROM users").rows,
        vec![vec![Value::Array(vec![Value::String("Alice".into()); 3])]]
    );
    assert_eq!(
        q(&c, "SELECT mixed.* FROM users").rows,
        vec![vec![Value::Array(vec![
            Value::Integer(1),
            first,
            Value::Null,
            Value::Null
        ])]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT posts[99].*,posts[-99].*,posts[-9223372036854775808].* FROM users"
        )
        .rows,
        vec![vec![Value::Null; 3]]
    );
    q(&c, "BEGIN");
    q(&c, "UPDATE people:a {name:'Pending'}");
    assert_eq!(
        q(&c, "SELECT posts[0].author.*.name FROM users").rows,
        vec![vec![Value::String("Pending".into())]]
    );
    assert_eq!(
        c.select_with_limits(
            "SELECT posts.* FROM users",
            &Parameters::new(),
            ResultLimits {
                max_rows: 1,
                max_payload_bytes: 10
            }
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        q(&c, "SELECT name FROM people").rows,
        vec![vec![Value::String("Pending".into())]]
    );
    q(&c, "ROLLBACK");
}

#[test]
fn projection_cycles_are_finite_and_reference_expansion_is_bounded() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "INSERT INTO posts {id:posts:p1,next:posts:p2}");
    q(&c, "INSERT INTO posts {id:posts:p2,next:posts:p1}");
    q(&c, "INSERT INTO users {id:users:u,posts:[posts:p1]}");
    assert_eq!(
        q(&c, "SELECT posts[0].next.next.* FROM users").rows,
        q(&c, "SELECT * FROM posts WHERE id=posts:p1").rows
    );
    let references = Value::Array(
        (0..16_385)
            .map(|i| {
                Value::Record(romedb::Record {
                    table: "posts".into(),
                    key: romedb::Key::Integer(i),
                })
            })
            .collect(),
    );
    c.execute(
        "INSERT INTO users {id:users:many,posts:$refs}",
        &Parameters::from([("$refs".into(), references)]),
    )
    .unwrap();
    assert_eq!(
        c.execute(
            "SELECT posts.* FROM users WHERE id=users:many",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        q(&c, "SELECT posts[0] FROM users WHERE id=users:many").rows,
        vec![vec![Value::Record(romedb::Record {
            table: "posts".into(),
            key: romedb::Key::Integer(0)
        })]]
    );
}

#[test]
fn bracket_identifiers_adjacent_to_sql_keywords_are_not_array_paths() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(
        &c,
        "INSERT INTO users {id:users:a,\"0\":7,rows:[11],\"select\":[13]}",
    );
    for sql in [
        "SELECT[0]FROM users",
        "SELECT DISTINCT[0]FROM users",
        "SELECT ALL[0]FROM users",
        "SELECT[0]FROM users ORDER BY[0]",
        "SELECT[0]FROM users GROUP BY[0]",
    ] {
        assert_eq!(q(&c, sql).rows, vec![vec![Value::Integer(7)]], "{sql}");
    }
    assert_eq!(
        q(
            &c,
            "WITH u AS(SELECT rows AS \"0\" FROM users)SELECT[0]FROM u"
        )
        .rows,
        vec![vec![Value::Array(vec![Value::Integer(11)])]]
    );
    assert_eq!(
        q(&c, "SELECT rows[0],\"select\"[0] FROM users").rows,
        vec![vec![Value::Integer(11), Value::Integer(13)]]
    );
}
