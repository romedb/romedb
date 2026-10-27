use romedb::{Database, Document, Parameters, ResultLimits, TransactionState, Value};

fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}
fn object(value: &Value) -> &Document {
    let Value::Object(document) = value else {
        panic!("{value:?}")
    };
    document
}
fn n(value: &Value) -> i64 {
    let Value::Integer(n) = object(value)["n"] else {
        panic!("{value:?}")
    };
    n
}

#[test]
fn every_write_returns_its_before_and_stored_after_image() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "DEFINE FIELD doubled ON docs TYPE integer VALUE(n*2)");
    let inserted=q(&c,"INSERT INTO docs {id:docs:a,n:1,tags:[true,docs:b]} RETURNING doc::before(),doc::after(),doc::diff(),*");
    assert_eq!(
        inserted.columns,
        vec![
            "doc::before ()",
            "doc::after ()",
            "doc::diff ()",
            "document"
        ]
    );
    assert_eq!(inserted.rows[0][0], Value::Null);
    assert_eq!(inserted.rows[0][1], inserted.rows[0][3]);
    assert_eq!(object(&inserted.rows[0][1])["doubled"], Value::Integer(2));
    let Value::Array(diff) = &inserted.rows[0][2] else {
        panic!()
    };
    assert_eq!(object(&diff[0])["path"], Value::String(String::new()));
    assert_eq!(object(&diff[0])["value"], inserted.rows[0][1]);
    for (sql, before, after) in [
        ("UPDATE docs:a {n:2}", 1, 2),
        ("UPDATE docs {n:n+1} WHERE id=docs:a", 2, 3),
        ("UPDATE docs SET n=4 WHERE id=docs:a", 3, 4),
        ("UPDATE docs:a MERGE {n:5}", 4, 5),
        ("UPDATE docs:a CONTENT {n:6}", 5, 6),
        (
            "UPDATE docs:a PATCH [{op:'replace',path:'/n',value:7}]",
            6,
            7,
        ),
        ("UPSERT docs:a {n:8}", 7, 8),
        ("UPSERT docs {id:docs:a,n:9}", 8, 9),
        ("INSERT OR REPLACE INTO docs(id,n) VALUES(docs:a,10)", 9, 10),
    ] {
        let result = q(
            &c,
            &format!(
                "{sql} RETURNING doc::before() AS old,doc::after() AS new,doc::diff() AS changes"
            ),
        );
        assert_eq!(result.affected, 1);
        assert_eq!(n(&result.rows[0][0]), before, "{sql}");
        assert_eq!(n(&result.rows[0][1]), after, "{sql}");
        assert_eq!(
            object(&result.rows[0][1])["doubled"],
            Value::Integer(after * 2)
        );
        let Value::Array(diff) = &result.rows[0][2] else {
            panic!()
        };
        assert!(diff
            .iter()
            .any(|v| object(v)["path"] == Value::String("/n".into())));
    }
    let deleted = q(
        &c,
        "DELETE FROM docs:a RETURNING doc::before(),doc::after(),doc::diff(),*",
    );
    assert_eq!(n(&deleted.rows[0][0]), 10);
    assert_eq!(deleted.rows[0][1], Value::Null);
    assert_eq!(deleted.rows[0][0], deleted.rows[0][3]);
    for sql in [
        "DELETE FROM docs:a",
        "UPDATE docs:a {n:11}",
        "UPDATE docs SET n=11",
    ] {
        let empty = q(
            &c,
            &format!("{sql} RETURNING doc::before() AS old,doc::after() AS new"),
        );
        assert!(empty.rows.is_empty());
        assert_eq!(empty.columns, vec!["old", "new"]);
    }
}

