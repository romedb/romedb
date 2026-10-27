use romedb::{Database, Key, Parameters, Record, Value};
fn query(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
fn setup() -> (Database, romedb::Connection) {
    let db = Database::open(":memory:").expect("open");
    let c = db.connect().expect("connect");
    query(&c, "CREATE TABLE users");
    query(&c, "CREATE INDEX users_city ON users (profile.city)");
    query(&c,"INSERT INTO users {id: users:2, name: 'Bob', active: true, profile: {city: 'Bangkok'}, tags: ['rust'], nickname: null}");
    query(
        &c,
        "INSERT INTO users {id: users:10, name: 'Alice', active: false, profile: {city: 'Berlin'}}",
    );
    query(
        &c,
        "INSERT INTO users {id: users:-1, name: 'Carol', profile: {city: 'Bangkok'}}",
    );
    (db, c)
}
#[test]
fn record_sources_preserve_projections_filters_and_typed_keys() {
    let (_db, c) = setup();
    query(&c, "INSERT INTO docs {id:docs:a,title:'Requested syntax'}");
    assert_eq!(
        query(&c, "SELECT title FROM docs:a").rows,
        vec![vec![Value::String("Requested syntax".into())]]
    );
    query(
        &c,
        "INSERT INTO users {id:users:a,name:'String key',active:true}",
    );
    query(&c, "INSERT INTO users {id:users:`2`,name:'String two'}");
    for (source, key) in [
        ("users:a", "users:a"),
        ("USERS:2", "users:2"),
        ("users:-1", "users:-1"),
        ("users:`2`", "users:`2`"),
        ("users:missing", "users:missing"),
    ] {
        for projection in ["name", "*", "id,active,tags,unknown", "users.profile.city"] {
            assert_eq!(
                query(&c, &format!("SELECT {projection} FROM {source}")).rows,
                query(
                    &c,
                    &format!("SELECT {projection} FROM users WHERE id={key}")
                )
                .rows
            );
        }
    }
    assert_eq!(
        query(
            &c,
            "SELECT u.name FROM users:2 AS u WHERE u.active=true ORDER BY u.name LIMIT 1"
        )
        .rows,
        vec![vec![Value::String("Bob".into())]]
    );
    assert!(query(
        &c,
        "SELECT name FROM users:2 WHERE active=false OR name='Alice'"
    )
    .rows
    .is_empty());
    assert_eq!(
        query(&c, "SELECT count(*) FROM users:missing").rows,
        vec![vec![Value::Integer(0)]]
    );
    query(
        &c,
        "INSERT INTO users {id:users:9223372036854775807,name:'Max'}",
    );
    query(
        &c,
        "INSERT INTO users {id:users:-9223372036854775808,name:'Min'}",
    );
    assert_eq!(
        query(&c, "SELECT name FROM users:9223372036854775807").rows,
        vec![vec![Value::String("Max".into())]]
    );
    assert_eq!(
        query(&c, "SELECT name FROM users:-9223372036854775808").rows,
        vec![vec![Value::String("Min".into())]]
    );
}

#[test]
fn record_sources_compose_with_joins_subqueries_and_transactions() {
    let (_db, c) = setup();
    assert_eq!(
        query(
            &c,
            "SELECT u.name,v.name FROM users:2 u LEFT JOIN users:missing v ON true"
        )
        .rows,
        vec![vec![Value::String("Bob".into()), Value::Null]]
    );
    assert_eq!(
        query(
            &c,
            "SELECT u.name,v.name FROM users:2 u JOIN users:10 v ON u.id!=v.id"
        )
        .rows,
        vec![vec![
            Value::String("Bob".into()),
            Value::String("Alice".into())
        ]]
    );
    assert_eq!(
        query(&c, "SELECT u.name,v.name FROM users:2 u, users:10 v").rows,
        vec![vec![
            Value::String("Bob".into()),
            Value::String("Alice".into())
        ]]
    );
    assert_eq!(
        query(
            &c,
            "WITH chosen AS (SELECT name FROM users:2) SELECT name FROM chosen"
        )
        .rows,
        vec![vec![Value::String("Bob".into())]]
    );
    assert_eq!(
        query(&c, "SELECT q.name FROM (SELECT name FROM users:2) q").rows,
        vec![vec![Value::String("Bob".into())]]
    );
    assert_eq!(
        query(
            &c,
            "SELECT name FROM users WHERE EXISTS(SELECT 1 FROM users:2 u WHERE u.id=users.id)"
        )
        .rows,
        vec![vec![Value::String("Bob".into())]]
    );
    query(&c, "BEGIN");
    query(&c, "UPDATE users:2 {name:'Changed'}");
    assert_eq!(
        query(&c, "SELECT name FROM users:2").rows,
        vec![vec![Value::String("Changed".into())]]
    );
    query(&c, "ROLLBACK");
    assert_eq!(
        query(&c, "SELECT name FROM users:2").rows,
        vec![vec![Value::String("Bob".into())]]
    );
    let plan = query(&c, "EXPLAIN QUERY PLAN SELECT name FROM users:2");
    let details = format!("{:?}", plan.rows);
    assert!(
        details.contains("SEARCH") && details.contains("(id=?)"),
        "{details}"
    );
    query(
        &c,
        &format!(
            "INSERT INTO users(n) VALUES {}",
            (0..1000)
                .map(|n| format!("({n})"))
                .collect::<Vec<_>>()
                .join(",")
        ),
    );
    let profile = c
        .profile_select("SELECT name FROM users:2", &Parameters::new())
        .unwrap();
    assert_eq!(profile.result.rows, vec![vec![Value::String("Bob".into())]]);
    assert_eq!(profile.metrics.fullscan_steps, 0);
    assert!(profile.metrics.rows_read <= 3, "{:?}", profile.metrics);
}

#[test]
fn record_sources_bind_constructor_keys_without_interpolating_sql() {
    let (_db, c) = setup();
    let params = Parameters::from([
        ("$key".into(), Value::Integer(2)),
        ("$name".into(), Value::String("Bob".into())),
    ]);
    assert_eq!(
        c.execute(
            "SELECT u.name FROM type::record('users',$key) u WHERE u.name=$name",
            &params
        )
        .unwrap()
        .rows,
        vec![vec![Value::String("Bob".into())]]
    );
    let positional = Parameters::from([
        ("?1".into(), Value::Integer(2)),
        ("?2".into(), Value::String("Bob".into())),
    ]);
    assert_eq!(
        c.execute(
            "SELECT name FROM type::record('users',?) WHERE name=?",
            &positional
        )
        .unwrap()
        .rows,
        vec![vec![Value::String("Bob".into())]]
    );
    let malicious = Parameters::from([(
        "$key".into(),
        Value::String("a'); DROP TABLE users; --".into()),
    )]);
    c.execute(
        "INSERT INTO users DOCUMENT $doc",
        &Parameters::from([(
            "$doc".into(),
            Value::Object(std::collections::BTreeMap::from([
                (
                    "id".into(),
                    Value::Record(Record {
                        table: "users".into(),
                        key: Key::String("a'); DROP TABLE users; --".into()),
                    }),
                ),
                ("name".into(), Value::String("Quoted".into())),
            ])),
        )]),
    )
    .unwrap();
    assert_eq!(
        c.execute("SELECT name FROM type::record('users',$key)", &malicious)
            .unwrap()
            .rows,
        vec![vec![Value::String("Quoted".into())]]
    );
    for sql in [
        "SELECT name FROM unknown:a",
        "SELECT name FROM type::record('users',1.5)",
        "SELECT name FROM type::record('users',null)",
        "SELECT name FROM __romedb_record_value('users',2)",
        "SELECT name FROM users:9223372036854775808",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert!(c
        .execute(
            "SELECT name FROM users:2",
            &Parameters::from([("$unused".into(), Value::Integer(1))])
        )
        .is_err());
    assert_eq!(
        query(&c, "SELECT count(*) FROM users").rows,
        vec![vec![Value::Integer(4)]]
    );
}
#[test]
fn projections_filters_sort_and_pagination() {
    let (_db, c) = setup();
    let result=query(&c,"SELECT u.id, u.name, u.active, u.profile.city AS city, u.tags, u.unknown FROM users AS u WHERE u.profile.city = 'Bangkok' ORDER BY u.name LIMIT 1 OFFSET 0");
    assert_eq!(
        result.columns,
        vec!["id", "name", "active", "city", "tags", "unknown"]
    );
    assert_eq!(
        result.rows[0],
        vec![
            Value::Record(Record {
                table: "users".into(),
                key: Key::Integer(2)
            }),
            Value::String("Bob".into()),
            Value::Boolean(true),
            Value::String("Bangkok".into()),
            Value::Array(vec![Value::String("rust".into())]),
            Value::Null
        ]
    );
    let result = query(
        &c,
        "SELECT name, length(name) AS size FROM users WHERE active = true ORDER BY name",
    );
    assert_eq!(
        result.rows,
        vec![vec![Value::String("Bob".into()), Value::Integer(3)]]
    );
    assert_eq!(
        query(&c, "SELECT name FROM users WHERE id = users:10").rows,
        vec![vec![Value::String("Alice".into())]]
    );
    let values = query(&c, "SELECT id FROM users ORDER BY id").rows;
    assert_eq!(
        values.into_iter().map(|r| r[0].clone()).collect::<Vec<_>>(),
        [-1, 2, 10].map(|key| Value::Record(Record {
            table: "users".into(),
            key: Key::Integer(key)
        }))
    );
}
#[test]
fn full_documents_preserve_absence_and_literal_keys() {
    let (_db, c) = setup();
    query(&c,"INSERT INTO users {id: users:literal, \"profile.city\": 'literal', profile: {city: 'nested'}}");
    assert_eq!(query(&c,"SELECT u.\"profile.city\" AS literal, u.profile.city AS nested FROM users u WHERE id = users:literal").rows,vec![vec![Value::String("literal".into()),Value::String("nested".into())]]);
    let row = query(&c, "SELECT u.* FROM users u WHERE id = users:2")
        .exactly_one()
        .expect("one");
    let Value::Object(doc) = &row[0] else {
        panic!("document");
    };
    assert_eq!(doc["nickname"], Value::Null);
    assert!(!doc.contains_key("unknown"));
    assert!(c
        .execute("SELECT u.profile = u.tags FROM users u", &Parameters::new())
        .is_err());
    let duplicate = query(&c, "SELECT name, name FROM users ORDER BY id");
    assert_eq!(duplicate.columns, vec!["name", "name"]);
    let single = query(&c, "SELECT name FROM users ORDER BY id");
    assert_eq!(
        duplicate.rows,
        single
            .rows
            .into_iter()
            .map(|row| vec![row[0].clone(), row[0].clone()])
            .collect::<Vec<_>>()
    );
}
#[test]
fn relational_record_text_joins_documents_by_record() {
    let (_db, c) = setup();
    query(&c, "CREATE TABLE roles");
    query(&c, "INSERT INTO roles {id: roles:`a-1`, name: 'admin'}");
    query(
        &c,
        "UPDATE users SET role = roles:`a-1`, note = 'users:10' WHERE id = users:2",
    );
    query(
        &c,
        "CREATE TABLE notes(id INTEGER PRIMARY KEY, body TEXT, user_id TEXT, role_ref TEXT)",
    );
    query(&c, "INSERT INTO notes VALUES (1, 'hi', 'users:2', 'roles:a-1'), (2, 'yo', 'users:10', 'roles:`a-1`'), (3, 'neg', 'users:-1', NULL), (4, 'none', 'users:404', 'x'), (5, 'text', 'Bob', NULL)");
    let names = |sql: &str| -> Vec<Vec<Value>> { query(&c, sql).rows };
    let text = |values: &[(&str, Option<&str>)]| -> Vec<Vec<Value>> {
        values
            .iter()
            .map(|(a, b)| {
                vec![
                    Value::String((*a).into()),
                    b.map_or(Value::Null, |b| Value::String(b.into())),
                ]
            })
            .collect()
    };
    let joined = text(&[
        ("hi", Some("Bob")),
        ("yo", Some("Alice")),
        ("neg", Some("Carol")),
    ]);
    assert_eq!(
        names("SELECT n.body, u.name FROM notes n JOIN users u ON u.id = n.user_id ORDER BY n.id"),
        joined
    );
    assert_eq!(
        names("SELECT n.body, u.name FROM notes n JOIN users u ON n.user_id = u.id ORDER BY n.id"),
        joined
    );
    assert_eq!(
        names("SELECT n.body, u.name FROM notes n, users u WHERE u.id = n.user_id ORDER BY n.id"),
        joined
    );
    assert_eq!(
        names("SELECT n.body, u.name FROM notes n LEFT JOIN users u ON u.id = n.user_id ORDER BY n.id"),
        text(&[("hi", Some("Bob")), ("yo", Some("Alice")), ("neg", Some("Carol")), ("none", None), ("text", None)])
    );
    assert_eq!(
        names(
            "SELECT n.body, u.name FROM notes n JOIN users u ON u.role = n.role_ref ORDER BY n.id"
        ),
        text(&[("hi", Some("Bob")), ("yo", Some("Bob"))])
    );
    assert_eq!(
        names("SELECT n.body, u.name FROM notes n JOIN users u ON u.note = n.user_id"),
        text(&[("yo", Some("Bob"))])
    );
    assert_eq!(
        names("SELECT n.body, u.name FROM notes n JOIN users u ON u.name = n.user_id"),
        text(&[("text", Some("Bob"))])
    );
    assert_eq!(
        names("SELECT count(*) FROM notes n JOIN users u ON u.id != n.user_id"),
        vec![vec![Value::Integer(12)]]
    );
    let membership = |sql: &str| -> Vec<String> {
        query(&c, sql)
            .rows
            .into_iter()
            .map(|row| match &row[0] {
                Value::String(text) => text.clone(),
                other => panic!("{other:?}"),
            })
            .collect()
    };
    assert_eq!(
        membership("SELECT name FROM users WHERE id IN (SELECT user_id FROM notes) ORDER BY name"),
        ["Alice", "Bob", "Carol"]
    );
    assert_eq!(
        membership("SELECT name FROM users WHERE id NOT IN (SELECT user_id FROM notes WHERE id < 3) ORDER BY name"),
        ["Carol"]
    );
    assert_eq!(
        membership("SELECT body FROM notes WHERE user_id IN (SELECT id FROM users) ORDER BY id"),
        ["hi", "yo", "neg"]
    );
    assert_eq!(
        membership(
            "SELECT body FROM notes WHERE user_id NOT IN (SELECT id FROM users) ORDER BY id"
        ),
        ["none", "text"]
    );
    let plan = query(
        &c,
        "EXPLAIN QUERY PLAN SELECT n.body, u.name FROM notes n JOIN users u ON u.id = n.user_id",
    );
    let plan = format!("{:?}", plan.rows);
    assert!(
        !plan.contains("SCAN notes") || plan.contains("HASH JOIN") || plan.contains("PRIMARY KEY"),
        "{plan}"
    );
}
#[test]
fn collection_joins_resolve_unqualified_names_and_bare_aliases() {
    let (_db, c) = setup();
    query(
        &c,
        "CREATE TABLE notes(id INTEGER PRIMARY KEY, body TEXT, user_id TEXT)",
    );
    query(
        &c,
        "INSERT INTO notes (id, body, user_id) VALUES (1, 'hi', 'users:2'), (2, 'yo', 'users:10')",
    );
    assert_eq!(
        query(&c, "SELECT n.body, name, active FROM notes n JOIN users u ON u.id = n.user_id ORDER BY n.id").rows,
        vec![
            vec![Value::String("hi".into()), Value::String("Bob".into()), Value::Boolean(true)],
            vec![Value::String("yo".into()), Value::String("Alice".into()), Value::Boolean(false)],
        ]
    );
    assert_eq!(
        query(
            &c,
            "SELECT notes.body FROM notes JOIN users ON users.id = notes.user_id WHERE active"
        )
        .rows,
        vec![vec![Value::String("hi".into())]]
    );
    let documents = query(
        &c,
        "SELECT n.body, u FROM notes n JOIN users u ON u.id = n.user_id ORDER BY n.id",
    );
    assert_eq!(documents.columns, vec!["body".to_owned(), "u".to_owned()]);
    let Value::Object(bob) = &documents.rows[0][1] else {
        panic!("whole document expected: {:?}", documents.rows[0][1]);
    };
    assert_eq!(bob.get("name"), Some(&Value::String("Bob".into())));
    // Any document can have a field named like a relational column.
    for (sql, name) in [
        (
            "SELECT body FROM notes n JOIN users u ON u.id = n.user_id",
            "body",
        ),
        (
            "SELECT n.body FROM notes n JOIN users u ON u.id = user_id",
            "user_id",
        ),
        (
            "SELECT id FROM notes n JOIN users u ON u.id = n.user_id",
            "id",
        ),
    ] {
        let error = c.execute(sql, &Parameters::new()).unwrap_err();
        assert!(
            error.to_string().contains(&format!("qualify {name}")),
            "{sql}: {error}"
        );
    }
    query(&c, "CREATE TABLE people");
    query(&c, "DEFINE FIELD name ON people TYPE string");
    query(&c, "DEFINE SCHEMA ON people STRICT");
    query(&c, "INSERT INTO people {id: people:1, name: 'Dana'}");
    query(
        &c,
        "INSERT INTO notes (id, body, user_id) VALUES (3, 'strict', 'people:1')",
    );
    assert_eq!(
        query(
            &c,
            "SELECT body, name FROM notes n JOIN people p ON p.id = user_id"
        )
        .rows,
        vec![vec![
            Value::String("strict".into()),
            Value::String("Dana".into())
        ]]
    );
}
#[test]
fn joins_and_typed_parameter_filters() {
    let (_db, c) = setup();
    query(&c, "CREATE TABLE posts");
    query(
        &c,
        "INSERT INTO posts {id: posts:p1, title: 'Hello', author: users:2}",
    );
    query(
        &c,
        "INSERT INTO posts {id: posts:p2, title: 'Orphan', author: users:404}",
    );
    let result = query(
        &c,
        "SELECT p.title, u.name FROM posts p LEFT JOIN users u ON p.author = u.id ORDER BY p.title",
    );
    assert_eq!(
        result.rows,
        vec![
            vec![Value::String("Hello".into()), Value::String("Bob".into())],
            vec![Value::String("Orphan".into()), Value::Null]
        ]
    );
    query(
        &c,
        "CREATE TABLE cities(name TEXT PRIMARY KEY, country TEXT)",
    );
    query(&c, "INSERT INTO cities VALUES ('Bangkok', 'Thailand')");
    assert_eq!(query(&c,"SELECT u.name, c.country FROM users u JOIN cities c ON u.profile.city = c.name ORDER BY u.name").rows.len(),2);
    let params = Parameters::from([
        ("$city".into(), Value::String("Bangkok".into())),
        (
            "$id".into(),
            Value::Record(Record {
                table: "users".into(),
                key: Key::Integer(2),
            }),
        ),
    ]);
    assert_eq!(
        c.execute(
            "SELECT name FROM users WHERE users.profile.city = $city AND id = $id",
            &params
        )
        .expect("bound filters")
        .rows
        .len(),
        1
    );
    assert_eq!(
        query(&c, "SELECT name FROM users WHERE id = 'users:2'")
            .rows
            .len(),
        0
    );
}
#[test]
fn planner_uses_managed_index_and_sees_transaction_changes() {
    let (_db, c) = setup();
    let plan = query(
        &c,
        "EXPLAIN QUERY PLAN SELECT u.name FROM users u WHERE u.profile.city = 'Bangkok'",
    );
    let details = format!("{:?}", plan.rows);
    assert!(details.contains("users_city"), "{details}");
    assert!(details.to_ascii_uppercase().contains("SEARCH"), "{details}");
    query(&c, "BEGIN");
    query(&c, "UPDATE users:2 {profile: {city: 'Paris'}}");
    assert_eq!(
        query(&c, "SELECT * FROM users u WHERE u.profile.city = 'Bangkok'")
            .rows
            .len(),
        1
    );
    query(&c, "ROLLBACK");
    assert_eq!(
        query(&c, "SELECT * FROM users u WHERE u.profile.city = 'Bangkok'")
            .rows
            .len(),
        2
    );
    assert!(c
        .execute("SELECT u.doc FROM __romedb_catalog u", &Parameters::new())
        .is_err());
    assert!(c
        .execute(
            "SELECT __romedb_value(u.doc, '[]') FROM users u",
            &Parameters::new()
        )
        .is_err());
}

#[test]
fn positional_ordering_and_indexed_null_semantics() {
    let (_db, c) = setup();
    assert_eq!(
        query(&c, "SELECT id FROM users ORDER BY 1").rows,
        query(&c, "SELECT id FROM users ORDER BY id").rows
    );
    query(
        &c,
        "INSERT INTO users {id: users:nullcity, profile: {city: null}}",
    );
    assert_eq!(
        query(&c, "SELECT id FROM users u WHERE u.profile.city IS NULL")
            .rows
            .len(),
        1
    );
    assert_eq!(
        query(&c, "SELECT id FROM users u WHERE u.profile.city = NULL")
            .rows
            .len(),
        0
    );
    let plan = query(
        &c,
        "EXPLAIN QUERY PLAN SELECT name FROM users WHERE id = users:2",
    );
    assert!(format!("{:?}", plan.rows).contains("SEARCH"));
    let profiles = query(&c, "SELECT DISTINCT profile FROM users").rows;
    assert_eq!(profiles.len(), 3);
    for city in [
        Value::String("Bangkok".into()),
        Value::String("Berlin".into()),
        Value::Null,
    ] {
        assert!(
            profiles.contains(&vec![Value::Object(std::collections::BTreeMap::from([(
                "city".into(),
                city
            )]))])
        );
    }
}

#[test]
fn deeply_nested_qualified_paths_preserve_values_and_index_predicates() {
    let (_db, c) = setup();
    query(&c, "INSERT INTO users {id:users:deep, profile:{address:{city:'Paris',details:{active:true,ref:users:2}},\"address.city\":{label:'literal'}}}");
    let sql = "SELECT u.profile.address.city AS city, u.profile.address.details.active AS active, u.profile.address.details.ref AS reference, u.profile.\"address.city\".label AS literal, u.profile.missing.value AS missing FROM users u WHERE u.profile.address.city='Paris'";
    let expected = vec![vec![
        Value::String("Paris".into()),
        Value::Boolean(true),
        Value::Record(Record {
            table: "users".into(),
            key: Key::Integer(2),
        }),
        Value::String("literal".into()),
        Value::Null,
    ]];
    assert_eq!(query(&c, sql).rows, expected);
    query(
        &c,
        "CREATE INDEX users_deep_city ON users (profile.address.city)",
    );
    assert_eq!(query(&c, sql).rows, expected);
    assert_eq!(query(&c,"SELECT u.profile.address.city FROM users u WHERE u.profile.address.city='Paris' ORDER BY u.profile.address.city").columns,vec!["city"]);
    assert_eq!(query(&c,"SELECT a.profile.address.city AS city FROM users a JOIN users b ON a.profile.address.details.ref=b.id WHERE b.id=users:2").rows,vec![vec![Value::String("Paris".into())]]);
    assert!(c
        .execute(
            "SELECT __romedb_path(u,profile,address,city) FROM users u",
            &Parameters::new()
        )
        .is_err());
    assert!(c
        .execute(
            "SELECT missing.profile.address.city FROM users u",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(
        query(
            &c,
            "SELECT 'u.profile.address.city' AS literal FROM users WHERE id=users:deep"
        )
        .rows,
        vec![vec![Value::String("u.profile.address.city".into())]]
    );
}

#[test]
fn deep_paths_handle_quoting_comments_and_depth_limits() {
    let (_db, c) = setup();
    query(
        &c,
        r#"INSERT INTO users {id:users:quoted, profile:{"a.b":{"quo""te":7}}}"#,
    );
    assert_eq!(query(&c,r#"SELECT [u].profile /* gap */ .[a.b]."quo""te" AS value FROM users u WHERE id=users:quoted"#).rows,vec![vec![Value::Integer(7)]]);
    let path = std::iter::repeat_n("missing", 64)
        .collect::<Vec<_>>()
        .join(".");
    assert_eq!(
        query(
            &c,
            &format!("SELECT u.{path} AS absent FROM users u WHERE id=users:2")
        )
        .rows,
        vec![vec![Value::Null]]
    );
    let error = c
        .execute(
            &format!("SELECT u.{path}.extra FROM users u"),
            &Parameters::new(),
        )
        .unwrap_err();
    assert_eq!(error.code(), "FDB_LIMIT");
    query(&c, "CREATE TABLE ordinary(value INTEGER)");
    query(&c, "INSERT INTO ordinary VALUES (9)");
    assert_eq!(
        query(&c, "SELECT main.ordinary.value FROM ordinary").rows,
        vec![vec![Value::Integer(9)]]
    );
}

#[test]
fn expression_column_names_preserve_literals_and_render_public_paths() {
    let (_db, c) = setup();
    query(
        &c,
        "INSERT INTO users {id:users:labels,profile:{address:{city:'Paris'}}}",
    );
    let result=query(&c,"SELECT upper(users.profile.address.city), '__romedb_fetch', coalesce(NULL,'__romedb_h_doc_get'), record::id(id), lower('__romedb_path(x,y,z,w)') FROM users WHERE id=users:labels");
    assert_eq!(
        result.columns[0],
        r#"upper ("users"."profile"."address"."city")"#
    );
    assert_eq!(result.columns[1], "'__romedb_fetch'");
    assert!(result.columns[2].contains("'__romedb_h_doc_get'"));
    assert!(result.columns[3].starts_with("record::id"));
    assert!(result.columns[4].contains("'__romedb_path(x,y,z,w)'"));
    assert_eq!(result.rows[0][0], Value::String("PARIS".into()));
    let returned=query(&c,"UPDATE users SET profile.address.city='Rome' WHERE id=users:labels RETURNING upper(users.profile.address.city)");
    assert_eq!(returned.columns[0], result.columns[0]);
    assert_eq!(returned.rows[0][0], Value::String("ROME".into()));
}

#[test]
fn distinct_uses_scalar_equality_and_paginates_after_deduplication() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    for (i, value) in [
        Value::Integer(1),
        Value::Number(1.0),
        Value::Boolean(true),
        Value::Integer(2),
        Value::Null,
        Value::Null,
        Value::String("1".into()),
    ]
    .into_iter()
    .enumerate()
    {
        c.execute(
            "INSERT INTO docs (id,v) VALUES (type::record('docs',$id),$v)",
            &Parameters::from([
                ("$id".into(), Value::Integer(i as i64)),
                ("$v".into(), value),
            ]),
        )
        .unwrap();
    }
    let all = query(&c, "SELECT DISTINCT v FROM docs ORDER BY v");
    query(&c, "CREATE TABLE baseline(v)");
    query(
        &c,
        "INSERT INTO baseline VALUES (1),(1.0),(1),(2),(NULL),(NULL),('1')",
    );
    assert_eq!(
        query(
            &c,
            "SELECT DISTINCT v+0 AS n FROM docs ORDER BY n DESC LIMIT 2 OFFSET 1"
        )
        .rows,
        query(
            &c,
            "SELECT DISTINCT v+0 AS n FROM baseline ORDER BY n DESC LIMIT 2 OFFSET 1"
        )
        .rows
    );
    assert_eq!(
        query(
            &c,
            "SELECT DISTINCT count(*) AS n FROM docs GROUP BY v ORDER BY n"
        )
        .rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(3)]
        ]
    );
    assert_eq!(all.rows.len(), 4);
    assert_eq!(all.rows[0], vec![Value::Null]);
    assert_eq!(all.rows[2], vec![Value::Integer(2)]);
    assert_eq!(all.rows[3], vec![Value::String("1".into())]);
    let page = c
        .execute(
            "SELECT DISTINCT v FROM docs ORDER BY v LIMIT $n OFFSET $skip",
            &Parameters::from([
                ("$n".into(), Value::Integer(1)),
                ("$skip".into(), Value::Integer(2)),
            ]),
        )
        .unwrap();
    assert_eq!(page.rows, vec![vec![Value::Integer(2)]]);
    assert_eq!(
        query(&c, "SELECT DISTINCT length('x') AS n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(
        query(&c, "SELECT DISTINCT count(*) AS n FROM docs").rows,
        vec![vec![Value::Integer(7)]]
    );
    assert_eq!(query(&c,"SELECT DISTINCT row_number() OVER (ORDER BY id) AS n FROM docs ORDER BY n DESC LIMIT 2").rows,vec![vec![Value::Integer(7)],vec![Value::Integer(6)]]);
}

#[test]
fn distinct_preserves_record_identity_collation_and_binary_values() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    for value in [
        Value::Record(Record {
            table: "Docs".into(),
            key: Key::Integer(1),
        }),
        Value::Record(Record {
            table: "docs".into(),
            key: Key::Integer(1),
        }),
        Value::Record(Record {
            table: "docs".into(),
            key: Key::String("1".into()),
        }),
        Value::Binary(vec![1]),
        Value::Binary(vec![1]),
        Value::String("A".into()),
        Value::String("a".into()),
    ] {
        c.execute(
            "INSERT INTO docs (v) VALUES ($v)",
            &Parameters::from([("$v".into(), value)]),
        )
        .unwrap();
    }
    let values = query(&c, "SELECT DISTINCT v FROM docs");
    assert_eq!(values.rows.len(), 5);
    assert_eq!(
        values
            .rows
            .iter()
            .filter(|r| matches!(&r[0], Value::Record(_)))
            .count(),
        2
    );
    assert_eq!(
        query(
            &c,
            "SELECT DISTINCT v COLLATE NOCASE AS text FROM docs WHERE typeof(v)='text'"
        )
        .rows
        .len(),
        1
    );
    assert_eq!(
        query(&c, "SELECT DISTINCT v FROM docs ORDER BY (1) LIMIT 2")
            .rows
            .len(),
        2
    );
    for position in ["0", "99", "-1", "(99)"] {
        assert!(c
            .execute(
                &format!("SELECT DISTINCT v FROM docs ORDER BY {position}"),
                &Parameters::new()
            )
            .is_err());
    }
    query(&c, "CREATE TABLE copied");
    assert_eq!(
        query(&c, "INSERT INTO copied (v) SELECT DISTINCT v FROM docs").affected,
        5
    );
    query(&c, "CREATE TABLE arrays");
    query(&c, "INSERT INTO arrays {v:[1]}");
    query(&c, "INSERT INTO arrays {v:[1]}");
    assert_eq!(
        query(&c, "SELECT DISTINCT v FROM arrays").rows,
        vec![vec![Value::Array(vec![Value::Integer(1)])]]
    );
}

#[test]
fn distinct_orders_the_projected_volatile_value() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    for _ in 0..64 {
        query(&c, "INSERT INTO docs {}");
    }
    for order in ["n", "1", "random()"] {
        let result = query(
            &c,
            &format!("SELECT DISTINCT random() AS n FROM docs ORDER BY {order}"),
        );
        let numbers = result
            .rows
            .iter()
            .map(|r| match r[0] {
                Value::Integer(i) => i,
                _ => panic!("integer random value"),
            })
            .collect::<Vec<_>>();
        assert!(
            numbers.windows(2).all(|pair| pair[0] < pair[1]),
            "{order}: {numbers:?}"
        );
    }
}

#[test]
fn collated_order_aliases_match_relational_precedence_and_distinct() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(v,n)");
    for (v, n) in [("a", "z"), ("B", "y"), ("c", "x"), ("a", "w")] {
        query(&c, &format!("INSERT INTO docs {{v:'{v}',n:'{n}'}}"));
        query(&c, &format!("INSERT INTO baseline VALUES ('{v}','{n}')"));
    }
    for distinct in ["", "DISTINCT "] {
        for order in [
            "n COLLATE NOCASE",
            "(n) COLLATE NOCASE DESC",
            "(1) COLLATE NOCASE",
            "v COLLATE NOCASE",
        ] {
            let collection = query(
                &c,
                &format!("SELECT {distinct}v AS n FROM docs ORDER BY {order}"),
            );
            let ordinary = query(
                &c,
                &format!("SELECT {distinct}v AS n FROM baseline ORDER BY {order}"),
            );
            assert_eq!(collection.rows, ordinary.rows, "{distinct}{order}");
        }
    }
    query(&c, "CREATE TABLE rel(x)");
    query(&c, "INSERT INTO rel VALUES ('A'),('a')");
    assert_eq!(
        query(
            &c,
            "SELECT DISTINCT d.v,r.x COLLATE NOCASE AS x FROM docs d CROSS JOIN rel r ORDER BY d.v"
        )
        .rows
        .len(),
        3
    );
}

#[test]
fn nested_order_aliases_match_relational_results_and_reuse_distinct_outputs() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(v,n)");
    for (v, n) in [(1, 9), (2, 8), (3, 7), (1, 9)] {
        query(&c, &format!("INSERT INTO docs {{v:{v},n:{n}}}"));
        query(&c, &format!("INSERT INTO baseline VALUES ({v},{n})"));
    }
    for distinct in ["", "DISTINCT "] {
        for order in ["n+0", "abs(n) DESC", "coalesce(n,0)", "n+docs.n DESC"] {
            let collection = query(
                &c,
                &format!("SELECT {distinct}v AS n FROM docs ORDER BY {order}"),
            );
            let ordinary = query(
                &c,
                &format!(
                    "SELECT {distinct}v AS n FROM baseline ORDER BY {}",
                    order.replace("docs.", "baseline.")
                ),
            );
            assert_eq!(collection.rows, ordinary.rows, "{distinct}{order}");
        }
    }
    for _ in 0..60 {
        query(&c, "INSERT INTO docs {v:1}");
    }
    let result = query(&c, "SELECT DISTINCT random() AS n FROM docs ORDER BY n+0");
    let values = result
        .rows
        .iter()
        .map(|r| match r[0] {
            Value::Integer(i) => i,
            _ => panic!("integer random"),
        })
        .collect::<Vec<_>>();
    assert!(values.windows(2).all(|p| p[0] < p[1]));
    assert!(c
        .execute(
            "SELECT DISTINCT sum(v) AS n FROM docs ORDER BY sum(n)",
            &Parameters::new()
        )
        .is_err());
}

