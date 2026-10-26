use romedb::{Database, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
fn setup() -> (Database, romedb::Connection) {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE posts");
    (db, c)
}
#[test]
fn scalar_expressions_match_engine_precedence() {
    let (_db, c) = setup();
    for (i, expr) in [
        "2 + 3 * 4",
        "2 = 3 < 1",
        "NOT 1 = 2 AND 1",
        "-9223372036854775808",
        "lower(upper('Hello'))",
        "8 >> 1 + 1",
        "NULL IS NOT 3",
    ]
    .iter()
    .enumerate()
    {
        let expected = q(&c, &format!("SELECT {expr}")).rows;
        q(
            &c,
            &format!("INSERT INTO posts {{id:type::record('posts',{i}),v:{expr}}}"),
        );
        let actual = q(
            &c,
            &format!("SELECT v FROM posts WHERE id=type::record('posts',{i})"),
        )
        .rows;
        assert_eq!(actual, expected, "{expr}");
    }
}
#[test]
fn patches_read_preupdate_document_and_upserts_handle_missing_fields() {
    let (_db, c) = setup();
    q(
        &c,
        "INSERT INTO posts {id:posts:p1,a:2,b:3,tags:array::new(true,posts:p2)}",
    );
    q(&c, "UPDATE posts:p1 {a:b,b:a,tags:array::append(tags,{ok:true}),label:coalesce('safe',array::append(1,2))}");
    assert_eq!(
        q(&c, "SELECT a,b,label FROM posts").rows,
        vec![vec![
            Value::Integer(3),
            Value::Integer(2),
            Value::String("safe".into())
        ]]
    );
    q(&c, "UPSERT posts:p1 {a:a+1}");
    q(&c, "UPSERT posts:p2 {a:coalesce(a,0)+1}");
    assert_eq!(
        q(&c, "SELECT a FROM posts ORDER BY a").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(4)]]
    );
    assert!(c
        .execute("UPDATE posts:p1 {a:length(tags)}", &Parameters::new())
        .is_err());
}
#[test]
fn predicate_patches_bind_body_parameters_and_rollback_all_rows() {
    let (_db, c) = setup();
    q(&c, "INSERT INTO posts {id:posts:p1,n:1}");
    q(&c, "INSERT INTO posts {id:posts:p2,n:2}");
    q(&c, "DEFINE FIELD n ON posts TYPE integer CHECK (n < 5)");
    let params = Parameters::from([
        ("$amount".into(), Value::Integer(1)),
        ("$min".into(), Value::Integer(1)),
    ]);
    let result = c
        .execute(
            "UPDATE posts {n:n+$amount} WHERE n >= $min RETURNING *",
            &params,
        )
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert!(c
        .execute("UPDATE posts {n:n+2}", &Parameters::new())
        .is_err());
    assert_eq!(
        q(&c, "SELECT n FROM posts ORDER BY n").rows,
        vec![vec![Value::Integer(2)], vec![Value::Integer(3)]]
    );
    assert!(q(&c, "UPDATE posts {n:0} WHERE n > 100 RETURNING *")
        .rows
        .is_empty());
}
#[test]
fn document_paths_preserve_null_presence_and_typed_values() {
    let (_db, c) = setup();
    q(
        &c,
        r#"INSERT INTO posts {id:posts:p1, a:doc::get({"a.b":[posts:p2]}, '$["a.b"][0]'), present:doc::has({x:null},'$.x'), missing:doc::has({x:null},'$.y'), raw:doc::get('{"x":1}','$.x')}"#,
    );
    let rows = q(&c, "SELECT a,present,missing,raw FROM posts").rows;
    assert!(matches!(rows[0][0], Value::Record(_)));
    assert_eq!(
        rows[0][1..],
        [Value::Boolean(true), Value::Boolean(false), Value::Null]
    );
    for path in ["$[*]", "$[-1]", "$..x", "$[0:2]"] {
        assert!(c
            .execute(
                &format!("UPDATE posts:p1 {{a:doc::get([], '{path}')}}"),
                &Parameters::new()
            )
            .is_err());
    }
}
#[test]
fn typed_record_comparisons_do_not_compare_storage_bytes() {
    let (_db, c) = setup();
    q(&c,"INSERT INTO posts {id:posts:p1,a:type::record('posts',2)<type::record('posts',10),b:type::record('posts',2)=type::record('posts','2'),c:posts:p1='posts:p1'}");
    assert_eq!(
        q(&c, "SELECT a,b,c FROM posts").rows,
        vec![vec![
            Value::Integer(1),
            Value::Integer(0),
            Value::Integer(0)
        ]]
    );
    for expr in [
        "posts:p1+1",
        "length(posts:p1)",
        "posts:p1<1",
        "array::new(1,)",
    ] {
        assert!(c
            .execute(&format!("UPDATE posts:p1 {{a:{expr}}}"), &Parameters::new())
            .is_err());
    }
}

#[test]
fn long_expression_chains_fail_before_evaluation() {
    let (_db, c) = setup();
    let expr = vec!["1"; 1000].join("+");
    assert!(c
        .execute(
            &format!("INSERT INTO posts {{v:{expr}}}"),
            &Parameters::new()
        )
        .is_err());
    assert!(q(&c, "SELECT * FROM posts").rows.is_empty());
}
#[test]
fn field_references_without_a_source_document_suggest_single_quotes() {
    let (_db, c) = setup();
    for (sql, field) in [
        ("INSERT INTO posts:a {foo: \"bar\"}", "bar"),
        ("INSERT INTO posts {id: posts:b, foo: bar}", "bar"),
        (
            "INSERT INTO posts {id: posts:c, foo: \"first name\".city}",
            "first name.city",
        ),
    ] {
        let error = c.execute(sql, &Parameters::new()).unwrap_err();
        assert_eq!(error.code(), "FDB_VALIDATION", "{sql}");
        assert_eq!(
            error.to_string(),
            format!(
                "validation: \"{field}\" is a field reference, but this statement has no source document; use '{field}' for a string"
            ),
            "{sql}"
        );
    }
    q(&c, "INSERT INTO posts:d {\"first name\": 'Ann'}");
    q(&c, "UPDATE posts {copy: \"first name\"} WHERE id = posts:d");
    assert_eq!(
        q(&c, "SELECT copy FROM posts:d").rows,
        vec![vec![Value::String("Ann".into())]]
    );
}
