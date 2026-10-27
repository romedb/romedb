use romedb::{Database, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
fn setup() -> (Database, romedb::Connection) {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE posts");
    q(
        &c,
        "INSERT INTO posts {id:posts:p1,n:1,tags:[true,posts:p2]}",
    );
    q(&c, "INSERT INTO posts {id:posts:p2,n:2,tags:[false]}");
    (db, c)
}
#[test]
fn conditional_values_preserve_types_and_lazy_branches() {
    let (_db, c) = setup();
    let rows = q(
        &c,
        "SELECT CASE WHEN n=1 THEN tags ELSE id END AS v FROM posts ORDER BY n",
    )
    .rows;
    assert!(matches!(&rows[0][0],Value::Array(a) if a[0]==Value::Boolean(true)));
    assert!(matches!(&rows[1][0], Value::Record(_)));
    assert_eq!(q(&c,"SELECT CASE n WHEN 1 THEN doc::get(tags,'$[0]') ELSE NULL END AS v FROM posts ORDER BY n").rows,vec![vec![Value::Boolean(true)],vec![Value::Null]]);
    assert_eq!(
        q(
            &c,
            "SELECT CASE WHEN n=1 THEN record::id(id) END AS v FROM posts ORDER BY n"
        )
        .rows,
        vec![vec![Value::String("p1".into())], vec![Value::Null]]
    );
    assert_eq!(q(&c,"SELECT CASE WHEN n=1 THEN 'safe' ELSE array::append(1,2) END AS v FROM posts WHERE n=1").rows,vec![vec![Value::String("safe".into())]]);
}
#[test]
fn case_composes_with_helpers_scalar_contexts_and_ordering() {
    let (_db, c) = setup();
    assert_eq!(q(&c,"SELECT doc::get(CASE n WHEN 1 THEN tags ELSE array::new(9) END,'$[0]') AS v FROM posts ORDER BY n").rows,vec![vec![Value::Boolean(true)],vec![Value::Integer(9)]]);
    assert_eq!(
        q(
            &c,
            "SELECT n FROM posts WHERE CASE WHEN n=1 THEN 0 ELSE 1 END"
        )
        .rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT (CASE WHEN n=1 THEN 10 ELSE 2 END) AS v FROM posts ORDER BY v"
        )
        .rows,
        vec![vec![Value::Integer(2)], vec![Value::Integer(10)]]
    );
    assert_eq!(q(&c,"SELECT CASE WHEN n=1 THEN NULL ELSE CASE WHEN n=2 THEN n+3 END END AS v FROM posts ORDER BY n").rows,vec![vec![Value::Null],vec![Value::Integer(5)]]);
}
#[test]
fn case_writes_validate_final_typed_candidates_and_rollback() {
    let (_db, c) = setup();
    q(&c, "DEFINE FIELD tags ON posts TYPE array REQUIRED");
    q(
        &c,
        "UPDATE posts SET tags=CASE WHEN n=1 THEN array::append(tags,5) ELSE tags END",
    );
    let before = q(&c, "SELECT tags FROM posts ORDER BY n").rows;
    assert!(c
        .execute(
            "UPDATE posts SET tags=CASE n WHEN 1 THEN array::new(6) ELSE 'bad' END",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(q(&c, "SELECT tags FROM posts ORDER BY n").rows, before);
    let params = Parameters::from([("$flag".into(), Value::Boolean(true))]);
    c.execute("INSERT INTO posts (id,tags) VALUES (posts:p3,CASE WHEN 1 THEN array::new($flag) ELSE NULL END)",&params).unwrap();
    assert_eq!(
        q(&c, "SELECT tags FROM posts WHERE id=posts:p3").rows,
        vec![vec![Value::Array(vec![Value::Boolean(true)])]]
    );
}
#[test]
fn object_case_supports_typed_keys_nested_results_and_lazy_evaluation() {
    let (_db, c) = setup();
    q(&c,"UPDATE posts {tags:CASE id WHEN posts:p1 THEN array::append(tags,CASE WHEN n=1 THEN true ELSE false END) ELSE tags END}");
    assert_eq!(
        q(&c, "SELECT doc::get(tags,'$[2]') AS v FROM posts WHERE n=1").rows,
        vec![vec![Value::Boolean(true)]]
    );
    q(&c,"UPSERT posts:p3 {tags:CASE WHEN true THEN [posts:p1] ELSE array::append(1,2) END, n:CASE NULL WHEN NULL THEN 1 ELSE 3 END}");
    assert_eq!(
        q(&c, "SELECT n FROM posts WHERE id=posts:p3").rows,
        vec![vec![Value::Integer(3)]]
    );
    q(&c, "UPDATE posts:p3 {v:CASE WHEN false THEN 2 END}");
    assert_eq!(
        q(&c, "SELECT v FROM posts WHERE id=posts:p3").rows,
        vec![vec![Value::Null]]
    );
    for expr in [
        "CASE END",
        "CASE WHEN 1 2 END",
        "CASE WHEN 1 THEN 2",
        "CASE WHEN 1 THEN 2 ELSE END",
    ] {
        assert!(c
            .execute(&format!("UPDATE posts:p3 {{v:{expr}}}"), &Parameters::new())
            .is_err());
    }
}

#[test]
fn simple_case_binary_keys_match_native_blobs_and_cast_affinity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE TABLE baseline(data BLOB)");
    for hex in ["31", "32", "", "61"] {
        q(&c, &format!("INSERT INTO docs (data) VALUES (X'{hex}')"));
        q(&c, &format!("INSERT INTO baseline VALUES (X'{hex}')"));
    }
    q(&c, "INSERT INTO docs (data) VALUES (NULL)");
    q(&c, "INSERT INTO baseline VALUES (NULL)");
    for expr in [
        "CASE data WHEN X'31' THEN 1 WHEN X'32' THEN 2 ELSE 0 END",
        "CASE X'31' WHEN data THEN 1 ELSE 0 END",
        "CASE substr(data,1,1) WHEN X'31' THEN 1 ELSE 0 END",
        "CASE data WHEN substr(X'3132',1,1) THEN 1 ELSE 0 END",
        "CASE CAST(data AS INTEGER) WHEN '1' THEN 1 ELSE 0 END",
        "CASE CAST(data AS TEXT) COLLATE NOCASE WHEN 'A' THEN 1 ELSE 0 END",
        "CASE data WHEN NULL THEN 1 ELSE 0 END",
        "CASE data WHEN X'31' THEN data ELSE X'32' END",
    ] {
        let sql = |table| format!("SELECT {expr} AS value FROM {table} ORDER BY data");
        assert_eq!(
            q(&c, &sql("docs")).rows,
            q(&c, &sql("baseline")).rows,
            "{expr}"
        );
    }
    q(&c, "CREATE INDEX docs_data ON docs(data)");
    q(&c, "BEGIN");
    assert_eq!(q(&c, "UPDATE docs SET data=CASE data WHEN X'31' THEN X'33' ELSE data END WHERE data=X'31' RETURNING data").rows,
               vec![vec![Value::Binary(b"3".to_vec())]]);
    q(&c, "ROLLBACK");
    assert_eq!(
        c.lookup_index("docs", "docs_data", &Value::Binary(b"1".to_vec()))
            .unwrap()
            .len(),
        1
    );
}