#[test]
fn order_alias_helpers_and_mixed_sources_retain_types() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "INSERT INTO docs {id:docs:2,v:1}");
    query(&c, "INSERT INTO docs {id:docs:10,v:2}");
    query(&c, "CREATE TABLE rel(v,bias)");
    query(&c, "INSERT INTO rel VALUES (1,5),(2,1)");
    for distinct in ["", "DISTINCT "] {
        let records = query(
            &c,
            &format!("SELECT {distinct}id AS ref FROM docs ORDER BY record::id(ref) DESC"),
        );
        assert!(matches!(
            &records.rows[0][0],
            Value::Record(Record {
                key: Key::Integer(10),
                ..
            })
        ));
        assert_eq!(query(&c,&format!("SELECT {distinct}d.v AS score FROM docs d JOIN rel r ON d.v=r.v ORDER BY score+r.bias")).rows,vec![vec![Value::Integer(2)],vec![Value::Integer(1)]]);
        assert_eq!(query(&c,&format!("SELECT {distinct}v AS bucket,sum(v) AS total FROM docs GROUP BY v ORDER BY abs(total) DESC")).rows,vec![vec![Value::Integer(2),Value::Integer(2)],vec![Value::Integer(1),Value::Integer(1)]]);
    }
}

#[test]
fn ordering_ordinal_recognition_matches_pinned_constant_expression_rules() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(v INTEGER)");
    for value in [2, 1, 3] {
        query(&c, &format!("INSERT INTO docs {{v:{value}}}"));
        query(&c, &format!("INSERT INTO baseline VALUES ({value})"));
    }
    for distinct in ["", "DISTINCT "] {
        for order in [
            "1",
            "+1",
            "(1)",
            "((+1))",
            "1 COLLATE BINARY",
            "+(1)",
            "+(+1)",
            "-(-1)",
            "-(+1)",
            "1.0",
            "1+0",
        ] {
            let sql =
                |table: &str| format!("SELECT {distinct}v FROM {table} ORDER BY {order},v DESC");
            assert_eq!(
                query(&c, &sql("docs")).rows,
                query(&c, &sql("baseline")).rows,
                "{distinct}{order}"
            );
        }
        for order in [
            "0",
            "-1",
            "+0",
            "(2)",
            "9223372036854775808",
            "18446744073709551615",
            "-9223372036854775808",
        ] {
            let sql = |table: &str| format!("SELECT {distinct}v FROM {table} ORDER BY {order}");
            match (
                c.execute(&sql("baseline"), &Parameters::new()),
                c.execute(&sql("docs"), &Parameters::new()),
            ) {
                (Ok(mut native), Ok(mut actual)) => {
                    // These accepted oversized numeric constants do not order
                    // rows; DISTINCT may use a different physical plan.
                    native.rows.sort_by_key(|row| format!("{row:?}"));
                    actual.rows.sort_by_key(|row| format!("{row:?}"));
                    assert_eq!(actual.rows, native.rows, "{distinct}{order}")
                }
                (Err(_), Err(_)) => {}
                (native, actual) => panic!("{distinct}{order}: native={native:?}, docs={actual:?}"),
            }
        }
    }
}

