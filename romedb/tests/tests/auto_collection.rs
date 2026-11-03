use romedb::{Database, Parameters};

#[test]
fn document_writes_create_collections_atomically_and_persist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auto.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        let q = |sql: &str| c.execute(sql, &Parameters::new());
        q("INSERT INTO posts {id:posts:first,title:'Hello'}").unwrap();
        q("UPSERT people:alice {name:'Alice'}").unwrap();
        q("UPSERT people:alice {name:'Updated'}").unwrap();
        assert_eq!(q("SELECT * FROM posts").unwrap().rows.len(), 1);
        assert_eq!(q("SELECT * FROM people").unwrap().rows.len(), 1);
        q("BEGIN").unwrap();
        q("INSERT INTO temporary {n:1}").unwrap();
        q("ROLLBACK").unwrap();
        q("CREATE TABLE temporary").unwrap();
        assert!(q("INSERT INTO failed {id:other:wrong}").is_err());
        q("CREATE TABLE failed").unwrap();
        assert!(q("UPSERT rejected:a {n:unknown_function()}").is_err());
        q("CREATE TABLE rejected").unwrap();
        q("CREATE TABLE relational(n INTEGER)").unwrap();
        assert!(q("INSERT INTO relational {n:1}").is_err());
        q("INSERT INTO relational VALUES (2)").unwrap();
        assert_eq!(q("SELECT * FROM relational").unwrap().rows.len(), 1);
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    for name in ["posts", "people"] {
        assert_eq!(
            c.execute(&format!("SELECT * FROM {name}"), &Parameters::new())
                .unwrap()
                .rows
                .len(),
            1
        );
    }
}

#[test]
fn a_record_delete_needs_no_from() {
    let database = Database::open(":memory:").unwrap();
    let c = database.connect().unwrap();
    let q = |sql: &str| c.execute(sql, &Parameters::new()).expect(sql);
    q("INSERT INTO posts {id:posts:a,title:'A'}");
    q("INSERT INTO posts {id:posts:b,title:'B'}");
    q("CREATE INDEX posts_title ON posts(title)");
    q("DELETE posts:a");
    q("DELETE posts:missing");
    assert_eq!(q("SELECT title FROM posts").rows.len(), 1);
    assert!(q("SELECT * FROM posts WHERE title='A'").rows.is_empty());
    assert_eq!(
        q("DELETE posts:b RETURNING title").rows,
        vec![vec![romedb::Value::String("B".into())]]
    );
    assert!(q("SELECT * FROM posts").rows.is_empty());
    q("INSERT INTO posts {id:posts:c,title:'C'}");
    q("DELETE FROM posts:c");
    assert!(q("SELECT * FROM posts").rows.is_empty());
    assert_eq!(
        c.check_collection_integrity("posts", Default::default())
            .unwrap()
            .index_entries,
        0
    );
}
