use romedb::{Database, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
#[test]
fn mixed_stars_preserve_native_columns_and_outer_join_nulls() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE posts");
    q(&c, "INSERT INTO posts {id:posts:p1,owner:1}");
    q(&c, "INSERT INTO posts {id:posts:p2,owner:2}");
    q(&c,"CREATE TABLE accounts (account_id INTEGER PRIMARY KEY, \"display name\" TEXT, payload BLOB)");
    q(&c, "INSERT INTO accounts VALUES (1,'Alice',X'00FF')");
    let rows=q(&c,"SELECT a.*, p.id AS post FROM posts p LEFT JOIN accounts a ON p.owner=a.account_id ORDER BY p.id");
    assert_eq!(
        rows.columns,
        vec!["account_id", "display name", "payload", "post"]
    );
    assert_eq!(
        &rows.rows[0][..3],
        q(&c, "SELECT * FROM accounts").rows[0].as_slice()
    );
    assert_eq!(&rows.rows[1][..3], &[Value::Null, Value::Null, Value::Null]);
    let all = q(
        &c,
        "SELECT * FROM posts p JOIN accounts a ON p.owner=a.account_id",
    );
    assert_eq!(
        all.columns,
        vec!["document", "account_id", "display name", "payload"]
    );
    assert!(matches!(&all.rows[0][0], Value::Object(_)));
    assert_eq!(
        &all.rows[0][1..],
        q(&c, "SELECT * FROM accounts").rows[0].as_slice()
    );
    let empty = q(
        &c,
        "SELECT a.* FROM posts p JOIN accounts a ON p.owner=a.account_id WHERE 0",
    );
    assert!(empty.rows.is_empty());
    assert_eq!(empty.columns, vec!["account_id", "display name", "payload"]);
}
#[test]
fn view_stars_and_group_ordinals_feed_validated_inserts() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE posts");
    q(&c, "INSERT INTO posts {owner:1}");
    q(&c, "CREATE TABLE accounts (account_id INTEGER, label TEXT)");
    q(&c, "INSERT INTO accounts VALUES (1,'Alice')");
    q(
        &c,
        "CREATE VIEW account_view AS SELECT label AS title, account_id AS owner_id FROM accounts",
    );
    assert_eq!(
        q(
            &c,
            "SELECT a.* FROM posts p JOIN account_view a ON p.owner=a.owner_id"
        )
        .rows,
        q(&c, "SELECT * FROM account_view").rows
    );
    q(&c, "CREATE TABLE copies");
    q(&c, "DEFINE FIELD owner ON copies TYPE integer REQUIRED");
    q(&c,"INSERT INTO copies (title,owner) SELECT a.* FROM posts p JOIN account_view a ON p.owner=a.owner_id");
    assert_eq!(
        q(&c, "SELECT title,owner FROM copies").rows,
        vec![vec![Value::String("Alice".into()), Value::Integer(1)]]
    );
    assert_eq!(q(&c,"SELECT a.*, count(*) AS n FROM posts p JOIN account_view a ON p.owner=a.owner_id GROUP BY 1,2").rows,
        vec![vec![Value::String("Alice".into()),Value::Integer(1),Value::Integer(1)]]);
}