#[test]
fn typed_record_ranges_use_numeric_keys_and_preserve_write_rollback() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    for key in [-10, -2, 2, 10] {
        c.execute("INSERT INTO docs (id,ref) VALUES (type::record('docs',$key),type::record('Docs',$key))", &Parameters::from([("$key".into(),Value::Integer(key))])).unwrap();
    }
    assert_eq!(
        query(
            &c,
            "SELECT record::id(id) AS key FROM docs WHERE ref < docs:10 ORDER BY id"
        )
        .rows,
        vec![
            vec![Value::Integer(-10)],
            vec![Value::Integer(-2)],
            vec![Value::Integer(2)]
        ]
    );
    assert_eq!(query(&c,"SELECT record::id(id) AS key FROM docs WHERE (ref) >= type::record('DOCS',2) ORDER BY id").rows, vec![vec![Value::Integer(2)],vec![Value::Integer(10)]]);
    query(&c, "UPDATE docs SET flag = ref <= docs:2");
    assert_eq!(
        query(&c, "SELECT flag FROM docs ORDER BY id").rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(1)],
            vec![Value::Integer(1)],
            vec![Value::Integer(0)]
        ]
    );
    query(&c, "BEGIN");
    assert_eq!(
        query(
            &c,
            "DELETE FROM docs WHERE ref > docs:2 RETURNING ref > docs:2 AS matched"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    query(&c, "ROLLBACK");
    assert_eq!(
        query(&c, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(4)]]
    );
    let p = Parameters::from([
        (
            "$a".into(),
            Value::Record(Record {
                table: "docs".into(),
                key: Key::Integer(10),
            }),
        ),
        (
            "$b".into(),
            Value::Record(Record {
                table: "DOCS".into(),
                key: Key::String("2".into()),
            }),
        ),
    ]);
    assert_eq!(
        c.execute("SELECT $a < $b AS value", &p).unwrap().rows,
        vec![vec![Value::Integer(1)]]
    );
    let p = Parameters::from([
        ("$a".into(), Value::Null),
        (
            "$b".into(),
            Value::Record(Record {
                table: "docs".into(),
                key: Key::Integer(1),
            }),
        ),
    ]);
    assert_eq!(
        c.execute("SELECT $a < $b AS value", &p).unwrap().rows,
        vec![vec![Value::Null]]
    );
    let p = Parameters::from([
        (
            "$a".into(),
            Value::Record(Record {
                table: "docs".into(),
                key: Key::Integer(1),
            }),
        ),
        ("$b".into(), Value::Integer(1)),
    ]);
    assert!(c.execute("SELECT $a < $b AS value", &p).is_err());
}

