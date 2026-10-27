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
fn projections_see_inserted_updated_and_deleted_typed_values() {
    let (_db, c) = setup();
    let inserted=q(&c,"INSERT INTO posts (id,n,tags) VALUES (posts:p1,2,array::new(posts:p2)) RETURNING id,n+1 AS next,tags");
    assert_eq!(inserted.affected, 1);
    assert_eq!(inserted.columns, vec!["id", "next", "tags"]);
    assert!(matches!(&inserted.rows[0][0], Value::Record(_)));
    assert_eq!(inserted.rows[0][1], Value::Integer(3));
    assert!(matches!(&inserted.rows[0][2], Value::Array(_)));
    assert_eq!(
        q(
            &c,
            "UPDATE posts SET n=n+2 RETURNING record::id(id) AS key,n"
        )
        .rows,
        vec![vec![Value::String("p1".into()), Value::Integer(4)]]
    );
    assert_eq!(
        q(
            &c,
            "DELETE FROM posts RETURNING n,doc::get(tags,'$[0]') AS ref"
        )
        .rows[0][0],
        Value::Integer(4)
    );
    assert!(q(&c, "SELECT * FROM posts").rows.is_empty());
}
#[test]
fn projection_failure_rolls_back_data_and_indexes_for_all_write_kinds() {
    let (_db, c) = setup();
    q(&c, "CREATE UNIQUE INDEX post_n ON posts (n)");
    q(&c, "INSERT INTO posts (n) VALUES (1),(2)");
    q(&c, "BEGIN");
    for sql in [
        "INSERT INTO posts (n) VALUES (3),(4) RETURNING CASE WHEN n=3 THEN n ELSE array::append(1,2) END AS v",
        "UPDATE posts SET n=n+10 RETURNING CASE WHEN n=11 THEN n ELSE array::append(1,2) END AS v",
        "DELETE FROM posts RETURNING CASE WHEN n=1 THEN n ELSE array::append(1,2) END AS v"
    ] {
        assert!(c.execute(sql,&Parameters::new()).is_err());
        assert_eq!(q(&c,"SELECT n FROM posts ORDER BY n").rows,vec![vec![Value::Integer(1)],vec![Value::Integer(2)]]);
        assert_eq!(c.lookup_index("posts","post_n",&Value::Integer(1)).unwrap().len(),1);
    }
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    q(&c, "COMMIT");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO posts (n) VALUES (9) RETURNING n");
    q(&c, "COMMIT");
}
#[test]
fn empty_results_keep_metadata_and_bound_results_keep_types() {
    let (_db, c) = setup();
    let params = Parameters::from([("$flag".into(), Value::Boolean(true))]);
    let empty = c
        .execute("UPDATE posts SET n=1 RETURNING n,$flag AS flag", &params)
        .unwrap();
    assert_eq!(empty.columns, vec!["n", "flag"]);
    assert!(empty.rows.is_empty());
    assert_eq!(empty.affected, 0);
    let row = c
        .execute(
            "INSERT INTO posts (n) VALUES (1) RETURNING $flag AS flag",
            &params,
        )
        .unwrap();
    assert_eq!(row.rows, vec![vec![Value::Boolean(true)]]);
    for projection in [
        "count(*)",
        "sum(n)",
        "min(n)",
        "(SELECT 1)",
        "row_number() OVER ()",
    ] {
        assert!(c
            .execute(
                &format!("DELETE FROM posts RETURNING {projection}"),
                &Parameters::new()
            )
            .is_err());
    }
    assert_eq!(q(&c, "SELECT n FROM posts").rows.len(), 1);
}
#[test]
fn insert_select_returning_and_qualified_snapshot_fields() {
    let (_db, c) = setup();
    q(&c, "INSERT INTO posts (n) VALUES (1),(2)");
    let result = q(
        &c,
        "INSERT INTO posts (n) SELECT n+10 FROM posts RETURNING posts.n AS n, *",
    );
    assert_eq!(result.affected, 2);
    assert!(result
        .rows
        .iter()
        .all(|r| matches!(&r[1], Value::Object(_))));
}
#[test]
fn object_writes_share_typed_returning_snapshots() {
    let (_db, c) = setup();
    let inserted = q(
        &c,
        "INSERT INTO posts {id:posts:p1,n:2,tags:[true]} RETURNING id,tags",
    );
    assert!(matches!(&inserted.rows[0][0], Value::Record(_)));
    assert_eq!(
        inserted.rows[0][1],
        Value::Array(vec![Value::Boolean(true)])
    );
    assert_eq!(
        q(&c, "UPDATE posts:p1 {n:n+1} RETURNING n").rows,
        vec![vec![Value::Integer(3)]]
    );
    assert_eq!(
        q(&c, "UPSERT posts:p1 {n:n+1} RETURNING n").rows,
        vec![vec![Value::Integer(4)]]
    );
    assert_eq!(
        q(&c, "UPSERT posts:p2 {n:5} RETURNING record::id(id) AS key").rows,
        vec![vec![Value::String("p2".into())]]
    );
    let updated = q(&c, "UPDATE posts {n:n+1} WHERE n >= 4 RETURNING n;");
    assert_eq!(updated.rows.len(), 2);
    assert_eq!(
        q(&c, "DELETE FROM posts:p1 RETURNING n").rows,
        vec![vec![Value::Integer(5)]]
    );
    assert_eq!(
        q(&c, "UPDATE posts:p2 {n:7} RETURNING ';'").rows,
        vec![vec![Value::String(";".into())]]
    );
    let absent = q(&c, "DELETE FROM posts:missing RETURNING n, id");
    assert_eq!(absent.columns, vec!["n", "id"]);
    assert!(absent.rows.is_empty());
}
#[test]
fn object_returning_syntax_errors_rollback_and_cannot_read_other_sources() {
    let (_db, c) = setup();
    for projection in [
        "n FROM posts",
        "(SELECT 1)",
        "count(*)",
        "__romedb_value(1,2)",
        "n; DELETE FROM posts",
    ] {
        let sql = format!("INSERT INTO posts {{id:posts:p1,n:1}} RETURNING {projection}");
        assert!(c.execute(&sql, &Parameters::new()).is_err(), "{sql}");
        assert!(q(&c, "SELECT * FROM posts").rows.is_empty());
    }
    q(&c, "INSERT INTO posts {id:posts:p1,n:1}");
    assert!(c
        .execute("DELETE FROM posts:p1 RETURNING sum(n)", &Parameters::new())
        .is_err());
    assert_eq!(
        q(&c, "SELECT n FROM posts").rows,
        vec![vec![Value::Integer(1)]]
    );
}
#[test]
fn predicate_returning_distinguishes_quoted_fields_strings_and_nested_calls() {
    let (_db, c) = setup();
    q(
        &c,
        "INSERT INTO posts {id:posts:p1,n:1,\"returning\":'RETURNING'}",
    );
    let params = Parameters::from([
        ("$n".into(), Value::Integer(2)),
        ("$flag".into(), Value::Boolean(true)),
    ]);
    let result=c.execute("UPDATE posts {n:$n} WHERE \"returning\"='RETURNING' AND (n=1) RETURNING coalesce(n,0) AS n,$flag AS flag",&params).unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::Integer(2), Value::Boolean(true)]]
    );
    let empty = q(&c, "UPDATE posts {n:3} WHERE n < 0 RETURNING n");
    assert_eq!(empty.columns, vec!["n"]);
    assert!(empty.rows.is_empty());
}