#[test]
fn replacement_conflicts_and_repeated_ids_keep_per_write_images() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE UNIQUE INDEX docs_n ON docs(n)");
    q(&c, "INSERT INTO docs(id,n) VALUES(docs:a,1),(docs:b,2)");
    let replaced=q(&c,"INSERT OR REPLACE INTO docs(id,n) VALUES(docs:a,2),(docs:a,3),(docs:c,3) RETURNING doc::before(),doc::after()");
    assert_eq!(replaced.affected, 3);
    assert_eq!(n(&replaced.rows[0][0]), 1);
    assert_eq!(n(&replaced.rows[1][0]), 2);
    assert_eq!(replaced.rows[2][0], Value::Null);
    assert_eq!(
        replaced.rows.iter().map(|r| n(&r[1])).collect::<Vec<_>>(),
        vec![2, 3, 3]
    );
    let ignored=q(&c,"INSERT OR IGNORE INTO docs(id,n) VALUES(docs:c,9),(docs:d,4) RETURNING doc::before(),doc::after()");
    assert_eq!(ignored.rows.len(), 1);
    assert_eq!(ignored.rows[0][0], Value::Null);
    assert_eq!(n(&ignored.rows[0][1]), 4);
    let deleted = q(&c, "DELETE FROM docs RETURNING doc::before(),doc::after()");
    assert_eq!(deleted.rows.len(), 2);
    assert!(deleted.rows.iter().all(|r| r[1] == Value::Null));
}