#[test]
fn typed_scalar_ranges_keep_native_numeric_null_and_text_rules() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(a,b)");
    for (a, b) in [
        ("1", "1.0"),
        ("9223372036854775807", "9.223372036854776e18"),
        ("null", "2"),
        ("true", "0"),
        ("'a'", "'B'"),
        ("'2'", "2"),
    ] {
        query(&c, &format!("INSERT INTO docs {{a:{a},b:{b}}}"));
        query(&c, &format!("INSERT INTO baseline VALUES ({a},{b})"));
    }
    for op in ["<", "<=", ">", ">="] {
        let sql = |table: &str| format!("SELECT a {op} b AS value FROM {table} ORDER BY value");
        assert_eq!(
            query(&c, &sql("docs")).rows,
            query(&c, &sql("baseline")).rows,
            "{op}"
        );
    }
}

#[test]
fn typed_between_matches_record_ranges_and_native_null_logic() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    for key in [1, 2, 10, 20] {
        c.execute(
            "INSERT INTO docs (id) VALUES (type::record('docs',$key))",
            &Parameters::from([("$key".into(), Value::Integer(key))]),
        )
        .unwrap();
    }
    assert_eq!(query(&c,"SELECT record::id(id) AS key FROM docs WHERE id BETWEEN docs:2 AND docs:10 ORDER BY id").rows,vec![vec![Value::Integer(2)],vec![Value::Integer(10)]]);
    assert_eq!(query(&c,"SELECT record::id(id) AS key FROM docs WHERE id NOT BETWEEN docs:2 AND docs:10 ORDER BY id").rows,vec![vec![Value::Integer(1)],vec![Value::Integer(20)]]);
    query(&c, "BEGIN");
    assert_eq!(query(&c,"DELETE FROM docs WHERE id BETWEEN docs:2 AND docs:10 RETURNING id BETWEEN docs:2 AND docs:10 AS matched").rows,vec![vec![Value::Integer(1)],vec![Value::Integer(1)]]);
    query(&c, "ROLLBACK");
    assert_eq!(
        query(&c, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(4)]]
    );
    query(&c, "CREATE TABLE values_doc");
    query(&c, "CREATE TABLE baseline(a,b,c)");
    for (a, b, c_) in [
        ("null", "1", "2"),
        ("0", "1", "null"),
        ("3", "null", "2"),
        ("1", "null", "2"),
        ("1", "0", "null"),
        ("1", "0", "2"),
        ("1.0", "true", "2"),
        ("'a'", "'A'", "'z'"),
    ] {
        query(
            &c,
            &format!("INSERT INTO values_doc {{a:{a},b:{b},c:{c_}}}"),
        );
        query(&c, &format!("INSERT INTO baseline VALUES ({a},{b},{c_})"));
    }
    for not in ["", "NOT "] {
        let sql = |table: &str| {
            format!("SELECT a {not}BETWEEN b AND c AS value FROM {table} ORDER BY value")
        };
        assert_eq!(
            query(&c, &sql("values_doc")).rows,
            query(&c, &sql("baseline")).rows,
            "{not}"
        );
    }
}

#[test]
fn managed_membership_plans_preserve_results_and_residual_predicates() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    for (key, score) in [("1", 1), ("1.0", 2), ("2", 3), ("null", 4), ("'1'", 5)] {
        query(&c, &format!("INSERT INTO docs {{key:{key},score:{score}}}"));
    }
    let tails = [
        "key IN (1,2,1,NULL)",
        "(key IN ((1),2)) AND score>1",
        "((key) = (1)) AND score>1",
        "key NOT IN (1,NULL)",
        "key IN ()",
        "key IN (1) OR score=5",
    ];
    let sql = |tail: &str| format!("SELECT score FROM docs WHERE {tail} ORDER BY score");
    let before = tails
        .iter()
        .map(|tail| query(&c, &sql(tail)).rows)
        .collect::<Vec<_>>();
    query(&c, "CREATE INDEX docs_key ON docs(key)");
    for (i, tail) in tails.iter().enumerate() {
        assert_eq!(query(&c, &sql(tail)).rows, before[i], "{tail}");
        let plan = query(&c, &format!("EXPLAIN QUERY PLAN {}", sql(tail)));
        let plan = format!("{:?}", plan.rows);
        if i < 3 {
            assert!(plan.contains("SEARCH i"), "{tail}: {plan}");
        }
    }
    let params = Parameters::from([
        ("$a".into(), Value::Integer(1)),
        ("$b".into(), Value::Integer(2)),
    ]);
    assert_eq!(
        c.execute(
            "SELECT score FROM docs WHERE key IN ($a,$b) ORDER BY score",
            &params
        )
        .unwrap()
        .rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(3)]
        ]
    );
    query(&c, "BEGIN");
    assert_eq!(
        query(
            &c,
            "DELETE FROM docs WHERE (key IN (1,2)) AND score>1 RETURNING score"
        )
        .rows
        .len(),
        2
    );
    query(&c, "ROLLBACK");
    assert_eq!(query(&c, &sql(tails[0])).rows, before[0]);
}

#[test]
fn managed_membership_preserves_typed_record_identity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "INSERT INTO docs {id:docs:a,ref:docs:1}");
    query(&c, "INSERT INTO docs {id:docs:b,ref:docs:`1`}");
    query(&c, "INSERT INTO docs {id:docs:c,ref:docs:2}");
    let sql = "SELECT id FROM docs WHERE ref IN (type::record('DOCS',1),docs:2,docs:2) ORDER BY id";
    let before = query(&c, sql).rows;
    assert_eq!(before.len(), 2);
    query(&c, "CREATE INDEX docs_ref ON docs(ref)");
    assert_eq!(query(&c, sql).rows, before);
    assert!(
        format!("{:?}", query(&c, &format!("EXPLAIN QUERY PLAN {sql}")).rows).contains("SEARCH i")
    );
}

#[test]
fn managed_null_filters_preserve_missing_values_and_outer_join_results() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "INSERT INTO docs {id:docs:a,marker:1}");
    query(&c, "INSERT INTO docs {id:docs:b,marker:2,key:null}");
    query(&c, "INSERT INTO docs {id:docs:c,marker:3,key:1}");
    query(&c, "INSERT INTO docs {id:docs:d,marker:4,key:'value'}");
    query(&c, "CREATE TABLE ordinary(marker INTEGER)");
    query(&c, "INSERT INTO ordinary VALUES (1),(2),(3),(4),(5)");
    let tails = [
        "key IS NULL",
        "((key) IS NULL) AND marker>1",
        "key IS NOT NULL",
        "key NOTNULL",
        "key=NULL",
        "key ISNULL",
        "NULL IS key",
        "NULL IS NOT key",
    ];
    let sql = |tail: &str| format!("SELECT marker FROM docs WHERE {tail} ORDER BY marker");
    let before = tails
        .iter()
        .map(|tail| query(&c, &sql(tail)).rows)
        .collect::<Vec<_>>();
    let join="SELECT n.marker FROM docs d RIGHT JOIN ordinary n ON d.marker=n.marker WHERE d.key IS NULL ORDER BY n.marker";
    let joined = query(&c, join).rows;
    assert_eq!(
        joined,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(5)]
        ]
    );
    query(&c, "CREATE INDEX docs_key ON docs(key)");
    for (i, tail) in tails.iter().enumerate() {
        assert_eq!(query(&c, &sql(tail)).rows, before[i], "{tail}");
        if i < 2 {
            let plan = format!(
                "{:?}",
                query(&c, &format!("EXPLAIN QUERY PLAN {}", sql(tail))).rows
            );
            // Pinned Turso scans native IS NULL indexes too. The useful
            // restriction is applied to compact keys before fetching documents.
            assert!(
                plan.contains(" AS i") && (plan.contains("SEARCH c") || plan.contains("HASH JOIN")),
                "{plan}"
            );
        }
    }
    assert_eq!(query(&c, join).rows, joined);
    query(&c, "BEGIN");
    assert_eq!(
        query(
            &c,
            "DELETE FROM docs WHERE key IS NULL AND marker>1 RETURNING marker"
        )
        .rows,
        vec![vec![Value::Integer(2)]]
    );
    query(&c, "ROLLBACK");
    assert_eq!(query(&c, &sql(tails[0])).rows, before[0]);
    query(&c, "UPDATE docs SET key=9 WHERE key IS NULL");
    assert!(query(&c, &sql(tails[0])).rows.is_empty());
    assert_eq!(query(&c, &sql(tails[2])).rows.len(), 4);
}