#[test]
fn deep_paths_work_in_write_expressions_and_all_returning_snapshots() {
    let (_db, c) = setup();
    q(&c, "DEFINE FIELD profile.address.city ON posts TYPE string");
    q(
        &c,
        "CREATE UNIQUE INDEX posts_city ON posts (profile.address.city)",
    );
    let inserted=q(&c,"INSERT INTO posts {id:posts:p1,profile:{address:{city:'Paris',active:true}}} RETURNING posts.profile.address.city,posts.profile.address.active");
    assert_eq!(inserted.columns, vec!["city", "active"]);
    assert_eq!(
        inserted.rows,
        vec![vec![Value::String("Paris".into()), Value::Boolean(true)]]
    );
    assert_eq!(q(&c,"UPDATE posts SET profile.address.city=upper(posts.profile.address.city) WHERE posts.profile.address.active=true RETURNING posts.profile.address.city").rows,vec![vec![Value::String("PARIS".into())]]);
    assert!(c
        .lookup_index("posts", "posts_city", &Value::String("Paris".into()))
        .unwrap()
        .is_empty());
    assert_eq!(
        c.lookup_index("posts", "posts_city", &Value::String("PARIS".into()))
            .unwrap()
            .len(),
        1
    );
    assert!(c.execute("UPDATE posts SET profile.address.city=posts.profile.address.active RETURNING posts.profile.address.city",&Parameters::new()).is_err());
    assert_eq!(
        q(&c, "SELECT posts.profile.address.city FROM posts").rows,
        vec![vec![Value::String("PARIS".into())]]
    );
    q(&c, "CREATE TABLE copies");
    assert_eq!(q(&c,"INSERT INTO copies (city,active) SELECT posts.profile.address.city,posts.profile.address.active FROM posts RETURNING city,active").rows,vec![vec![Value::String("PARIS".into()),Value::Boolean(true)]]);
    q(&c, "BEGIN");
    assert_eq!(q(&c,"DELETE FROM posts WHERE posts.profile.address.city='PARIS' RETURNING posts.profile.address.city").rows,vec![vec![Value::String("PARIS".into())]]);
    q(&c, "ROLLBACK");
    assert_eq!(
        c.lookup_index("posts", "posts_city", &Value::String("PARIS".into()))
            .unwrap()
            .len(),
        1
    );
    let empty=q(&c,"UPDATE posts SET profile.address.city='unused' WHERE posts.profile.address.city='missing' RETURNING posts.profile.address.city");
    assert_eq!(empty.columns, vec!["city"]);
    assert!(empty.rows.is_empty());
    assert!(c
        .execute(
            "DELETE FROM posts RETURNING unknown.profile.address.city",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(
        c.lookup_index("posts", "posts_city", &Value::String("PARIS".into()))
            .unwrap()
            .len(),
        1
    );
}