#[test]
fn parameters_aliases_and_snapshot_helpers_remain_scoped() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    let params = Parameters::from([
        ("$before".into(), Value::Integer(17)),
        ("$after".into(), Value::Boolean(true)),
        ("$diff".into(), Value::Null),
        (
            "$__romedb_mutation_before_0".into(),
            Value::String("caller".into()),
        ),
    ]);
    let result=c.execute("INSERT INTO docs(id,n) VALUES(docs:a,1) RETURNING $before,$after,$diff,$__romedb_mutation_before_0,doc::before(),doc::get(doc::after(),'$.n') AS n",&params).unwrap();
    assert_eq!(
        result.rows[0],
        vec![
            Value::Integer(17),
            Value::Boolean(true),
            Value::Null,
            Value::String("caller".into()),
            Value::Null,
            Value::Integer(1)
        ]
    );
    for sql in [
        "SELECT doc::before() FROM docs",
        "UPDATE docs SET n=doc::after()",
        "UPDATE docs:a {n:doc::before()}",
        "UPDATE docs SET n=2 RETURNING doc::before(1)",
        "UPDATE docs SET n=2 RETURNING doc::diff(1,2)",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
        assert_eq!(
            q(&c, "SELECT n FROM docs").rows,
            vec![vec![Value::Integer(1)]]
        );
    }
    for sql in [
        "SELECT doc::before() FROM docs WHERE 0",
        "SELECT doc::after() WHERE 0",
        "UPDATE docs SET n=doc::after() WHERE 0",
    ] {
        assert_eq!(
            c.execute(sql, &Parameters::new()).unwrap_err().code(),
            "FDB_VALIDATION",
            "{sql}"
        );
    }
    assert_eq!(
        c.profile_select("SELECT doc::diff() FROM docs WHERE 0", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    q(&c, "CREATE TABLE native(n INTEGER)");
    assert!(c
        .execute(
            "INSERT INTO native VALUES(1) RETURNING doc::after()",
            &Parameters::new()
        )
        .is_err());
    assert!(q(&c, "SELECT * FROM native").rows.is_empty());
}

#[test]
fn result_and_diff_bounds_roll_back_mutations_and_preserve_prior_work() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE UNIQUE INDEX docs_n ON docs(n)");
    q(&c, "INSERT INTO docs(id,n) VALUES(docs:a,1)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs(id,n) VALUES(docs:b,2)");
    for limits in [
        ResultLimits {
            max_rows: 1,
            max_payload_bytes: 10000,
        },
        ResultLimits {
            max_rows: 10,
            max_payload_bytes: 40,
        },
    ] {
        assert_eq!(
            c.write_with_result_limits(
                "UPDATE docs SET n=n+10 RETURNING doc::before(),doc::after(),doc::diff()",
                &Parameters::new(),
                limits
            )
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        assert_eq!(c.transaction_state(), TransactionState::Active);
        assert_eq!(
            q(&c, "SELECT n FROM docs ORDER BY n").rows,
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        );
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
    let changes = Document::from_iter((0..1025).map(|i| (format!("f{i}"), Value::Integer(i))));
    let params = Parameters::from([("$body".into(), Value::Object(changes))]);
    assert_eq!(
        c.execute("UPDATE docs:a MERGE $body RETURNING doc::diff()", &params)
            .unwrap_err()
            .code(),
        "FDB_LIMIT"
    );
    assert_eq!(q(&c, "SELECT n FROM docs ORDER BY n").rows.len(), 2);
    let lazy = c
        .execute(
            "UPDATE docs:a MERGE $body RETURNING CASE WHEN true THEN 1 ELSE doc::diff() END",
            &params,
        )
        .unwrap();
    assert_eq!(lazy.rows, vec![vec![Value::Integer(1)]]);
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT n FROM docs").rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn snapshots_survive_reader_isolation_reopen_and_all_index_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("mutations.db");
    let final_document;
    {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE docs",
            "CREATE UNIQUE INDEX docs_n ON docs(n)",
            "CREATE INDEX docs_ref ON docs(author)",
            "CREATE SEARCH INDEX docs_text ON docs(body) USING FULLTEXT",
            "CREATE SEARCH INDEX docs_vec ON docs(v) USING VECTOR WITH(metric='l2',dimensions=2)",
            "CREATE SEARCH INDEX docs_geo ON docs(location) USING SPATIAL",
            "INSERT INTO docs {id:docs:a,n:1,author:refs:a,body:'old',v:vector32('[1,0]'),location:geo::point(0,0)}",
        ] {q(&c,sql);}
        let original = q(&c, "SELECT * FROM docs").rows[0][0].clone();
        let reader = db.connect().unwrap();
        q(&reader, "BEGIN");
        assert_eq!(
            q(&reader, "SELECT * FROM docs").rows,
            vec![vec![original.clone()]]
        );
        q(&c, "BEGIN");
        q(&c, "INSERT INTO docs {id:docs:prior,n:2}");
        let sql="UPDATE docs:a MERGE {n:3,author:refs:b,body:'new',v:vector32('[0,1]'),location:geo::point(1,1)} RETURNING doc::before() AS old,doc::after() AS new,doc::diff() AS diff";
        assert_eq!(
            c.write_with_result_limits(
                sql,
                &Parameters::new(),
                ResultLimits {
                    max_rows: 1,
                    max_payload_bytes: 30
                }
            )
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        assert_eq!(
            q(&c, "SELECT * FROM docs WHERE id=docs:a").rows,
            vec![vec![original.clone()]]
        );
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
        let result = q(&c, sql);
        assert_eq!(result.rows[0][0], original);
        final_document = result.rows[0][1].clone();
        q(&c, "COMMIT");
        assert_eq!(q(&reader, "SELECT * FROM docs").rows, vec![vec![original]]);
        q(&reader, "COMMIT");
        for sql in [
            "SELECT id FROM docs WHERE n=3",
            "SELECT id FROM docs WHERE author=refs:b",
            "SELECT id FROM search::text('docs_text','new',1)",
            "SELECT id FROM search::vector('docs_vec',vector32('[0,1]'),1)",
            "SELECT id FROM search::near('docs_geo',geo::point(1,1),10)",
        ] {
            assert_eq!(
                q(&c, sql).rows,
                vec![vec![object(&final_document)["id"].clone()]],
                "{sql}"
            );
        }
    }
    let c = Database::open(file.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(
        q(
            &c,
            "DELETE FROM docs:a RETURNING doc::before(),doc::after()"
        )
        .rows,
        vec![vec![final_document, Value::Null]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
    assert_eq!(
        q(&c, "SELECT n FROM docs").rows,
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn retained_before_images_consume_write_payload_without_extra_row_counts() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    let p = Parameters::from([("$text".into(), Value::String("x".repeat(60)))]);
    c.execute("INSERT INTO docs {id:docs:a,n:1,text:$text}", &p)
        .unwrap();
    let c = c.with_write_buffer_limits(ResultLimits {
        max_rows: 1,
        max_payload_bytes: 110,
    });
    q(&c, "UPDATE docs SET n=2 RETURNING doc::after() AS after");
    q(&c, "BEGIN");
    assert_eq!(
        c.execute(
            "UPDATE docs SET n=3 RETURNING doc::before() AS before",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    assert_eq!(c.transaction_state(), TransactionState::Active);
    assert_eq!(
        q(&c, "SELECT n FROM docs").rows,
        vec![vec![Value::Integer(2)]]
    );
    q(&c, "ROLLBACK");
    let c = c.with_write_buffer_limits(ResultLimits {
        max_rows: 1,
        max_payload_bytes: 250,
    });
    assert_eq!(
        q(
            &c,
            "UPDATE docs SET n=3 RETURNING doc::before() AS before,doc::after() AS after"
        )
        .rows
        .len(),
        1
    );
}