#[test]
fn binary_literals_match_typed_fields_and_managed_index_keys() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(
        &c,
        "INSERT INTO docs (id,data) VALUES (docs:a,X'0102'),(docs:b,X'03'),(docs:c,NULL)",
    );
    let tails = [
        "data=X'0102'",
        "(X'0102')=data",
        "data IS X'0102'",
        "data!=X'0102'",
        "data IS NOT X'0102'",
        "data IN (X'0102',X'03',X'0102',NULL)",
        "data NOT IN (X'0102',X'03')",
    ];
    let sql = |tail: &str| format!("SELECT id FROM docs WHERE {tail} ORDER BY id");
    let expected = [
        vec!["a"],
        vec!["a"],
        vec!["a"],
        vec!["b"],
        vec!["b", "c"],
        vec!["a", "b"],
        vec![],
    ];
    for indexed in [false, true] {
        if indexed {
            query(&c, "CREATE INDEX docs_data ON docs(data)");
        }
        for (tail, keys) in tails.iter().zip(&expected) {
            let rows = keys
                .iter()
                .map(|key| {
                    vec![Value::Record(Record {
                        table: "docs".into(),
                        key: Key::String((*key).into()),
                    })]
                })
                .collect::<Vec<_>>();
            assert_eq!(query(&c, &sql(tail)).rows, rows, "{indexed}: {tail}");
        }
    }
    for tail in [tails[0], tails[5]] {
        let plan = query(&c, &format!("EXPLAIN QUERY PLAN {}", sql(tail)));
        assert!(format!("{:?}", plan.rows).contains("SEARCH i"));
    }
    let p = Parameters::from([("$data".into(), Value::Binary(vec![1, 2]))]);
    assert_eq!(
        c.execute("SELECT $data=X'0102' AS same FROM docs LIMIT 1", &p)
            .unwrap()
            .rows,
        vec![vec![Value::Integer(1)]]
    );
    query(&c, "BEGIN");
    assert_eq!(
        query(
            &c,
            "DELETE FROM docs WHERE data=X'0102' RETURNING data=X'0102' AS matched"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    query(&c, "ROLLBACK");
    assert_eq!(
        c.lookup_index("docs", "docs_data", &Value::Binary(vec![1, 2]))
            .unwrap()
            .len(),
        1
    );
    // Raw bytes that imitate a record encoding remain a binary value.
    let bytes =
        b"FDB\x01{\"type\":\"Record\",\"value\":{\"table\":\"docs\",\"key\":{\"String\":\"a\"}}}";
    let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert!(query(&c, &format!("SELECT id FROM docs WHERE id=X'{hex}'"))
        .rows
        .is_empty());
}

#[test]
fn binary_payload_order_and_ranges_match_native_blobs() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(data BLOB)");
    for hex in ["", "00", "02", "0200", "0A", "FF"] {
        query(&c, &format!("INSERT INTO docs (data) VALUES (X'{hex}')"));
        query(&c, &format!("INSERT INTO baseline VALUES (X'{hex}')"));
    }
    for distinct in ["", "DISTINCT "] {
        for tail in [
            "ORDER BY data",
            "ORDER BY data DESC",
            "WHERE data < X'0A' ORDER BY data",
            "WHERE data >= (X'02') ORDER BY data",
            "WHERE data BETWEEN X'02' AND X'0A' ORDER BY data",
            "WHERE data NOT BETWEEN X'02' AND X'0A' ORDER BY data",
        ] {
            let sql = |table: &str| format!("SELECT {distinct}data FROM {table} {tail}");
            assert_eq!(
                query(&c, &sql("docs")).rows,
                query(&c, &sql("baseline")).rows,
                "{distinct}{tail}"
            );
        }
    }
    assert_eq!(
        query(
            &c,
            "SELECT (X'02')=X'02' AS same,X'02'<X'0A' AS smaller FROM docs LIMIT 1"
        )
        .rows,
        vec![vec![Value::Integer(1), Value::Integer(1)]]
    );
    assert_eq!(
        query(&c, "SELECT X'02' AS bytes FROM docs LIMIT 1").rows,
        vec![vec![Value::Binary(vec![2])]]
    );
    assert_eq!(
        query(
            &c,
            "SELECT X'02' IN (X'02',X'03') AS member FROM docs LIMIT 1"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    query(&c, "CREATE INDEX docs_data ON docs(data)");
    assert_eq!(
        query(
            &c,
            "SELECT data FROM docs WHERE data IN (X'02',X'0A') ORDER BY data"
        )
        .rows,
        vec![vec![Value::Binary(vec![2])], vec![Value::Binary(vec![10])]]
    );
    query(&c, "BEGIN");
    assert_eq!(
        query(
            &c,
            "DELETE FROM docs WHERE data BETWEEN X'02' AND X'0A' RETURNING data"
        )
        .rows
        .len(),
        3
    );
    query(&c, "ROLLBACK");
    assert_eq!(
        query(&c, "SELECT data FROM docs ORDER BY data").rows,
        query(&c, "SELECT data FROM baseline ORDER BY data").rows
    );
}

#[test]
fn binary_membership_normalizes_literals_fields_and_function_results() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(data BLOB)");
    for hex in ["", "02", "0A"] {
        query(&c, &format!("INSERT INTO docs (data) VALUES (X'{hex}')"));
        query(&c, &format!("INSERT INTO baseline VALUES (X'{hex}')"));
    }
    query(&c, "INSERT INTO docs (data) VALUES (NULL)");
    query(&c, "INSERT INTO baseline VALUES (NULL)");
    let p = Parameters::from([("$bytes".into(), Value::Binary(vec![2]))]);
    for predicate in [
        "X'02' IN (data)",
        "(X'02') NOT IN (data,NULL)",
        "X'02' IN (data,X'02',substr(X'0203',1,1))",
        "data IN (substr(X'0203',1,1),X'0A')",
        "data NOT IN (substr(X'0203',1,1),NULL)",
        "X'02' IN ($bytes)",
        "$bytes IN (data,substr(X'0203',1,1))",
        "X'02' IN ()",
    ] {
        let p = if predicate.contains("$bytes") {
            p.clone()
        } else {
            Parameters::new()
        };
        let sql = |table| format!("SELECT {predicate} AS matched FROM {table} ORDER BY data");
        assert_eq!(
            c.execute(&sql("docs"), &p).unwrap().rows,
            c.execute(&sql("baseline"), &p).unwrap().rows,
            "{predicate}"
        );
    }
    assert_eq!(
        query(&c, "SELECT X'02' IN (docs:a) AS matched FROM docs LIMIT 1").rows,
        vec![vec![Value::Integer(0)]]
    );
    let bytes =
        b"FDB\x01{\"type\":\"Record\",\"value\":{\"table\":\"docs\",\"key\":{\"String\":\"a\"}}}";
    let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(
        query(
            &c,
            &format!("SELECT X'{hex}' IN (docs:a) AS matched FROM docs LIMIT 1")
        )
        .rows,
        vec![vec![Value::Integer(0)]]
    );
    query(&c, "CREATE INDEX docs_data ON docs(data)");
    query(&c, "BEGIN");
    assert_eq!(
        query(&c, "DELETE FROM docs WHERE X'02' IN (data) RETURNING data").rows,
        vec![vec![Value::Binary(vec![2])]]
    );
    query(&c, "ROLLBACK");
    assert_eq!(
        c.lookup_index("docs", "docs_data", &Value::Binary(vec![2]))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn native_function_membership_preserves_binary_and_collation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(
        &c,
        "CREATE TABLE baseline(data BLOB, label TEXT, number INTEGER)",
    );
    for values in ["X'02','A',2", "X'0A','b',3", "NULL,NULL,NULL"] {
        query(
            &c,
            &format!("INSERT INTO docs (data,label,number) VALUES ({values})"),
        );
        query(&c, &format!("INSERT INTO baseline VALUES ({values})"));
    }
    for predicate in [
        "substr(X'0203',1,1) IN (data)",
        "(substr(X'0203',1,1)) NOT IN (data,NULL)",
        "substr(X'0203',1,1) IN (data,X'02')",
        "lower('A') COLLATE NOCASE IN (label)",
        "(lower('A') COLLATE NOCASE) NOT IN (label,NULL)",
        "abs(-2) IN (number,3)",
        "abs(-2) IN ('2')",
        "CAST('2' AS INTEGER) IN ('2')",
        "CAST('a' AS TEXT) COLLATE NOCASE IN ('A')",
    ] {
        let sql = |table| format!("SELECT {predicate} AS matched FROM {table} ORDER BY data");
        assert_eq!(
            query(&c, &sql("docs")).rows,
            query(&c, &sql("baseline")).rows,
            "{predicate}"
        );
    }
    let bytes =
        b"FDB\x01{\"type\":\"Record\",\"value\":{\"table\":\"docs\",\"key\":{\"String\":\"a\"}}}";
    let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(
        query(
            &c,
            &format!("SELECT substr(X'{hex}',1) IN (docs:a) AS matched FROM docs LIMIT 1")
        )
        .rows,
        vec![vec![Value::Integer(0)]]
    );
}

#[test]
fn cast_and_scalar_membership_keep_native_affinity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(data BLOB, number, label)");
    for values in ["X'32',2,'A'", "X'33','2','b'", "NULL,NULL,NULL"] {
        query(
            &c,
            &format!("INSERT INTO docs (data,number,label) VALUES ({values})"),
        );
        query(&c, &format!("INSERT INTO baseline VALUES ({values})"));
    }
    for predicate in [
        "CAST('2' AS BLOB) IN (data)",
        "CAST('2' AS BLOB) NOT IN (data,NULL)",
        "+X'32' IN (data)",
        "+data IN (X'32')",
        "data IN (+data)",
        "X'32' IN (+data)",
        "'a' IN (label COLLATE NOCASE)",
        "CAST('2' AS REAL) IN (number)",
        "CAST('2' AS NUMERIC) IN (number)",
        "+CAST('2' AS INTEGER) IN ('2')",
        "CAST('2' AS INTEGER) IN (number)",
        "CAST('2' AS INTEGER) IN ('2')",
        "CAST(2 AS TEXT) IN (number)",
        "CAST('a' COLLATE NOCASE AS TEXT) IN (label)",
        "CAST('a' AS TEXT) COLLATE NOCASE IN (label)",
        "(1+1) IN (number)",
        "2 IN (number)",
        "NULL IN (data)",
        "CAST(data AS TEXT) IN ('2')",
    ] {
        let sql = |table| format!("SELECT {predicate} AS matched FROM {table} ORDER BY data");
        assert_eq!(
            query(&c, &sql("docs")).rows,
            query(&c, &sql("baseline")).rows,
            "{predicate}"
        );
    }
}

#[test]
fn binary_predicate_contexts_use_native_truth_conversion() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(data BLOB)");
    for hex in ["31", "30", "2D32", "", "6162"] {
        query(&c, &format!("INSERT INTO docs (data) VALUES (X'{hex}')"));
        query(&c, &format!("INSERT INTO baseline VALUES (X'{hex}')"));
    }
    query(&c, "INSERT INTO docs (data) VALUES (NULL)");
    query(&c, "INSERT INTO baseline VALUES (NULL)");
    for template in [
        "SELECT data FROM SOURCE WHERE data ORDER BY data",
        "SELECT CASE WHEN data THEN 1 ELSE 0 END AS truth FROM SOURCE ORDER BY data",
        "SELECT data FROM SOURCE GROUP BY data HAVING data ORDER BY data",
        "SELECT count(*) FILTER (WHERE data) AS total FROM SOURCE",
        "SELECT sum(1) FILTER (WHERE data) AS total FROM SOURCE",
        "SELECT count(*) AS total FROM SOURCE a JOIN SOURCE b ON a.data",
        "SELECT count(*) AS total FROM SOURCE a LEFT JOIN SOURCE b ON a.data",
    ] {
        assert_eq!(
            query(&c, &template.replace("SOURCE", "docs")).rows,
            query(&c, &template.replace("SOURCE", "baseline")).rows,
            "{template}"
        );
    }
    query(&c, "CREATE INDEX docs_data ON docs(data)");
    query(&c, "BEGIN");
    assert_eq!(
        query(&c, "DELETE FROM docs WHERE data RETURNING data")
            .rows
            .len(),
        2
    );
    query(&c, "ROLLBACK");
    assert_eq!(
        c.lookup_index("docs", "docs_data", &Value::Binary(b"1".to_vec()))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn binary_equality_expressions_match_native_blobs_and_affinity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(data BLOB)");
    for hex in ["31", "32", "", "61"] {
        query(&c, &format!("INSERT INTO docs (data) VALUES (X'{hex}')"));
        query(&c, &format!("INSERT INTO baseline VALUES (X'{hex}')"));
    }
    query(&c, "INSERT INTO docs (data) VALUES (NULL)");
    query(&c, "INSERT INTO baseline VALUES (NULL)");
    for expr in [
        "substr(data,1,1)=data",
        "data!=substr(X'3132',1,1)",
        "CAST('1' AS BLOB)=data",
        "data IS substr(data,1,1)",
        "data IS NOT substr(data,1,1)",
        "CAST(data AS INTEGER)='1'",
        "CAST(data AS TEXT) COLLATE NOCASE='A'",
        "(data+0)=1",
        "CASE WHEN data=X'31' THEN data ELSE X'' END=data",
    ] {
        let sql = |table| format!("SELECT {expr} AS matched FROM {table} ORDER BY data");
        assert_eq!(
            query(&c, &sql("docs")).rows,
            query(&c, &sql("baseline")).rows,
            "{expr}"
        );
    }
    query(&c, "CREATE INDEX docs_data ON docs(data)");
    query(&c, "BEGIN");
    assert_eq!(
        query(
            &c,
            "DELETE FROM docs WHERE data=substr(X'3132',1,1) RETURNING data"
        )
        .rows,
        vec![vec![Value::Binary(b"1".to_vec())]]
    );
    query(&c, "ROLLBACK");
    assert_eq!(
        c.lookup_index("docs", "docs_data", &Value::Binary(b"1".to_vec()))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn binary_pattern_operands_match_native_blob_behavior() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(data BLOB, pattern BLOB)");
    for values in [
        "X'616263',X'6125'",
        "X'612562',X'61212562'",
        "X'',X'25'",
        "NULL,X'25'",
    ] {
        query(
            &c,
            &format!("INSERT INTO docs (data,pattern) VALUES ({values})"),
        );
        query(&c, &format!("INSERT INTO baseline VALUES ({values})"));
    }
    for expr in [
        "data LIKE 'a%'",
        "data NOT LIKE 'a%'",
        "data GLOB 'a*'",
        "data NOT GLOB 'a*'",
        "data LIKE pattern",
        "data LIKE pattern ESCAPE '!'",
        "'abc' LIKE pattern",
        "data LIKE 'a!%b' ESCAPE X'21'",
    ] {
        let sql = |table| format!("SELECT {expr} AS matched FROM {table} ORDER BY data");
        assert_eq!(
            query(&c, &sql("docs")).rows,
            query(&c, &sql("baseline")).rows,
            "{expr}"
        );
    }
    query(&c, "BEGIN");
    let expected = query(&c, "SELECT data FROM baseline WHERE data GLOB 'a*'")
        .rows
        .len();
    assert_eq!(
        query(&c, "DELETE FROM docs WHERE data GLOB 'a*' RETURNING data")
            .rows
            .len(),
        expected
    );
    query(&c, "ROLLBACK");
}

#[test]
fn native_binary_aliases_keep_comparison_keys_in_having_and_order() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(data BLOB)");
    for hex in ["31", "32", ""] {
        query(&c, &format!("INSERT INTO docs (data) VALUES (X'{hex}')"));
        query(&c, &format!("INSERT INTO baseline VALUES (X'{hex}')"));
    }
    for template in [
        "SELECT substr(data,1,1) AS part FROM SOURCE GROUP BY data HAVING part=data ORDER BY data",
        "SELECT substr(data,1,1) AS part FROM SOURCE GROUP BY data HAVING part IN (data) ORDER BY data",
        "SELECT CAST(data AS INTEGER) AS number FROM SOURCE GROUP BY data HAVING number='1' ORDER BY data",
        "SELECT substr(data,1,1) AS part FROM SOURCE ORDER BY part=X'31',data",
        "SELECT CAST(data AS TEXT) COLLATE NOCASE AS word FROM SOURCE GROUP BY data HAVING word='1' ORDER BY data",
    ] {
        assert_eq!(query(&c, &template.replace("SOURCE", "docs")).rows,
                   query(&c, &template.replace("SOURCE", "baseline")).rows, "{template}");
    }
}

#[test]
fn where_aliases_resolve_source_expressions_and_keep_index_candidates() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(city TEXT,amount INTEGER)");
    for (city, amount) in [("A", 1), ("B", 2)] {
        query(
            &c,
            &format!("INSERT INTO docs {{city:'{city}',amount:{amount},place:'stored'}}"),
        );
        query(
            &c,
            &format!("INSERT INTO baseline VALUES ('{city}',{amount})"),
        );
    }
    for template in [
        "SELECT city AS place FROM SOURCE WHERE place='A'",
        "SELECT lower(city) AS place FROM SOURCE WHERE place='a'",
        "SELECT amount+1 AS next FROM SOURCE WHERE next>2",
        "SELECT 7 AS key FROM SOURCE WHERE key=7 ORDER BY city",
        "SELECT city AS place FROM SOURCE WHERE place IN ('A','B') ORDER BY place",
    ] {
        assert_eq!(
            query(&c, &template.replace("SOURCE", "docs")).rows,
            query(&c, &template.replace("SOURCE", "baseline")).rows,
            "{template}"
        );
    }
    assert_eq!(
        query(
            &c,
            "SELECT city AS place FROM docs d WHERE d.place='stored'"
        )
        .rows
        .len(),
        2
    );
    query(&c, "CREATE INDEX docs_city ON docs(city)");
    let plan = query(
        &c,
        "EXPLAIN QUERY PLAN SELECT city AS place FROM docs WHERE place='A'",
    );
    assert!(format!("{:?}", plan.rows).contains("SEARCH i"));
    assert!(c
        .execute(
            "SELECT sum(amount) AS total FROM docs WHERE total>1",
            &Parameters::new()
        )
        .is_err());
    query(&c, "CREATE TABLE results");
    query(&c, "BEGIN");
    query(
        &c,
        "INSERT INTO results (place) SELECT city AS place FROM docs WHERE place='A'",
    );
    assert_eq!(
        query(&c, "SELECT place FROM results").rows,
        vec![vec![Value::String("A".into())]]
    );
    query(&c, "ROLLBACK");
    assert!(query(&c, "SELECT place FROM results").rows.is_empty());
}

#[test]
fn join_predicates_resolve_projection_aliases_before_typed_lowering() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(city TEXT,amount INTEGER)");
    query(&c, "CREATE TABLE matches(city TEXT,n INTEGER)");
    query(&c, "INSERT INTO matches VALUES ('a',1),('b',2)");
    for (city, amount) in [("A", 1), ("B", 2), ("C", 3)] {
        query(
            &c,
            &format!("INSERT INTO docs {{city:'{city}',amount:{amount},place:'stored'}}"),
        );
        query(
            &c,
            &format!("INSERT INTO baseline VALUES ('{city}',{amount})"),
        );
    }
    for template in [
        "SELECT lower(d.city) AS place,m.n FROM SOURCE d JOIN matches m ON place=m.city ORDER BY d.city",
        "SELECT lower(d.city) AS place,m.n FROM SOURCE d LEFT JOIN matches m ON PLACE=m.city ORDER BY d.city",
        "SELECT d.amount+1 AS next,m.n FROM SOURCE d JOIN matches m ON next=m.n ORDER BY d.city",
        "SELECT lower(d.city) AS place,m.n FROM SOURCE d LEFT JOIN matches m ON place=m.city AND m.n>1 ORDER BY d.city",
        "SELECT 1 AS key,m.n FROM SOURCE d JOIN matches m ON key=m.n ORDER BY d.city",
    ] {
        assert_eq!(query(&c, &template.replace("SOURCE", "docs")).rows,
            query(&c, &template.replace("SOURCE", "baseline")
                .replace("ON place=", "ON lower(d.city)=")
                .replace("ON PLACE=", "ON lower(d.city)=")
                .replace("ON next=", "ON d.amount+1=")
                .replace("ON key=", "ON 1=")).rows, "{template}");
    }
    assert_eq!(query(&c, "SELECT d.city AS place,m.n FROM docs d JOIN matches m ON d.place='stored' AND d.amount=m.n ORDER BY d.city").rows.len(), 2);
    assert!(c
        .execute(
            "SELECT lower(d.city) AS place FROM baseline d JOIN matches m ON place=m.city",
            &Parameters::new()
        )
        .is_err());
    query(&c, "CREATE TABLE links");
    query(&c, "CREATE TABLE refs");
    query(
        &c,
        "INSERT INTO links (id,data,ref) VALUES (links:a,X'31',refs:a)",
    );
    query(&c, "INSERT INTO refs (id,data) VALUES (refs:a,X'31')");
    assert_eq!(
        query(
            &c,
            "SELECT l.ref AS target,r.data FROM links l JOIN refs r ON target=r.id"
        )
        .rows,
        vec![vec![
            Value::Record(Record {
                table: "refs".into(),
                key: Key::String("a".into())
            }),
            Value::Binary(vec![49])
        ]]
    );
    assert_eq!(
        query(
            &c,
            "SELECT l.data AS bytes,r.id FROM links l JOIN refs r ON bytes=r.data"
        )
        .rows
        .len(),
        1
    );
    for sql in [
        "SELECT sum(d.amount) AS total FROM docs d JOIN matches m ON total=m.n",
        "SELECT record::fetch(l.ref) AS target FROM links l JOIN refs r ON target=r.id",
        "SELECT d.city AS place FROM docs d JOIN matches m ON city=m.city",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    query(&c, "CREATE TABLE copied(city TEXT,n INTEGER)");
    query(&c, "BEGIN");
    query(&c, "INSERT INTO copied SELECT lower(d.city) AS place,m.n FROM docs d JOIN matches m ON place=m.city");
    assert_eq!(
        query(&c, "SELECT count(*) FROM copied").rows,
        vec![vec![Value::Integer(2)]]
    );
    query(&c, "ROLLBACK");
    assert!(query(&c, "SELECT * FROM copied").rows.is_empty());
}

#[test]
fn quoted_internal_functions_are_rejected_across_collection_queries_and_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "INSERT INTO docs {id:docs:a,n:1}");
    for sql in [
        "SELECT '__romedb_pack'(1) FROM docs",
        "SELECT '__ROMEDB_PACK' /* comment */ (1) FROM docs",
        "SELECT n FROM docs WHERE '__romedb_pack'(1) IS NOT NULL",
        "SELECT n FROM docs ORDER BY '__romedb_pack'(n)",
        "INSERT INTO docs (id,n) VALUES (docs:b,'__romedb_pack'(1))",
        "INSERT INTO docs (n) SELECT '__romedb_pack'(1) FROM docs",
        "UPDATE docs SET n='__romedb_pack'(1)",
        "DELETE FROM docs WHERE '__romedb_pack'(1) IS NOT NULL",
        "UPDATE docs SET n=2 RETURNING '__romedb_pack'(n)",
        "INSERT INTO docs {id:docs:b,n:2} RETURNING '__romedb_pack'(n)",
        "UPDATE docs:a {n:2} RETURNING '__romedb_pack'(n)",
        "DELETE docs:a RETURNING '__romedb_pack'(n)",
    ] {
        assert_eq!(
            c.execute(sql, &Parameters::new()).unwrap_err().code(),
            "FDB_UNSUPPORTED",
            "{sql}"
        );
        assert_eq!(
            query(&c, "SELECT n FROM docs").rows,
            vec![vec![Value::Integer(1)]],
            "{sql}"
        );
    }
    assert_eq!(
        query(
            &c,
            "SELECT '__romedb_pack',record::id(id),lower('__romedb_literal') FROM docs"
        )
        .rows,
        vec![vec![
            Value::String("__romedb_pack".into()),
            Value::String("a".into()),
            Value::String("__romedb_literal".into())
        ]]
    );
    query(&c, "UPDATE docs SET label='__romedb_pack' WHERE id=docs:a");
    assert_eq!(
        query(&c, "UPDATE docs:a {n:2} RETURNING '__romedb_pack',n").rows,
        vec![vec![
            Value::String("__romedb_pack".into()),
            Value::Integer(2)
        ]]
    );
}

#[test]
fn mixed_native_columns_preserve_binary_equality_affinity_and_collation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(
        &c,
        "CREATE TABLE native(value BLOB,n INTEGER,t TEXT COLLATE NOCASE)",
    );
    query(&c, "CREATE TABLE baseline(value BLOB,n TEXT,t TEXT)");
    for value in ["X'31'", "X'32'", "NULL"] {
        query(
            &c,
            &format!("INSERT INTO docs (value,n,t) VALUES ({value},'1','hello')"),
        );
        query(
            &c,
            &format!("INSERT INTO baseline VALUES ({value},'1','hello')"),
        );
    }
    query(
        &c,
        "INSERT INTO native VALUES (X'31',1,'HELLO'),(X'32',2,'other'),(NULL,3,NULL)",
    );
    for predicate in [
        "d.value=n.value",
        "n.value=d.value",
        "d.value<>n.value",
        "d.value IS n.value",
        "n.value IS NOT d.value",
        "d.n=n.n",
        "n.n=d.n",
        "d.t=n.t",
        "d.t COLLATE NOCASE=n.t",
        "d.t COLLATE BINARY=n.t COLLATE NOCASE",
        "n.t=d.t",
        "d.value=(n.value COLLATE BINARY)",
    ] {
        let template = format!("SELECT count(*) FROM SOURCE d JOIN native n ON {predicate}");
        assert_eq!(
            query(&c, &template.replace("SOURCE", "docs")).rows,
            query(&c, &template.replace("SOURCE", "baseline")).rows,
            "{predicate}"
        );
    }
    assert_eq!(
        query(
            &c,
            "SELECT count(*) FROM docs d LEFT JOIN native n ON d.value=n.value WHERE n.n IS NULL"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    query(&c, "CREATE TABLE record_bytes(value BLOB)");
    let record = Value::Record(Record {
        table: "docs".into(),
        key: Key::String("a".into()),
    });
    c.execute(
        "INSERT INTO record_bytes VALUES ($value)",
        &Parameters::from([("$value".into(), record.clone())]),
    )
    .unwrap();
    let bytes = query(&c, "SELECT value FROM record_bytes").rows[0][0].clone();
    query(&c, "CREATE TABLE typed");
    c.execute(
        "INSERT INTO typed (ref,data) VALUES ($ref,$data)",
        &Parameters::from([("$ref".into(), record), ("$data".into(), bytes)]),
    )
    .unwrap();
    assert_eq!(query(&c, "SELECT t.ref=r.value,t.data=r.value,r.value=t.ref,r.value=t.data FROM typed t JOIN record_bytes r ON 1").rows,
        vec![vec![Value::Integer(0),Value::Integer(1),Value::Integer(0),Value::Integer(1)]]);
    query(&c, "CREATE TABLE copied(value BLOB)");
    query(&c, "BEGIN");
    query(
        &c,
        "INSERT INTO copied SELECT d.value FROM docs d JOIN native n ON d.value=n.value",
    );
    assert_eq!(
        query(&c, "SELECT hex(value) FROM copied ORDER BY value").rows,
        vec![
            vec![Value::String("31".into())],
            vec![Value::String("32".into())]
        ]
    );
    query(&c, "ROLLBACK");
    assert!(query(&c, "SELECT * FROM copied").rows.is_empty());
}

#[test]
fn mixed_native_column_aliases_keep_projection_identity_in_having() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "INSERT INTO docs (value) VALUES (X'31'),(X'32')");
    query(
        &c,
        "CREATE TABLE native(value BLOB,picked BLOB,n INTEGER,t TEXT COLLATE NOCASE)",
    );
    query(
        &c,
        "INSERT INTO native VALUES (X'31',X'99',1,'HELLO'),(X'32',X'99',2,'other')",
    );
    for predicate in [
        "picked=X'31'",
        "X'31'=picked",
        "(picked COLLATE BINARY) IS X'31'",
    ] {
        let sql=format!("SELECT n.value AS picked,count(*) FROM docs d JOIN native n ON d.value=n.value GROUP BY n.value HAVING {predicate}");
        assert_eq!(
            query(&c, &sql).rows,
            vec![vec![Value::Binary(vec![49]), Value::Integer(1)]],
            "{sql}"
        );
    }
    assert!(query(&c,"SELECT n.value AS picked,count(*) FROM docs d JOIN native n ON d.value=n.value GROUP BY n.value HAVING n.picked=X'31'").rows.is_empty());
    assert_eq!(query(&c,"SELECT n.n AS picked,count(*) FROM docs d JOIN native n ON d.value=n.value GROUP BY n.n HAVING picked='1'").rows,vec![vec![Value::Integer(1),Value::Integer(1)]]);
    assert_eq!(query(&c,"SELECT n.t AS picked,count(*) FROM docs d JOIN native n ON d.value=n.value GROUP BY n.t HAVING picked='hello'").rows,vec![vec![Value::String("HELLO".into()),Value::Integer(1)]]);
}

#[test]
fn mixed_native_membership_preserves_binary_keys_and_lhs_affinity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(value BLOB,n TEXT,t TEXT)");
    query(
        &c,
        "CREATE TABLE native(value BLOB,n INTEGER,t TEXT COLLATE NOCASE)",
    );
    for value in ["X'31'", "X'32'", "NULL"] {
        query(
            &c,
            &format!("INSERT INTO docs (value,n,t) VALUES ({value},'1','hello')"),
        );
        query(
            &c,
            &format!("INSERT INTO baseline VALUES ({value},'1','hello')"),
        );
    }
    query(
        &c,
        "INSERT INTO native VALUES (X'31',1,'HELLO'),(X'32',2,'other'),(NULL,3,NULL)",
    );
    for predicate in [
        "n.value IN (d.value)",
        "n.value NOT IN (d.value)",
        "n.value IN (d.value,X'99',NULL)",
        "n.value NOT IN (d.value,NULL)",
        "d.value IN (n.value)",
        "d.value NOT IN (n.value,X'99')",
        "n.n IN (d.n)",
        "n.n NOT IN (d.n,'2')",
        "n.t IN (d.t)",
        "(n.t COLLATE BINARY) IN (d.t)",
        "n.value IN ()",
        "n.value NOT IN ()",
    ] {
        let template =
            format!("SELECT n.n,{predicate} FROM SOURCE d JOIN native n ON 1 ORDER BY n.n,d.value");
        assert_eq!(
            query(&c, &template.replace("SOURCE", "docs")).rows,
            query(&c, &template.replace("SOURCE", "baseline")).rows,
            "{predicate}"
        );
    }
    assert_eq!(query(&c,"SELECT n.value AS picked,count(*) FROM docs d JOIN native n ON n.value IN (d.value) GROUP BY n.value HAVING picked IN (X'31')").rows,
        vec![vec![Value::Binary(vec![49]),Value::Integer(1)]]);
}

#[test]
fn mixed_native_ranges_preserve_payload_order_affinity_and_collation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(value BLOB,n,t)");
    query(
        &c,
        "CREATE TABLE native(value BLOB,n INTEGER,t TEXT COLLATE NOCASE)",
    );
    for value in ["X''", "X'02'", "X'0a'", "X'ff'", "NULL"] {
        query(
            &c,
            &format!("INSERT INTO docs (value,n,t) VALUES ({value},'10','alpha')"),
        );
        query(
            &c,
            &format!("INSERT INTO baseline VALUES ({value},'10','alpha')"),
        );
    }
    query(&c, "INSERT INTO native VALUES (X'02',2,'BETA'),(X'0a',10,'ALPHA'),(X'ff',20,'aardvark'),(NULL,NULL,NULL)");
    for op in ["<", "<=", ">", ">="] {
        for (left, right) in [
            ("d.value", "n.value"),
            ("n.value", "d.value"),
            ("d.value", "(+n.value)"),
            ("(+d.value)", "n.value"),
            ("n.value", "(d.value)"),
            ("+d.n", "n.n"),
            ("(n.value)", "d.value"),
            ("d.n", "n.n"),
            ("n.n", "d.n"),
            ("d.t", "n.t"),
            ("n.t", "d.t"),
            ("d.t", "n.t COLLATE BINARY"),
        ] {
            let predicate = format!("{left} {op} {right}");
            let template =
                format!("SELECT {predicate} FROM SOURCE d JOIN native n ON 1 ORDER BY d.value,n.n");
            // A collection field has no declared collation. The baseline's
            // physical column does, so explicitly select the native column's
            // NOCASE collation when neither operand has an explicit override.
            let baseline = if (left == "d.t" && right == "n.t") || (left == "n.t" && right == "d.t")
            {
                template.replace("d.t", "d.t COLLATE NOCASE")
            } else {
                template.clone()
            };
            assert_eq!(
                query(&c, &template.replace("SOURCE", "docs")).rows,
                query(&c, &baseline.replace("SOURCE", "baseline")).rows,
                "{predicate}"
            );
        }
    }
    assert_eq!(query(&c,"SELECT count(*) FROM docs d LEFT JOIN native n ON n.n=2 AND d.value<n.value WHERE n.n IS NULL").rows,vec![vec![Value::Integer(4)]]);
    for source in [
        "(SELECT value FROM docs)",
        "(WITH a AS (SELECT value FROM docs) SELECT value FROM a)",
    ] {
        assert_eq!(
            query(
                &c,
                &format!(
                    "SELECT count(*) FROM {source} d JOIN native n ON d.value<n.value WHERE n.n=20"
                )
            )
            .rows,
            vec![vec![Value::Integer(3)]]
        );
    }
    query(&c, "CREATE TABLE copied(value BLOB)");
    query(&c, "BEGIN");
    query(&c,"INSERT INTO copied SELECT d.value FROM docs d JOIN native n ON d.value<n.value WHERE n.n=20");
    assert_eq!(
        query(&c, "SELECT hex(value) FROM copied ORDER BY value").rows,
        vec![
            vec![Value::String("".into())],
            vec![Value::String("02".into())],
            vec![Value::String("0A".into())]
        ]
    );
    query(&c, "ROLLBACK");
    assert!(query(&c, "SELECT * FROM copied").rows.is_empty());
    query(&c, "CREATE TABLE refs");
    query(&c, "INSERT INTO refs {value:docs:a}");
    for op in ["<", "<=", ">", ">="] {
        for predicate in [
            format!("r.value {op} n.value"),
            format!("n.value {op} r.value"),
        ] {
            let error = c
                .execute(
                    &format!("SELECT {predicate} FROM refs r JOIN native n ON n.n=2"),
                    &Parameters::new(),
                )
                .unwrap_err();
            assert!(
                error.to_string().contains("mixed record/scalar ordering"),
                "{error}"
            );
            assert_eq!(
                query(
                    &c,
                    &format!("SELECT {predicate} FROM refs r JOIN native n ON n.n IS NULL")
                )
                .rows,
                vec![vec![Value::Null]]
            );
        }
    }
}

#[test]
fn native_column_between_document_bounds_matches_sql() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE bounds");
    query(&c, "CREATE TABLE baseline(lo,hi,nlo,nhi,tlo,thi)");
    query(
        &c,
        "CREATE TABLE native(k INTEGER,value BLOB,n INTEGER,t TEXT COLLATE NOCASE)",
    );
    for (lo, hi) in [
        ("X'02'", "X'ff'"),
        ("X''", "X'0a'"),
        ("NULL", "X'0a'"),
        ("X'0a'", "NULL"),
        ("X'ff'", "X'02'"),
    ] {
        query(&c,&format!("INSERT INTO bounds (lo,hi,nlo,nhi,tlo,thi) VALUES ({lo},{hi},'2','10','alpha','gamma')"));
        query(
            &c,
            &format!("INSERT INTO baseline VALUES ({lo},{hi},'2','10','alpha','gamma')"),
        );
    }
    query(&c,"INSERT INTO native VALUES (1,X'',1,'AARDVARK'),(2,X'02',2,'BETA'),(3,X'0a',10,'GAMMA'),(4,X'ff',11,'zeta'),(5,NULL,NULL,NULL)");
    for not in ["", "NOT "] {
        for (value, lo, hi) in [
            ("n.value", "d.lo", "d.hi"),
            ("n.value", "(+d.lo)", "(d.hi)"),
            ("n.n", "+d.nlo", "(+d.nhi)"),
            ("n.value", "d.lo", "X'0a'"),
            ("n.value", "X'02'", "d.hi"),
            ("n.n", "d.nlo", "d.nhi"),
            ("+n.n", "d.nlo", "d.nhi"),
            ("n.t", "d.tlo", "d.thi"),
            ("n.t COLLATE BINARY", "d.tlo", "d.thi"),
        ] {
            let sql=format!("SELECT n.k,{value} {not}BETWEEN {lo} AND {hi} AS result FROM SOURCE d JOIN native n ON 1 ORDER BY d.lo,d.hi,n.k");
            assert_eq!(
                query(&c, &sql.replace("SOURCE", "bounds")).rows,
                query(&c, &sql.replace("SOURCE", "baseline")).rows,
                "{sql}"
            );
        }
    }
    assert_eq!(query(&c,"SELECT count(*) FROM native n LEFT JOIN bounds d ON n.n BETWEEN d.nlo AND d.nhi WHERE d.nlo IS NULL").rows,vec![vec![Value::Integer(3)]]);
    assert_eq!(query(&c,"WITH d AS (SELECT nlo,nhi FROM bounds) SELECT count(*) FROM native n JOIN d ON n.n BETWEEN d.nlo AND d.nhi").rows,vec![vec![Value::Integer(10)]]);
    query(&c, "CREATE TABLE copied(n INTEGER CHECK(n<5))");
    query(&c, "BEGIN");
    let error = c.execute("INSERT INTO copied SELECT n.n FROM native n JOIN bounds d ON n.n BETWEEN d.nlo AND d.nhi", &Parameters::new()).unwrap_err();
    assert_eq!(error.code(), "FDB_CONSTRAINT");
    assert!(query(&c, "SELECT * FROM copied").rows.is_empty());
    query(&c, "INSERT INTO copied VALUES (1)");
    query(&c, "ROLLBACK");
    assert!(query(&c, "SELECT * FROM copied").rows.is_empty());
    query(&c, "CREATE TABLE refs");
    query(&c, "INSERT INTO refs {lo:bounds:a,hi:bounds:z}");
    for not in ["", "NOT "] {
        let sql =
            format!("SELECT n.value {not}BETWEEN d.lo AND d.hi FROM refs d JOIN native n ON n.k=1");
        assert!(c
            .execute(&sql, &Parameters::new())
            .unwrap_err()
            .to_string()
            .contains("mixed record/scalar ordering"));
        assert_eq!(
            query(&c, &sql.replace("n.k=1", "n.k=5")).rows,
            vec![vec![Value::Null]]
        );
    }
}

#[test]
fn collated_collection_ranges_resolve_fields_instead_of_native_columns() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(a,b)");
    for values in [
        "('alpha','BETA')",
        "('BETA','alpha')",
        "('alpha','ALPHA')",
        "(NULL,'alpha')",
    ] {
        query(&c, &format!("INSERT INTO docs(a,b) VALUES {values}"));
        query(&c, &format!("INSERT INTO baseline VALUES {values}"));
    }
    for op in ["<", "<=", ">", ">="] {
        for (left, right) in [
            ("a", "b COLLATE NOCASE"),
            ("a COLLATE NOCASE", "b"),
            ("a", "(+b) COLLATE BINARY"),
            ("(a COLLATE NOCASE)", "b"),
        ] {
            let sql = format!("SELECT {left} {op} {right} FROM SOURCE ORDER BY a,b");
            let expected = query(&c, &sql.replace("SOURCE", "baseline")).rows;
            for source in [
                "docs",
                "(SELECT a,b FROM docs) d",
                "(WITH d AS (SELECT a,b FROM docs) SELECT a,b FROM d) d",
            ] {
                assert_eq!(
                    query(&c, &sql.replace("SOURCE", source)).rows,
                    expected,
                    "{sql}: {source}"
                );
            }
        }
    }
}

#[test]
fn document_between_native_bounds_preserves_payloads_and_affinity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "CREATE TABLE baseline(v,n,t)");
    query(&c, "CREATE TABLE bounds(lo BLOB,hi BLOB,nlo INTEGER,nhi INTEGER,tlo TEXT COLLATE NOCASE,thi TEXT COLLATE NOCASE)");
    for v in ["X''", "X'02'", "X'0a'", "X'ff'", "NULL"] {
        query(
            &c,
            &format!("INSERT INTO docs(v,n,t) VALUES ({v},'10','BETA')"),
        );
        query(
            &c,
            &format!("INSERT INTO baseline VALUES ({v},'10','BETA')"),
        );
    }
    query(&c, "INSERT INTO bounds(lo,hi,nlo,nhi) VALUES (X'02',X'0a',2,10),(X'ff',X'02',10,2),(NULL,X'0a',NULL,10),(X'02',NULL,2,NULL),(NULL,NULL,NULL,NULL)");
    query(&c, "UPDATE bounds SET tlo='alpha',thi='gamma'");
    for not in ["", "NOT "] {
        for (v, lo, hi) in [
            ("d.v", "b.lo", "b.hi"),
            ("d.n", "b.nlo", "b.nhi"),
            ("d.t", "b.tlo COLLATE NOCASE", "b.thi COLLATE NOCASE"),
            ("d.t", "b.tlo", "b.thi"),
            ("d.t", "b.tlo COLLATE NOCASE", "b.thi COLLATE BINARY"),
            ("d.t", "b.tlo COLLATE BINARY", "b.thi COLLATE NOCASE"),
            ("(+d.v)", "(+b.lo)", "(b.hi)"),
        ] {
            let sql = format!("SELECT {v} {not}BETWEEN {lo} AND {hi} FROM SOURCE d JOIN bounds b ON 1 ORDER BY d.v,b.lo,b.hi");
            assert_eq!(
                query(&c, &sql.replace("SOURCE", "docs")).rows,
                query(
                    &c,
                    // Isolate the baseline column's implicit BINARY collation
                    // so each bound supplies the comparison collation.
                    &sql.replace("SOURCE", "baseline")
                        .replace("d.t", "(SELECT d.t)")
                )
                .rows,
                "{sql}"
            );
        }
    }
    for source in [
        "docs d",
        "(SELECT v,n,t FROM docs) d",
        "(WITH d AS (SELECT v,n,t FROM docs) SELECT * FROM d) d",
    ] {
        let sql =
            format!("SELECT count(*) FROM {source} JOIN bounds b ON d.v BETWEEN b.lo AND b.hi");
        assert_eq!(query(&c, &sql).rows, vec![vec![Value::Integer(2)]]);
    }
    query(&c, "CREATE TABLE refs");
    query(&c, "INSERT INTO refs {v:docs:a}");
    for not in ["", "NOT "] {
        let sql = format!("SELECT d.v {not}BETWEEN b.lo AND b.hi FROM refs d JOIN bounds b ON b.lo IS NULL AND b.hi IS NULL");
        assert_eq!(query(&c, &sql).rows, vec![vec![Value::Null]]);
        let error = c
            .execute(
                &sql.replace("b.hi IS NULL", "b.hi IS NOT NULL"),
                &Parameters::new(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("mixed record/scalar ordering"));
    }
    query(&c, "CREATE TABLE copied(v BLOB CHECK(v<>X'0a'))");
    query(&c, "BEGIN");
    let error = c
        .execute(
            "INSERT INTO copied SELECT d.v FROM docs d JOIN bounds b ON d.v BETWEEN b.lo AND b.hi",
            &Parameters::new(),
        )
        .unwrap_err();
    assert_eq!(error.code(), "FDB_CONSTRAINT");
    assert!(query(&c, "SELECT * FROM copied").rows.is_empty());
    query(&c, "ROLLBACK");
}

#[test]
fn collection_not_indexed_bypasses_managed_candidates() {
    let (_db, c) = setup();
    for table in ["users NOT INDEXED", "users AS u NOT INDEXED"] {
        let alias = if table.contains(" AS ") { "u" } else { "users" };
        let sql = format!("SELECT name FROM {table} WHERE {alias}.profile.city='Bangkok'");
        assert_eq!(
            query(&c, &sql).rows,
            query(
                &c,
                "SELECT name FROM users WHERE users.profile.city='Bangkok'"
            )
            .rows
        );
        let plan = format!("{:?}", query(&c, &format!("EXPLAIN QUERY PLAN {sql}")).rows);
        assert!(!plan.contains("users_city"), "{plan}");
        query(&c, "BEGIN");
        query(&c, "UPDATE users:2 {profile:{city:'Paris'}}");
        assert_eq!(query(&c, &sql).rows.len(), 1);
        query(&c, "ROLLBACK");
        assert_eq!(query(&c, &sql).rows.len(), 2);
    }
    assert!(c
        .execute(
            "SELECT name FROM users INDEXED BY users_city",
            &Parameters::new()
        )
        .is_err());
}

#[test]
fn keyword_columns_keep_their_names_when_a_document_table_appears_in_a_subquery() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE settings");
    query(
        &c,
        "CREATE TABLE organizations(id TEXT, plan TEXT, key TEXT, name TEXT)",
    );
    query(
        &c,
        "INSERT INTO organizations VALUES ('o1','trial','k1','Acme')",
    );
    query(&c, "INSERT INTO settings {id: settings:t1, note: 'x'}");
    let result = query(
        &c,
        "SELECT plan, key, name, o.plan FROM organizations o WHERE id='o1' AND EXISTS (SELECT 1 FROM settings t WHERE t.id=settings:t1)",
    );
    assert_eq!(&result.columns[..3], &["plan", "key", "name"]);
    assert!(
        result.columns.iter().all(|column| !column.contains('"')),
        "{:?}",
        result.columns
    );
    assert_eq!(
        result.rows[0][..3],
        [
            Value::String("trial".into()),
            Value::String("k1".into()),
            Value::String("Acme".into())
        ]
    );
    let returned = query(
        &c,
        "INSERT INTO settings {id: settings:t2, plan: 'basic'} RETURNING plan",
    );
    assert_eq!(returned.columns, vec!["plan"]);
    assert_eq!(returned.rows, vec![vec![Value::String("basic".into())]]);
}
