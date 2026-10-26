use romedb::{Database, Document, Key, Parameters, Record, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
fn setup() -> (Database, romedb::Connection) {
    let db = Database::open(":memory:").expect("open");
    let c = db.connect().expect("connect");
    q(&c, "CREATE TABLE users");
    q(&c, "DEFINE FIELD name ON users TYPE string REQUIRED");
    q(&c, "CREATE UNIQUE INDEX users_name ON users (name)");
    (db, c)
}

#[test]
fn content_replaces_fields_and_merge_recurses_without_changing_ids() {
    let (_db, c) = setup();
    q(&c,"INSERT INTO users {id:users:u1,name:'Alice',obsolete:true,profile:{city:'Bangkok',country:'TH'},tags:[1,2]}");
    let replaced = q(
        &c,
        "UPDATE users:u1 CONTENT {name:'Bob',profile:{city:'Paris'}} RETURNING *",
    );
    assert_eq!(replaced.affected, 1);
    let Value::Object(doc) = &replaced.rows[0][0] else {
        panic!()
    };
    assert!(!doc.contains_key("obsolete"));
    assert!(!doc.contains_key("tags"));
    assert_eq!(
        doc["id"],
        Value::Record(Record {
            table: "users".into(),
            key: Key::String("u1".into())
        })
    );
    assert_eq!(
        doc["profile"],
        Value::Object(Document::from([(
            "city".into(),
            Value::String("Paris".into())
        )]))
    );
    q(
        &c,
        "UPDATE users:u1 MERGE {profile:{country:'FR'},tags:[3],nullable:null}",
    );
    assert_eq!(
        q(
            &c,
            "SELECT u.profile.city,u.profile.country,u.tags,u.nullable FROM users u"
        )
        .rows,
        vec![vec![
            Value::String("Paris".into()),
            Value::String("FR".into()),
            Value::Array(vec![Value::Integer(3)]),
            Value::Null
        ]]
    );
    q(&c, "UPDATE users:u1 {profile:{city:'Tokyo'}}");
    assert_eq!(
        q(&c, "SELECT u.profile.country FROM users u").rows,
        vec![vec![Value::Null]]
    );
    let body = Value::Object(Document::from([
        (
            "id".into(),
            Value::Record(Record {
                table: "USERS".into(),
                key: Key::String("u1".into()),
            }),
        ),
        ("name".into(), Value::String("Bound".into())),
        ("bytes".into(), Value::Binary(vec![0, 255])),
        ("large".into(), Value::Integer(i64::MAX)),
        ("vector".into(), Value::vector32(&[1.0, 2.0]).unwrap()),
    ]));
    let params = Parameters::from([("$body".into(), body)]);
    let bound = c
        .execute(
            "UPDATE users CONTENT $body WHERE id=users:u1 RETURNING name,bytes,large,vector",
            &params,
        )
        .unwrap();
    assert_eq!(bound.rows[0][0], Value::String("Bound".into()));
    assert_eq!(bound.rows[0][1], Value::Binary(vec![0, 255]));
    assert_eq!(bound.rows[0][2], Value::Integer(i64::MAX));
    assert_eq!(bound.rows[0][3], Value::vector32(&[1.0, 2.0]).unwrap());
    assert_eq!(
        q(
            &c,
            "UPDATE users:missing CONTENT {name:'Absent'} RETURNING *"
        )
        .affected,
        0
    );
    c.check_collection_integrity("users", Default::default())
        .unwrap();
}

#[test]
fn content_and_merge_failures_restore_documents_indexes_and_prior_work() {
    let (_db, c) = setup();
    q(&c, "INSERT INTO users {id:users:u1,name:'Alice'}");
    q(&c, "INSERT INTO users {id:users:u2,name:'Bob'}");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO users {id:users:prior,name:'Prior'}");
    let original = q(&c, "SELECT * FROM users ORDER BY id").rows;
    for sql in [
        "UPDATE users CONTENT {name:'same'} RETURNING *",
        "UPDATE users MERGE {name:'same'} RETURNING *",
        "UPDATE users:u1 CONTENT {profile:{x:1}}",
        "UPDATE users:u1 CONTENT {id:users:other,name:'Changed'}",
        "UPDATE users:u1 MERGE {id:users:other}",
        "UPDATE users CONTENT 42",
        "UPDATE users MERGE [1,2]",
        "UPDATE users:u1 CONTENT {name:'Changed'} RETURNING record::fetch(id)",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
        assert_eq!(
            c.transaction_state(),
            romedb::TransactionState::Active,
            "{sql}"
        );
        assert_eq!(
            q(&c, "SELECT * FROM users ORDER BY id").rows,
            original,
            "{sql}"
        );
        c.check_collection_integrity("users", Default::default())
            .unwrap();
    }
    let error = c
        .write_with_result_limits(
            "UPDATE users:u1 CONTENT {name:'Changed'} RETURNING *",
            &Parameters::new(),
            romedb::ResultLimits {
                max_rows: 0,
                max_payload_bytes: 1000,
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), "FDB_LIMIT");
    assert_eq!(q(&c, "SELECT * FROM users ORDER BY id").rows, original);
    q(
        &c,
        "UPDATE users MERGE {name:upper(name)} WHERE id=users:u1",
    );
    assert_eq!(
        c.lookup_index("users", "users_name", &Value::String("ALICE".into()))
            .unwrap()
            .len(),
        1
    );
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT count(*) FROM users").rows,
        vec![vec![Value::Integer(2)]]
    );
}
#[test]
fn content_and_merge_keep_every_index_atomic_across_rollback_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("content.db");
    let probes=[
        "SELECT id FROM docs WHERE uid=1",
        "SELECT id FROM docs WHERE author=writers:one",
        "SELECT id,score FROM search::text('docs_text','old',10) ORDER BY id",
        "SELECT id,distance FROM search::vector('docs_vec',vector32('[1,0]'),2) ORDER BY distance,id",
        "SELECT id FROM search::near('docs_geo',geo::point(0,0),100) ORDER BY id",
    ];
    {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c,"INSERT INTO docs {id:docs:a,uid:1,title:'old alpha',author:writers:one,v:vector32('[1,0]'),location:geo::point(0,0)}");
        q(&c,"INSERT INTO docs {id:docs:b,uid:2,title:'old beta',author:writers:two,v:vector32('[0,1]'),location:geo::point(2,2)}");
        for sql in [
            "CREATE UNIQUE INDEX docs_uid ON docs(uid)",
            "CREATE INDEX docs_author ON docs(author)",
            "CREATE SEARCH INDEX docs_text ON docs(title) USING FULLTEXT",
            "CREATE SEARCH INDEX docs_vec ON docs(v) USING VECTOR WITH (metric='l2',dimensions=2)",
            "CREATE SEARCH INDEX docs_geo ON docs(location) USING SPATIAL",
            "CREATE TABLE prior(n INTEGER)",
            "BEGIN",
            "INSERT INTO prior VALUES(9)",
        ] {
            q(&c, sql);
        }
        let original = q(&c, "SELECT * FROM docs ORDER BY id").rows;
        let expected = probes.map(|sql| q(&c, sql).rows);
        assert!(c.execute("UPDATE docs CONTENT {uid:1,title:'changed',author:writers:other,v:vector32('[5,5]'),location:geo::point(1,1)}",&Parameters::new()).is_err());
        for sql in [
            "UPDATE docs CONTENT {uid:uid,title:'interrupted',author:author,v:v,location:location}",
            "UPDATE docs MERGE {title:'interrupted'}",
            "UPDATE docs PATCH [{op:'replace',path:'/title',value:'interrupted'}]",
        ] {
            let report = c.write_metered(
                sql,
                &Parameters::new(),
                romedb::ResultLimits {
                    max_rows: 100,
                    max_payload_bytes: 65536,
                },
                romedb::WriteWorkLimits {
                    max_row_mutations: Some(1),
                    ..Default::default()
                },
            );
            assert!(report.outcome.is_err(), "{sql}");
            assert!(report.work.mutation_budget_exhausted, "{sql}");
            assert_eq!(report.work.row_mutations, 2, "{sql}");
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
            assert_eq!(q(&c, "SELECT * FROM docs ORDER BY id").rows, original);
            for (probe, expected) in probes.iter().zip(&expected) {
                assert_eq!(&q(&c, probe).rows, expected, "{probe}");
            }
            c.check_collection_integrity("docs", Default::default())
                .unwrap();
            assert_eq!(
                q(&c, "SELECT n FROM prior").rows,
                vec![vec![Value::Integer(9)]]
            );
        }
        q(&c,"UPDATE docs:a MERGE {title:'fresh',author:writers:changed,v:vector32('[5,5]'),location:geo::point(1,1)}");
        assert!(q(&c, probes[1]).rows.is_empty());
        assert!(q(&c, probes[4]).rows.is_empty());
        assert_eq!(
            q(&c, "SELECT id FROM search::text('docs_text','fresh',10)")
                .rows
                .len(),
            1
        );
        q(&c, "ROLLBACK");
        for (probe, expected) in probes.iter().zip(&expected) {
            assert_eq!(&q(&c, probe).rows, expected, "{probe}");
        }
        let reader = db.connect().unwrap();
        q(&reader, "BEGIN");
        for (probe, expected) in probes.iter().zip(&expected) {
            assert_eq!(&q(&reader, probe).rows, expected, "{probe}");
        }
        q(&c,"UPDATE docs:a CONTENT {uid:1,title:'fresh',author:writers:changed,v:vector32('[5,5]'),location:geo::point(1,1)}");
        for (probe, expected) in probes.iter().zip(&expected) {
            assert_eq!(&q(&reader, probe).rows, expected, "{probe}");
        }
        q(&reader, "COMMIT");
        assert_eq!(
            q(
                &reader,
                "SELECT id FROM search::text('docs_text','fresh',10)"
            )
            .rows
            .len(),
            1
        );
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
    }
    let db = Database::open(file.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        q(&c, "SELECT id FROM search::text('docs_text','fresh',10)")
            .rows
            .len(),
        1
    );
    assert!(q(&c, probes[1]).rows.is_empty());
    assert!(q(&c, probes[4]).rows.is_empty());
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn column_list_insert_and_multirow_updates_use_pre_update_values() {
    let (_db, c) = setup();
    let rows=q(&c,"INSERT INTO users (id, name, a, b) VALUES (users:u1, 'Alice', 1, 2), (users:u2, 'Bob', 3, 4) RETURNING *");
    assert_eq!(rows.affected, 2);
    let params = Parameters::from([("$key".into(), Value::String("u3".into()))]);
    c.execute(
        "INSERT INTO users (id, name, a, b) VALUES (type::record('users', $key), 'Carol', 5, 6)",
        &params,
    )
    .expect("dynamic id");
    q(&c, "UPDATE users SET a=b, b=a WHERE a < 5 RETURNING *");
    assert_eq!(
        q(&c, "SELECT a, b FROM users WHERE id = users:u1").rows,
        vec![vec![Value::Integer(2), Value::Integer(1)]]
    );
    assert_eq!(
        q(&c, "SELECT a, b FROM users WHERE id = users:u2").rows,
        vec![vec![Value::Integer(4), Value::Integer(3)]]
    );
    assert_eq!(
        q(&c, "SELECT a, b FROM users WHERE id = users:u3").rows,
        vec![vec![Value::Integer(5), Value::Integer(6)]]
    );
    q(&c, "UPDATE users SET name=upper(name)");
    assert_eq!(
        c.lookup_index("users", "users_name", &Value::String("ALICE".into()))
            .expect("updated index")
            .len(),
        1
    );
    let deleted = q(&c, "DELETE FROM users WHERE a < 5 RETURNING *");
    assert_eq!(deleted.affected, 2);
    assert!(c
        .lookup_index("users", "users_name", &Value::String("ALICE".into()))
        .expect("deleted index")
        .is_empty());
}
#[test]
fn failed_statements_rollback_all_rows_and_keep_outer_transaction() {
    let (_db, c) = setup();
    assert!(c
        .execute(
            "INSERT INTO users (id,name) VALUES (users:u1,'same'),(users:u2,'same')",
            &Parameters::new()
        )
        .is_err());
    assert!(q(&c, "SELECT * FROM users").rows.is_empty());
    q(
        &c,
        "INSERT INTO users (id,name) VALUES (users:u1,'Alice'),(users:u2,'Bob')",
    );
    q(&c, "CREATE TABLE accounts (id INTEGER)");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO accounts VALUES (42)");
    assert!(c
        .execute("UPDATE users SET name='duplicate'", &Parameters::new())
        .is_err());
    assert_eq!(
        q(&c, "SELECT name FROM users ORDER BY name").rows,
        vec![
            vec![Value::String("Alice".into())],
            vec![Value::String("Bob".into())]
        ]
    );
    assert_eq!(
        q(&c, "SELECT id FROM accounts").rows,
        vec![vec![Value::Integer(42)]]
    );
    q(&c, "DELETE FROM users");
    q(&c, "ROLLBACK");
    assert_eq!(q(&c, "SELECT * FROM users").rows.len(), 2);
    assert!(q(&c, "SELECT * FROM accounts").rows.is_empty());
}
#[test]
fn typed_parameters_and_validation_cannot_be_bypassed() {
    let (_db, c) = setup();
    q(&c, "DEFINE FIELD active ON users TYPE boolean");
    let params = Parameters::from([
        (
            "$id".into(),
            Value::Record(Record {
                table: "users".into(),
                key: Key::String("typed".into()),
            }),
        ),
        ("$active".into(), Value::Boolean(true)),
    ]);
    c.execute(
        "INSERT INTO users (id,name,active) VALUES ($id,'Typed',$active)",
        &params,
    )
    .expect("typed input");
    assert_eq!(
        q(&c, "SELECT active FROM users").rows,
        vec![vec![Value::Boolean(true)]]
    );
    let profile = Value::Object(Document::from([(
        "city".into(),
        Value::String("Bangkok".into()),
    )]));
    let params = Parameters::from([("$profile".into(), profile.clone())]);
    c.execute("UPDATE users SET profile=$profile", &params)
        .expect("typed object assignment");
    assert_eq!(q(&c, "SELECT profile FROM users").rows, vec![vec![profile]]);
    for sql in [
        "UPDATE users SET id = users:other",
        "UPDATE users SET name = 42",
        "UPDATE users SET active = 1",
        "INSERT INTO users (id,name) VALUES (1,'bad')",
        "INSERT INTO users (id,name,active) VALUES (users:u2,'x',1) ON CONFLICT DO NOTHING",
        "UPDATE users SET name='x', name='y'",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert!(q(
        &c,
        "INSERT INTO users (name) VALUES ('Typed') ON CONFLICT(name) DO NOTHING RETURNING *"
    )
    .rows
    .is_empty());
    assert_eq!(
        q(&c, "SELECT name FROM users").rows,
        vec![vec![Value::String("Typed".into())]]
    );
}
#[test]
fn record_expression_projection_stays_typed_and_ordinary_sql_stays_relational() {
    let (_db, c) = setup();
    q(&c, "INSERT INTO users (id,name) VALUES (users:u1,'Alice')");
    let record = Value::Record(Record {
        table: "users".into(),
        key: Key::Integer(123),
    });
    assert_eq!(
        q(&c, "SELECT users:123 AS reference FROM users").rows,
        vec![vec![record]]
    );
    q(
        &c,
        "CREATE TABLE ordinary (id INTEGER PRIMARY KEY, name TEXT)",
    );
    q(&c, "INSERT INTO ordinary (id,name) VALUES (1,'x'),(2,'y')");
    q(&c, "UPDATE ordinary SET name=upper(name)");
    assert_eq!(
        q(&c, "DELETE FROM ordinary WHERE id=1 RETURNING id").rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn nested_set_unset_and_overlap_rules() {
    let (_db, c) = setup();
    q(&c,"INSERT INTO users {id: users:u1, name: 'Alice', profile: {city: 'Bangkok', zip: 10110}, a: 1, b: 2}");
    q(&c, "CREATE INDEX users_city ON users (profile.city)");
    q(
        &c,
        "UPDATE users:u1 SET profile.city='Berlin', a=b, b=a RETURNING *",
    );
    assert_eq!(
        q(
            &c,
            "SELECT u.profile.city AS city, u.profile.zip AS zip, a, b FROM users u"
        )
        .rows,
        vec![vec![
            Value::String("Berlin".into()),
            Value::Integer(10110),
            Value::Integer(2),
            Value::Integer(1)
        ]]
    );
    assert_eq!(
        c.lookup_index("users", "users_city", &Value::String("Berlin".into()))
            .expect("nested index")
            .len(),
        1
    );
    q(&c, "UPDATE users SET extra.deep.leaf=42 WHERE id=users:u1");
    q(
        &c,
        "UPDATE users SET \"profile.city\"='literal', profile.city='Paris'",
    );
    q(
        &c,
        "UPDATE users:u1 UNSET profile.city, missing.deep RETURNING *",
    );
    let row = q(&c, "SELECT * FROM users").exactly_one().expect("one");
    let Value::Object(doc) = &row[0] else {
        panic!("document");
    };
    let Value::Object(profile) = &doc["profile"] else {
        panic!("profile");
    };
    assert!(!profile.contains_key("city"));
    assert!(profile.contains_key("zip"));
    assert_eq!(doc["profile.city"], Value::String("literal".into()));
    assert!(c
        .lookup_index("users", "users_city", &Value::String("Paris".into()))
        .expect("unset index")
        .is_empty());
    for sql in [
        "UPDATE users SET profile=1, profile.city='x'",
        "UPDATE users SET name.first='x'",
        "UPDATE users UNSET name",
        "UPDATE users UNSET id",
        "UPDATE users SET a=1, a=2",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert_eq!(
        q(&c, "SELECT name FROM users").rows,
        vec![vec![Value::String("Alice".into())]]
    );
}

#[test]
fn standalone_record_constructors_and_parameter_names() {
    let (_db, c) = setup();
    let params = Parameters::from([("$key".into(), Value::Integer(i64::MIN))]);
    let value = Value::Record(Record {
        table: "users".into(),
        key: Key::Integer(i64::MIN),
    });
    assert_eq!(
        c.execute("SELECT type::record('users', $key) AS id", &params)
            .expect("constructor")
            .rows,
        vec![vec![value]]
    );
    assert_eq!(
        q(&c, "SELECT users:123 AS id").rows,
        vec![vec![Value::Record(Record {
            table: "users".into(),
            key: Key::Integer(123)
        })]]
    );
    for sql in [
        "SELECT type::record('', 1)",
        "SELECT type::record('users', '')",
        "SELECT type::record('users', 1.5)",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    let params = Parameters::from([("$name::suffix".into(), Value::Integer(7))]);
    assert_eq!(
        c.execute("SELECT $name::suffix", &params).unwrap().rows,
        vec![vec![Value::Integer(7)]]
    );
    q(
        &c,
        "INSERT INTO users (id,name,n) VALUES (users:u1,'Alice',10),(users:u2,'Bob',2)",
    );
    let rows = q(
        &c,
        "SELECT type::record('users', n) AS r FROM users ORDER BY r",
    )
    .rows;
    assert_eq!(
        rows,
        vec![
            vec![Value::Record(Record {
                table: "users".into(),
                key: Key::Integer(2)
            })],
            vec![Value::Record(Record {
                table: "users".into(),
                key: Key::Integer(10)
            })]
        ]
    );
}

#[test]
fn indexed_and_anonymous_parameters_keep_statement_numbering() {
    let (_db, c) = setup();
    let params = Parameters::from([
        (
            "?1".into(),
            Value::Record(Record {
                table: "users".into(),
                key: Key::String("numbered".into()),
            }),
        ),
        ("?2".into(), Value::String("Alice".into())),
    ]);
    c.execute("INSERT INTO users (id,name) VALUES (?,?)", &params)
        .expect("anonymous slots");
    let params = Parameters::from([
        ("?1".into(), Value::String("Bob".into())),
        ("?2".into(), Value::String("Alice".into())),
    ]);
    c.execute("UPDATE users SET name=?1 WHERE name=?2", &params)
        .expect("numbered update");
    let params = Parameters::from([
        ("?1".into(), Value::Integer(1)),
        ("?2".into(), Value::Integer(2)),
    ]);
    assert_eq!(
        c.execute("SELECT ?1 + ?2", &params)
            .expect("ordinary positional SQL")
            .rows,
        vec![vec![Value::Integer(3)]]
    );
    assert_eq!(
        q(&c, "SELECT name FROM users").rows,
        vec![vec![Value::String("Bob".into())]]
    );
}

#[test]
fn explicit_tuple_updates_preserve_snapshots_and_atomic_validation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(a INTEGER,b INTEGER)",
        "INSERT INTO native VALUES(1,2),(3,4)",
        "CREATE TABLE docs",
        "INSERT INTO docs(a,b) SELECT a,b FROM native",
        "DEFINE FIELD a ON docs TYPE integer CHECK(a<10)",
        "CREATE UNIQUE INDEX docs_a ON docs(a)",
    ] {
        q(&c, sql);
    }
    for assignment in ["(a,b)=(b,a)", "(a,b)=(a+1,b+2)", "(a,b)=(SELECT b,a)"] {
        for source in ["native", "docs"] {
            q(&c, &format!("UPDATE {source} SET {assignment}"));
        }
        assert_eq!(
            q(&c, "SELECT a,b FROM docs ORDER BY a").rows,
            q(&c, "SELECT a,b FROM native ORDER BY a").rows
        );
    }
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs(a,b) VALUES(0,0)");
    let before = q(&c, "SELECT a,b FROM docs ORDER BY a").rows;
    let error = c
        .execute("UPDATE docs SET (a,b)=(SELECT a+6,b+1)", &Parameters::new())
        .unwrap_err();
    assert_eq!(error.code(), "FDB_VALIDATION");
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(q(&c, "SELECT a,b FROM docs ORDER BY a").rows, before);
    assert!(c
        .lookup_index("docs", "docs_a", &Value::Integer(6))
        .unwrap()
        .is_empty());
    let params = Parameters::from([
        ("$a".into(), Value::Integer(7)),
        ("$b".into(), Value::Integer(8)),
    ]);
    assert_eq!(
        c.execute(
            "UPDATE docs SET (a,b)=($a,$b) WHERE a=0 RETURNING a,b",
            &params
        )
        .unwrap()
        .rows,
        vec![vec![Value::Integer(7), Value::Integer(8)]]
    );
    for sql in [
        "UPDATE docs SET (a,b)=(1,2),a=3",
        "UPDATE docs SET (id,a)=(docs:other,1)",
        "UPDATE docs SET (a,b)=(1,2,3)",
        "UPDATE docs SET (a,b)=(SELECT b,a ORDER BY 3)",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert_eq!(
        c.check_collection_integrity("docs", romedb::IntegrityLimits::default())
            .unwrap()
            .index_entries,
        3
    );
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT a,b FROM docs ORDER BY a").rows,
        q(&c, "SELECT a,b FROM native ORDER BY a").rows
    );
}

#[test]
fn tuple_updates_preserve_typed_values_and_mixed_assignment_snapshots() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(a,b,c) VALUES(1,2,3)");
    let record = Value::Record(Record {
        table: "docs".into(),
        key: Key::String("target".into()),
    });
    let object = Value::Object(Document::from([(
        "nested".into(),
        Value::Array(vec![Value::Boolean(true), Value::Binary(vec![0, 255])]),
    )]));
    let params = Parameters::from([("?1".into(), record.clone()), ("?2".into(), object.clone())]);
    let result = c
        .execute("UPDATE docs SET (a,b)=(?1,?2),c=a RETURNING a,b,c", &params)
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![record.clone(), object.clone(), Value::Integer(1)]]
    );
    assert_eq!(
        q(&c, "UPDATE docs SET (a,b)=(b,a),c=b RETURNING a,b,c").rows,
        vec![vec![object.clone(), record.clone(), object.clone()]]
    );
    let before = q(&c, "SELECT a,b,c FROM docs").rows;
    assert_eq!(
        c.execute(
            "UPDATE docs SET (a,b)=(?1,?2)",
            &Parameters::from([("?1".into(), Value::Null)])
        )
        .unwrap_err()
        .code(),
        "FDB_PARAMETER"
    );
    assert_eq!(q(&c, "SELECT a,b,c FROM docs").rows, before);
    let empty = c
        .execute(
            "UPDATE docs SET (a,b)=(?1,?2) WHERE 0 RETURNING a,b",
            &params,
        )
        .unwrap();
    assert_eq!(empty.columns, vec!["a", "b"]);
    assert!(empty.rows.is_empty());
    assert_eq!(empty.affected, 0);
}

#[test]
fn source_free_tuple_subqueries_match_native_snapshots_and_empty_rows() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE native(a INTEGER,b INTEGER)");
    q(&c, "INSERT INTO native VALUES(1,2),(3,4)");
    for (rhs, expected) in [
        (
            "(SELECT b,a)",
            vec![
                vec![Value::Integer(2), Value::Integer(1)],
                vec![Value::Integer(4), Value::Integer(3)],
            ],
        ),
        (
            "(SELECT 8,9 WHERE 0)",
            vec![
                vec![Value::Null, Value::Null],
                vec![Value::Null, Value::Null],
            ],
        ),
    ] {
        q(&c, "BEGIN");
        assert_eq!(
            q(&c, &format!("UPDATE native SET (a,b)={rhs} RETURNING a,b")).rows,
            expected
        );
        q(&c, "ROLLBACK");
    }
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(a,b) SELECT a,b FROM native");
    assert_eq!(
        q(&c, "UPDATE docs SET (a,b)=(SELECT b,a) RETURNING a,b").rows,
        vec![
            vec![Value::Integer(2), Value::Integer(1)],
            vec![Value::Integer(4), Value::Integer(3)]
        ]
    );
    assert_eq!(
        q(
            &c,
            "UPDATE docs SET (a,b)=(SELECT 8,9 WHERE 0) RETURNING a,b"
        )
        .rows,
        vec![
            vec![Value::Null, Value::Null],
            vec![Value::Null, Value::Null]
        ]
    );
}

#[test]
fn tuple_select_parameters_and_conditional_rows_keep_types() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs(n) VALUES(1),(2)");
    let record = Value::Record(Record {
        table: "docs".into(),
        key: Key::Integer(i64::MAX),
    });
    let object = Value::Object(Document::from([(
        "values".into(),
        Value::Array(vec![Value::Boolean(true), Value::Binary(vec![0, 255])]),
    )]));
    let params = Parameters::from([("$r".into(), record.clone()), ("$o".into(), object.clone())]);
    assert_eq!(
        c.execute(
            "UPDATE docs SET (a,b)=(SELECT $r,$o WHERE n=1) RETURNING n,a,b",
            &params
        )
        .unwrap()
        .rows,
        vec![
            vec![Value::Integer(1), record.clone(), object.clone()],
            vec![Value::Integer(2), Value::Null, Value::Null]
        ]
    );
    assert_eq!(
        q(
            &c,
            "UPDATE docs SET (a,b)=(SELECT b,a),n=n+1 WHERE n=1 RETURNING a,b,n"
        )
        .rows,
        vec![vec![object, record, Value::Integer(2)]]
    );
    let before = q(&c, "SELECT a,b,n FROM docs").rows;
    assert_eq!(
        c.execute("UPDATE docs SET (a,b)=(SELECT $r,$o)", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_PARAMETER"
    );
    assert_eq!(q(&c, "SELECT a,b,n FROM docs").rows, before);
}

#[test]
fn tuple_predicate_binding_preserves_quoted_fields_and_nested_scope() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER, a INTEGER, b INTEGER, \"true\" INTEGER)",
        "INSERT INTO native VALUES(1,2,3,7),(2,4,5,8)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b,\"true\") SELECT n,a,b,\"true\" FROM native",
    ] {
        q(&c, sql);
    }
    for tuple in [
        "(SELECT b,a WHERE n=1)",
        "(SELECT \"true\",a WHERE \"true\"=7)",
        "(SELECT b,a WHERE n IN (SELECT 1))",
        "(SELECT b,a WHERE EXISTS(SELECT 1 FROM native x WHERE x.n=1))",
    ] {
        q(&c, "BEGIN");
        let expected = q(
            &c,
            &format!("UPDATE native SET (a,b)={tuple} RETURNING n,a,b"),
        )
        .rows;
        assert_eq!(
            q(
                &c,
                &format!("UPDATE docs SET (a,b)={tuple} RETURNING n,a,b")
            )
            .rows,
            expected,
            "{tuple}"
        );
        q(&c, "ROLLBACK");
    }
}

#[test]
fn tuple_select_bound_pagination_matches_native_empty_rows() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(a INTEGER,b INTEGER)",
        "INSERT INTO native VALUES(1,2),(3,4)",
        "CREATE TABLE docs",
        "INSERT INTO docs(a,b) SELECT a,b FROM native",
    ] {
        q(&c, sql);
    }
    for (limit, offset) in [(0, 0), (1, 0), (1, 1), (-1, 0), (-1, 2)] {
        q(&c, "BEGIN");
        let params = Parameters::from([
            ("$limit".into(), Value::Integer(limit)),
            ("$offset".into(), Value::Integer(offset)),
        ]);
        let query = |source| {
            format!(
                "UPDATE {source} SET (a,b)=(SELECT b,a LIMIT $limit OFFSET $offset) RETURNING a,b"
            )
        };
        let native = query("native")
            .replace("$limit", &limit.to_string())
            .replace("$offset", &offset.to_string());
        let expected = c.execute(&native, &Parameters::new()).unwrap();
        let actual = c.execute(&query("docs"), &params).unwrap();
        assert_eq!(actual.rows, expected.rows, "{limit}/{offset}");
        assert_eq!(actual.affected, expected.affected);
        q(&c, "ROLLBACK");
    }
    let before = q(&c, "SELECT a,b FROM docs ORDER BY a").rows;
    assert_eq!(
        c.execute(
            "UPDATE docs SET (a,b)=(SELECT b,a LIMIT $limit)",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_PARAMETER"
    );
    assert_eq!(q(&c, "SELECT a,b FROM docs ORDER BY a").rows, before);
}

#[test]
fn tuple_select_keeps_each_candidates_typed_snapshot() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    let mut expected = Vec::new();
    for n in 1..=3 {
        let record = Value::Record(Record {
            table: "docs".into(),
            key: Key::Integer(n),
        });
        let bytes = Value::Binary(vec![n as u8, 0, 255]);
        let params = Parameters::from([
            ("$n".into(), Value::Integer(n)),
            ("$a".into(), record.clone()),
            ("$b".into(), bytes.clone()),
        ]);
        c.execute("INSERT INTO docs(n,a,b) VALUES($n,$a,$b)", &params)
            .unwrap();
        expected.push(vec![Value::Integer(n), bytes, record]);
    }
    q(&c, "BEGIN");
    assert_eq!(
        q(&c, "UPDATE docs SET (a,b)=(SELECT b,a) RETURNING n,a,b").rows,
        expected
    );
    assert_eq!(q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows, expected);
    q(&c, "ROLLBACK");
    for row in &mut expected {
        row.swap(1, 2);
    }
    assert_eq!(q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows, expected);
}

#[test]
fn sourceful_tuple_select_lookups_match_native_rows() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO native VALUES(1,0,0),(2,0,0),(3,0,0)",
        "CREATE TABLE lookup(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO lookup VALUES(1,2,3),(2,4,5)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
    ] {
        q(&c, sql);
    }
    q(&c, "CREATE TABLE lookup_docs");
    q(
        &c,
        "INSERT INTO lookup_docs(n,a,b) SELECT n,a,b FROM lookup",
    );
    for source in [
        "lookup",
        "lookup_docs",
        "(SELECT n,a,b FROM lookup)",
        "(SELECT n,a,b FROM lookup_docs)",
    ] {
        q(&c, "BEGIN");
        let expected=q(&c,"UPDATE native SET (a,b)=(SELECT x.a,x.b FROM lookup x WHERE x.n=native.n) RETURNING n,a,b");
        assert_eq!(q(&c,&format!("UPDATE docs SET (a,b)=(SELECT x.a,x.b FROM {source} x WHERE x.n=docs.n) RETURNING n,a,b")).rows,expected.rows);
        q(&c, "ROLLBACK");
    }
}

#[test]
fn tuple_lookup_validation_restores_indexes_and_allows_retry() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) VALUES(1,1,0),(2,2,0)",
        "DEFINE FIELD a ON docs TYPE integer CHECK(a<10)",
        "CREATE UNIQUE INDEX docs_a ON docs(a)",
        "CREATE TABLE lookup(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO lookup VALUES(1,7,8),(2,11,12)",
        "CREATE TABLE lookup_docs",
        "INSERT INTO lookup_docs(n,a,b) SELECT n,a,b FROM lookup",
    ] {
        q(&c, sql);
    }
    q(&c, "INSERT INTO lookup VALUES(2,6,9)");
    q(&c, "INSERT INTO lookup_docs(n,a,b) VALUES(2,6,9)");
    for source in ["lookup", "lookup_docs"] {
        for (local, aliases, distinct) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (true, true, false),
            (false, false, true),
            (true, true, true),
        ] {
            q(&c, "BEGIN");
            q(&c, "INSERT INTO docs(n,a,b) VALUES(0,0,0)");
            let before = q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows;
            let sql=format!("UPDATE docs SET (a,b)=(SELECT x.a,x.b FROM {source} x WHERE x.n=docs.n ORDER BY x.a DESC LIMIT 1) WHERE n>0 RETURNING a,b");
            let sql = if local {
                sql.replace(
                    &format!("SELECT x.a,x.b FROM {source} x"),
                    &format!(
                        "WITH chosen AS (SELECT n,a,b FROM {source}) SELECT x.a,x.b FROM chosen x"
                    ),
                )
            } else {
                sql
            };
            let sql = if aliases {
                sql.replace(
                    "SELECT x.a,x.b",
                    "SELECT x.a AS first_value,x.b AS second_value",
                )
            } else {
                sql
            };
            let sql = if distinct {
                sql.replace("SELECT x.a", "SELECT DISTINCT x.a")
            } else {
                sql
            };
            let error = c.execute(&sql, &Parameters::new()).unwrap_err();
            assert_eq!(error.code(), "FDB_VALIDATION", "{sql}: {error:?}");
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
            assert_eq!(q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows, before);
            assert!(c
                .lookup_index("docs", "docs_a", &Value::Integer(7))
                .unwrap()
                .is_empty());
            let ordered_retry = sql.replace("ORDER BY x.a DESC", "ORDER BY x.a ASC");
            assert_eq!(
                q(&c, &ordered_retry).rows,
                vec![
                    vec![Value::Integer(7), Value::Integer(8)],
                    vec![Value::Integer(6), Value::Integer(9)]
                ]
            );
            q(&c, "UPDATE docs SET (a,b)=(n,0) WHERE n>0");
            let retry = sql.replace("WHERE n>0", "WHERE n=1");
            assert_eq!(
                q(&c, &retry).rows,
                vec![vec![Value::Integer(7), Value::Integer(8)]]
            );
            assert_eq!(
                c.check_collection_integrity("docs", Default::default())
                    .unwrap()
                    .index_entries,
                3
            );
            q(&c, "ROLLBACK");
            assert_eq!(
                q(&c, "SELECT a,b FROM docs ORDER BY n").rows,
                vec![
                    vec![Value::Integer(1), Value::Integer(0)],
                    vec![Value::Integer(2), Value::Integer(0)]
                ]
            );
        }
    }
}

#[test]
fn tuple_self_lookups_materialize_before_any_update() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(
        &c,
        "INSERT INTO docs(n,a,b) VALUES(1,10,11),(2,20,21),(3,30,31)",
    );
    q(&c, "CREATE UNIQUE INDEX docs_a ON docs(a)");
    q(&c, "BEGIN");
    let result=q(&c,"UPDATE docs SET (a,b)=(SELECT x.a+100,x.b+100 FROM docs x WHERE x.n=CASE docs.n WHEN 1 THEN 3 ELSE docs.n-1 END) RETURNING n,a,b");
    let expected = vec![
        vec![Value::Integer(1), Value::Integer(130), Value::Integer(131)],
        vec![Value::Integer(2), Value::Integer(110), Value::Integer(111)],
        vec![Value::Integer(3), Value::Integer(120), Value::Integer(121)],
    ];
    assert_eq!(result.rows, expected);
    assert_eq!(q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows, expected);
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .index_entries,
        3
    );
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT a FROM docs ORDER BY n").rows,
        vec![
            vec![Value::Integer(10)],
            vec![Value::Integer(20)],
            vec![Value::Integer(30)]
        ]
    );
}

#[test]
fn tuple_lookup_multiple_matches_keep_columns_from_one_row() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO native VALUES(1,0,0),(2,0,0)",
        "CREATE TABLE lookup(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO lookup VALUES(1,10,11),(1,20,21),(2,30,31)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
    ] {
        q(&c, sql);
    }
    for tuple in [
        "(SELECT docs.a,docs.b FROM lookup docs WHERE docs.n=2)",
        "(SELECT x.a,x.b FROM lookup x WHERE x.n=2)",
    ] {
        q(&c, "BEGIN");
        let native_tuple = tuple
            .replace("docs.", "x.")
            .replace("lookup docs", "lookup x");
        let expected = q(
            &c,
            &format!("UPDATE native SET (a,b)={native_tuple} RETURNING n,a,b"),
        );
        assert_eq!(
            q(
                &c,
                &format!("UPDATE docs SET (a,b)={tuple} RETURNING n,a,b")
            )
            .rows,
            expected.rows,
            "{tuple}"
        );
        q(&c, "ROLLBACK");
    }
    q(&c, "CREATE TABLE lookup_docs");
    q(
        &c,
        "INSERT INTO lookup_docs(n,a,b) SELECT n,a,b FROM lookup",
    );
    for ordering in ["x.a DESC", "x.a+docs.n DESC"] {
        q(&c, "BEGIN");
        let native_order = ordering.replace("docs.n", "native.n");
        let expected=q(&c,&format!("UPDATE native SET (a,b)=(SELECT x.a,x.b FROM lookup x WHERE x.n=native.n ORDER BY {native_order} LIMIT 1) RETURNING n,a,b"));
        assert_eq!(q(&c,&format!("UPDATE docs SET (a,b)=(SELECT x.a,x.b FROM lookup_docs x WHERE x.n=docs.n ORDER BY {ordering} LIMIT 1) RETURNING n,a,b")).rows,expected.rows);
        q(&c, "ROLLBACK");
    }
    for page in [
        "",
        " LIMIT 1",
        " LIMIT 1 OFFSET 1",
        " LIMIT 1 OFFSET 3",
        " ORDER BY x.a DESC LIMIT 1",
        " ORDER BY x.a+x.b DESC LIMIT 1 OFFSET 1",
    ] {
        q(&c, "BEGIN");
        let query = |target| {
            format!("UPDATE {target} SET (a,b)=(SELECT x.a,x.b FROM lookup x WHERE x.n={target}.n{page}) RETURNING n,a,b")
        };
        let expected = q(&c, &query("native"));
        let actual = q(&c, &query("docs"));
        assert_eq!(actual.rows, expected.rows, "{page}");
        assert_eq!(actual.affected, expected.affected);
        q(&c, "ROLLBACK");
    }
}

#[test]
fn tuple_iterator_sources_match_native_values_and_empty_rows() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,j TEXT,a INTEGER,b INTEGER)",
        "INSERT INTO native VALUES(1,'[4,5]',0,0),(2,'[]',0,0)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,j,a,b) SELECT n,j,a,b FROM native",
    ] {
        q(&c, sql);
    }
    for iterator in ["json_each", "main.json_each", "json_tree"] {
        q(&c, "BEGIN");
        let query = |target| {
            format!("UPDATE {target} SET (a,b)=(SELECT x.key,x.value FROM {iterator}({target}.j) x WHERE x.type='integer') RETURNING n,a,b")
        };
        let expected = q(&c, &query("native"));
        assert_eq!(q(&c, &query("docs")).rows, expected.rows, "{iterator}");
        q(&c, "ROLLBACK");
    }
}

#[test]
fn ordered_tuple_lookup_collation_and_nulls_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a TEXT,b INTEGER)",
        "INSERT INTO native VALUES(1,NULL,0)",
        "CREATE TABLE lookup(a TEXT,b INTEGER)",
        "INSERT INTO lookup VALUES('a',1),('B',2),(NULL,3)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
        "CREATE TABLE lookup_docs",
        "INSERT INTO lookup_docs(a,b) SELECT a,b FROM lookup",
    ] {
        q(&c, sql);
    }
    for source in ["lookup", "lookup_docs"] {
        for order in [
            "x.a COLLATE BINARY",
            "x.a COLLATE NOCASE",
            "x.a COLLATE NOCASE DESC",
            "x.a NULLS LAST",
            "x.a DESC NULLS FIRST",
        ] {
            q(&c, "BEGIN");
            let expected=q(&c,&format!("UPDATE native SET (a,b)=(SELECT x.a,x.b FROM lookup x ORDER BY {order} LIMIT 1) RETURNING a,b"));
            assert_eq!(q(&c,&format!("UPDATE docs SET (a,b)=(SELECT x.a,x.b FROM {source} x ORDER BY {order} LIMIT 1) RETURNING a,b")).rows,expected.rows,"{source}: {order}");
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn ordered_tuple_lookup_reuses_bound_projection_and_pagination_values() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs(n) VALUES(1)",
        "CREATE TABLE lookup(n INTEGER,a INTEGER)",
        "INSERT INTO lookup VALUES(1,10),(1,20)",
    ] {
        q(&c, sql);
    }
    let sql="UPDATE docs SET (a,b)=(SELECT x.a+$limit,$offset FROM lookup x WHERE x.n=docs.n ORDER BY x.a DESC LIMIT $limit OFFSET $offset) RETURNING a,b";
    for (limit, offset, expected) in [
        (1, 0, vec![Value::Integer(21), Value::Integer(0)]),
        (1, 1, vec![Value::Integer(11), Value::Integer(1)]),
        (0, 0, vec![Value::Null, Value::Null]),
        (1, 2, vec![Value::Null, Value::Null]),
    ] {
        q(&c, "BEGIN");
        let params = Parameters::from([
            ("$limit".into(), Value::Integer(limit)),
            ("$offset".into(), Value::Integer(offset)),
        ]);
        assert_eq!(c.execute(sql, &params).unwrap().rows, vec![expected]);
        q(&c, "ROLLBACK");
    }
    assert_eq!(
        c.execute(sql, &Parameters::new()).unwrap_err().code(),
        "FDB_PARAMETER"
    );
    assert_eq!(
        q(&c, "SELECT a,b FROM docs").rows,
        vec![vec![Value::Null, Value::Null]]
    );
}

#[test]
fn tuple_join_sources_preserve_outer_join_nulls() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO native VALUES(1,0,0),(2,0,0),(3,0,0)",
        "CREATE TABLE lhs(n INTEGER,a INTEGER)",
        "INSERT INTO lhs VALUES(1,10),(2,20)",
        "CREATE TABLE rhs(n INTEGER,b INTEGER)",
        "INSERT INTO rhs VALUES(1,11)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
        "CREATE TABLE lhs_docs",
        "INSERT INTO lhs_docs(n,a) SELECT n,a FROM lhs",
    ] {
        q(&c, sql);
    }
    for join in ["JOIN", "LEFT JOIN"] {
        for source in ["lhs", "lhs_docs"] {
            for projection in ["x.a,y.b", "x.a+1,coalesce(y.b,-1)"] {
                q(&c, "BEGIN");
                let expected=q(&c,&format!("UPDATE native SET (a,b)=(SELECT {projection} FROM lhs x {join} rhs y ON y.n=x.n WHERE x.n=native.n) RETURNING n,a,b"));
                assert_eq!(q(&c,&format!("UPDATE docs SET (a,b)=(SELECT {projection} FROM {source} x {join} rhs y ON y.n=x.n WHERE x.n=docs.n) RETURNING n,a,b")).rows,expected.rows,"{source}: {join}");
                q(&c, "ROLLBACK");
            }
        }
    }
}

#[test]
fn tuple_local_ctes_supply_correlated_lookup_rows() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO native VALUES(1,0,0),(2,0,0)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
        "CREATE TABLE lookup(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO lookup VALUES(1,4,5)",
        "CREATE TABLE lookup_docs",
        "INSERT INTO lookup_docs(n,a,b) SELECT n,a,b FROM lookup",
    ] {
        q(&c, sql);
    }
    for source in ["lookup", "lookup_docs"] {
        for definition in [
            "chosen AS (SELECT n,a,b FROM SOURCE)",
            "base(k,v,w) AS (SELECT n,a,b FROM SOURCE), chosen AS (SELECT k AS n,v AS a,w AS b FROM base)",
        ] {
        q(&c, "BEGIN");
        let expected=q(&c,"UPDATE native SET (a,b)=(WITH chosen AS (SELECT n,a,b FROM lookup) SELECT x.a,x.b FROM chosen x WHERE x.n=native.n) RETURNING n,a,b");
        let definition=definition.replace("SOURCE",source);
        assert_eq!(q(&c,&format!("UPDATE docs SET (a,b)=(WITH {definition} SELECT x.a,x.b FROM chosen x WHERE x.n=docs.n) RETURNING n,a,b")).rows,expected.rows);
        q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn tuple_lookup_aliases_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO native VALUES(1,0,0),(2,0,0)",
        "CREATE TABLE lookup(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO lookup VALUES(1,6,9),(1,11,8),(2,4,7)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
        "CREATE TABLE lookup_docs",
        "INSERT INTO lookup_docs(n,a,b) SELECT n,a,b FROM lookup",
    ] {
        q(&c, sql);
    }
    for source in ["lookup", "lookup_docs"] {
        for (projection, order, expected) in [
            ("x.a,x.b", "1 DESC", [[1, 11, 8], [2, 4, 7]]),
            ("x.a,x.b", "2 DESC", [[1, 6, 9], [2, 4, 7]]),
            (
                "x.a AS chosen,x.b AS other",
                "2 DESC",
                [[1, 6, 9], [2, 4, 7]],
            ),
            (
                "x.a AS chosen,x.b AS other",
                "chosen DESC",
                [[1, 11, 8], [2, 4, 7]],
            ),
            ("-x.a AS a,x.b other", "a DESC", [[1, -6, 9], [2, -4, 7]]),
            (
                "x.a AS chosen,x.b",
                "chosen + x.b DESC",
                [[1, 11, 8], [2, 4, 7]],
            ),
            (
                "x.a AS chosen,x.b AS chosen",
                "chosen DESC",
                [[1, 11, 8], [2, 4, 7]],
            ),
            (
                "x.a AS romedb_tuple_values_0,x.b AS other",
                "romedb_tuple_values_0 DESC",
                [[1, 11, 8], [2, 4, 7]],
            ),
        ] {
            q(&c, "BEGIN");
            let native = q(&c, &format!("UPDATE native SET (a,b)=(SELECT {projection} FROM lookup x WHERE x.n=native.n ORDER BY {order} LIMIT 1) RETURNING n,a,b"));
            assert_eq!(
                native.rows,
                expected
                    .into_iter()
                    .map(|row| row.into_iter().map(Value::Integer).collect::<Vec<_>>())
                    .collect::<Vec<_>>()
            );
            let sql = format!("UPDATE docs SET (a,b)=(SELECT {projection} FROM {source} x WHERE x.n=docs.n ORDER BY {order} LIMIT 1) RETURNING n,a,b");
            assert_eq!(q(&c, &sql).rows, native.rows, "{sql}");
            q(&c, "ROLLBACK");
        }
        for order in ["0", "3", "-1"] {
            q(&c, "BEGIN");
            let before = q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows;
            let native = c
                .execute(
                    &format!(
                        "UPDATE native SET (a,b)=(SELECT x.a,x.b FROM lookup x ORDER BY {order})"
                    ),
                    &Parameters::new(),
                )
                .unwrap_err();
            let error = c
                .execute(
                    &format!(
                        "UPDATE docs SET (a,b)=(SELECT x.a,x.b FROM {source} x ORDER BY {order})"
                    ),
                    &Parameters::new(),
                )
                .unwrap_err();
            assert_eq!(native.code(), "FDB_ENGINE");
            assert_eq!(error.code(), "FDB_VALIDATION", "{source}: {order}");
            assert_eq!(q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows, before);
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn source_free_tuple_positions_preserve_candidates_and_empty_rows() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO native VALUES(1,6,11),(2,8,9)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
    ] {
        q(&c, sql);
    }
    for order in ["1", "2 DESC", "(2)"] {
        for tail in ["", " LIMIT 0", " LIMIT 1 OFFSET 1"] {
            q(&c, "BEGIN");
            let expected = q(&c, &format!("UPDATE native SET (a,b)=(SELECT b,a WHERE n=1 ORDER BY {order}{tail}) RETURNING n,a,b"));
            let sql = format!("UPDATE docs SET (a,b)=(SELECT b,a WHERE n=1 ORDER BY {order}{tail}) RETURNING n,a,b");
            assert_eq!(q(&c, &sql).rows, expected.rows, "{sql}");
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn source_free_tuple_aliases_match_native_scope() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO native VALUES(1,6,11),(2,-8,9)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
    ] {
        q(&c, sql);
    }
    for (projection, predicate, order) in [
        ("b AS chosen,a AS other", "chosen>0 AND n=1", "chosen"),
        ("-a AS a,b AS b", "TARGET.a>0", "a DESC"),
        ("b AS chosen,a AS chosen", "chosen>0", "2"),
        ("b AS chosen,a", "chosen IN (11)", "chosen+a"),
    ] {
        q(&c, "BEGIN");
        let native_predicate = predicate.replace("TARGET", "native");
        let expected = q(&c, &format!("UPDATE native SET (a,b)=(SELECT {projection} WHERE {native_predicate} ORDER BY {order}) RETURNING n,a,b"));
        let predicate = predicate.replace("TARGET", "docs");
        let sql = format!("UPDATE docs SET (a,b)=(SELECT {projection} WHERE {predicate} ORDER BY {order}) RETURNING n,a,b");
        assert_eq!(q(&c, &sql).rows, expected.rows, "{sql}");
        q(&c, "ROLLBACK");
    }
    // Source-free logical SELECTs bind explicit aliases locally. Qualify a
    // document field when its name collides with an alias.
    q(&c, "BEGIN");
    assert_eq!(
        q(
            &c,
            "UPDATE docs SET (a,b)=(SELECT -a AS a,b AS b WHERE a>0 ORDER BY a) RETURNING n,a,b"
        )
        .rows,
        vec![
            vec![Value::Integer(1), Value::Null, Value::Null],
            vec![Value::Integer(2), Value::Integer(8), Value::Integer(9)]
        ]
    );
    q(&c, "ROLLBACK");
}

#[test]
fn distinct_tuple_lookups_deduplicate_before_pagination() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO native VALUES(1,0,0),(2,0,0)",
        "CREATE TABLE lookup(n INTEGER,a INTEGER,b INTEGER)",
        "INSERT INTO lookup VALUES(1,6,9),(1,6,9),(1,11,8),(1,11,8)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
        "CREATE TABLE lookup_docs",
        "INSERT INTO lookup_docs(n,a,b) SELECT n,a,b FROM lookup",
    ] {
        q(&c, sql);
    }
    for source in ["lookup", "lookup_docs"] {
        for offset in [0, 1, 2] {
            q(&c, "BEGIN");
            let native = q(&c, &format!("UPDATE native SET (a,b)=(SELECT DISTINCT x.a,x.b FROM lookup x WHERE x.n=native.n ORDER BY 1 LIMIT 1 OFFSET {offset}) RETURNING n,a,b"));
            let sql = format!("UPDATE docs SET (a,b)=(SELECT DISTINCT x.a,x.b FROM {source} x WHERE x.n=docs.n ORDER BY 1 LIMIT 1 OFFSET {offset}) RETURNING n,a,b");
            assert_eq!(q(&c, &sql).rows, native.rows, "{sql}");
            q(&c, "ROLLBACK");
        }
    }
    q(&c, "BEGIN");
    let native = q(
        &c,
        "UPDATE native SET (a,b)=(SELECT DISTINCT b,a WHERE n=1) RETURNING n,a,b",
    );
    assert_eq!(
        q(
            &c,
            "UPDATE docs SET (a,b)=(SELECT DISTINCT b,a WHERE n=1) RETURNING n,a,b"
        )
        .rows,
        native.rows
    );
    q(&c, "ROLLBACK");
}

#[test]
fn aggregate_tuple_lookups_match_native_empty_and_grouped_rows() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a,b)",
        "INSERT INTO native VALUES(1,0,0),(2,0,0)",
        "CREATE TABLE lookup(n INTEGER,a INTEGER)",
        "INSERT INTO lookup VALUES(1,6),(1,11),(1,6)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
        "CREATE TABLE lookup_docs",
        "INSERT INTO lookup_docs(n,a) SELECT n,a FROM lookup",
    ] {
        q(&c, sql);
    }
    for source in ["lookup", "lookup_docs"] {
        for projection in [
            "sum(x.a),count(*)",
            "sum(x.a) FILTER(WHERE x.a>7),count(*) FILTER(WHERE x.a>7)",
            "sum(x.a) FILTER(WHERE x.a>TARGET.n*7),count(*) FILTER(WHERE x.a>TARGET.n*7)",
            "avg(x.a),total(x.a)",
            "min(x.a),max(x.a)",
            "sum(DISTINCT x.a),count(DISTINCT x.a)",
            "coalesce(sum(x.a),0)+1,count(*)*2",
        ] {
            for group in ["", " GROUP BY x.n", " GROUP BY x.n HAVING count(*)>3"] {
                q(&c, "BEGIN");
                let native_projection = projection.replace("TARGET", "native");
                let native = q(&c, &format!("UPDATE native SET (a,b)=(SELECT {native_projection} FROM lookup x WHERE x.n=native.n{group}) RETURNING n,a,b"));
                let projection = projection.replace("TARGET", "docs");
                let sql=format!("UPDATE docs SET (a,b)=(SELECT {projection} FROM {source} x WHERE x.n=docs.n{group}) RETURNING n,a,b");
                assert_eq!(q(&c, &sql).rows, native.rows, "{sql}");
                q(&c, "ROLLBACK");
            }
        }
        for offset in [0, 1, 2] {
            q(&c, "BEGIN");
            let native=q(&c,&format!("UPDATE native SET (a,b)=(SELECT sum(x.a) AS total_value,count(*) AS amount FROM lookup x WHERE x.n=native.n GROUP BY x.a HAVING amount>0 ORDER BY total_value DESC LIMIT 1 OFFSET {offset}) RETURNING n,a,b"));
            let sql=format!("UPDATE docs SET (a,b)=(SELECT sum(x.a) AS total_value,count(*) AS amount FROM {source} x WHERE x.n=docs.n GROUP BY x.a HAVING amount>0 ORDER BY total_value DESC LIMIT 1 OFFSET {offset}) RETURNING n,a,b");
            assert_eq!(q(&c, &sql).rows, native.rows, "{sql}");
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn aggregate_tuple_validation_restores_indexes_and_allows_retry() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) VALUES(1,1,0),(2,2,0)",
        "DEFINE FIELD a ON docs TYPE integer CHECK(a<10)",
        "CREATE UNIQUE INDEX docs_a ON docs(a)",
        "CREATE TABLE lookup",
        "INSERT INTO lookup(n,a) VALUES(1,3),(1,4),(2,5),(2,6)",
    ] {
        q(&c, sql);
    }
    for (filtered, nested, windowed, values) in [
        (false, false, false, false),
        (true, false, false, false),
        (false, true, false, false),
        (false, false, true, false),
        (false, false, false, true),
    ] {
        q(&c, "BEGIN");
        q(&c, "INSERT INTO docs(n,a,b) VALUES(0,0,0)");
        let before = q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows;
        let sql="UPDATE docs SET (a,b)=(SELECT sum(x.a),count(*) FROM lookup x WHERE x.n=docs.n) WHERE n>0 RETURNING a,b";
        let sql = if filtered {
            sql.replace(
                "sum(x.a),count(*)",
                "sum(x.a) FILTER(WHERE x.a>docs.n),count(*) FILTER(WHERE x.a>docs.n)",
            )
        } else {
            sql.to_owned()
        };
        let sql = if nested {
            "UPDATE docs SET (a,b)=(SELECT (SELECT sum(x.a) FROM lookup x WHERE x.n=docs.n),(SELECT count(*) FROM lookup x WHERE x.n=docs.n)) WHERE n>0 RETURNING a,b".to_owned()
        } else {
            sql
        };
        let sql = if windowed {
            sql.replace("sum(x.a),count(*)", "sum(x.a) OVER(),count(*) OVER()")
        } else {
            sql
        };
        let sql = if values {
            "UPDATE docs SET (a,b)=(VALUES((SELECT sum(x.a) FROM lookup x WHERE x.n=docs.n),(SELECT count(*) FROM lookup x WHERE x.n=docs.n))) WHERE n>0 RETURNING a,b".to_owned()
        } else {
            sql
        };
        assert_eq!(
            c.execute(&sql, &Parameters::new()).unwrap_err().code(),
            "FDB_VALIDATION"
        );
        assert_eq!(q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows, before);
        assert!(c
            .lookup_index("docs", "docs_a", &Value::Integer(7))
            .unwrap()
            .is_empty());
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        let retry = sql.replace("x.n=docs.n", "x.n=docs.n AND x.a<6");
        assert_eq!(
            q(&c, &retry).rows,
            vec![
                vec![Value::Integer(7), Value::Integer(2)],
                vec![Value::Integer(5), Value::Integer(1)]
            ]
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            3
        );
        q(&c, "ROLLBACK");
        assert_eq!(q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows, before[1..]);
    }
}

#[test]
fn filtered_tuple_parameters_validate_before_mutation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) VALUES(1,0,0),(2,0,0)",
        "CREATE TABLE lookup",
        "INSERT INTO lookup(n,a) VALUES(1,6),(1,11)",
    ] {
        q(&c, sql);
    }
    let sql="UPDATE docs SET (a,b)=(SELECT sum(x.a) FILTER(WHERE x.a>$minimum),count(*) FILTER(WHERE x.a>$minimum) FROM lookup x WHERE x.n=docs.n) RETURNING n,a,b";
    q(&c, "BEGIN");
    let before = q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows;
    assert_eq!(
        c.execute(sql, &Parameters::new()).unwrap_err().code(),
        "FDB_PARAMETER"
    );
    assert_eq!(q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows, before);
    for (minimum, sum, count) in [
        (0, Value::Integer(17), 2),
        (7, Value::Integer(11), 1),
        (20, Value::Null, 0),
    ] {
        let params = Parameters::from([("$minimum".into(), Value::Integer(minimum))]);
        assert_eq!(
            c.execute(sql, &params).unwrap().rows,
            vec![
                vec![Value::Integer(1), sum, Value::Integer(count)],
                vec![Value::Integer(2), Value::Null, Value::Integer(0)]
            ]
        );
    }
    q(&c, "ROLLBACK");
    assert_eq!(q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows, before);
}

#[test]
fn tuple_aggregate_filter_subqueries_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a,b)",
        "INSERT INTO native VALUES(1,0,0),(2,0,0)",
        "CREATE TABLE lookup(n INTEGER,a INTEGER)",
        "INSERT INTO lookup VALUES(1,6),(1,11)",
        "CREATE TABLE allowed(n INTEGER,a INTEGER)",
        "INSERT INTO allowed VALUES(1,11)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
        "CREATE TABLE lookup_docs",
        "INSERT INTO lookup_docs(n,a) SELECT n,a FROM lookup",
    ] {
        q(&c, sql);
    }
    for source in ["lookup", "lookup_docs"] {
        for filter in [
            "x.a>(SELECT 7)",
            "x.a IN (SELECT a FROM allowed)",
            "EXISTS(SELECT 1 FROM allowed y WHERE y.a=x.a AND y.n=TARGET.n)",
        ] {
            q(&c, "BEGIN");
            let native_filter = filter.replace("TARGET", "native");
            let expected=q(&c,&format!("UPDATE native SET (a,b)=(SELECT sum(x.a) FILTER(WHERE {native_filter}),count(*) FILTER(WHERE {native_filter}) FROM lookup x WHERE x.n=native.n) RETURNING n,a,b"));
            let filter = filter.replace("TARGET", "docs");
            let sql=format!("UPDATE docs SET (a,b)=(SELECT sum(x.a) FILTER(WHERE {filter}),count(*) FILTER(WHERE {filter}) FROM {source} x WHERE x.n=docs.n) RETURNING n,a,b");
            assert_eq!(q(&c, &sql).rows, expected.rows, "{sql}");
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn nested_tuple_projections_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a,b)",
        "INSERT INTO native VALUES(1,0,0),(2,0,0)",
        "CREATE TABLE lookup(n INTEGER,a INTEGER)",
        "INSERT INTO lookup VALUES(1,6),(1,11)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
        "CREATE TABLE lookup_docs",
        "INSERT INTO lookup_docs(n,a) SELECT n,a FROM lookup",
    ] {
        q(&c, sql);
    }
    for source in ["lookup", "lookup_docs"] {
        for projection in [
            "(SELECT max(x.a) FROM SOURCE x WHERE x.n=TARGET.n),TARGET.n+1",
            "EXISTS(SELECT 1 FROM SOURCE x WHERE x.n=TARGET.n),TARGET.n IN (SELECT n FROM SOURCE)",
        ] {
            q(&c, "BEGIN");
            let native_projection = projection
                .replace("SOURCE", "lookup")
                .replace("TARGET", "native");
            let expected = q(
                &c,
                &format!("UPDATE native SET (a,b)=(SELECT {native_projection}) RETURNING n,a,b"),
            );
            let projection = projection
                .replace("SOURCE", source)
                .replace("TARGET", "docs");
            let sql = format!("UPDATE docs SET (a,b)=(SELECT {projection}) RETURNING n,a,b");
            assert_eq!(q(&c, &sql).rows, expected.rows, "{sql}");
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn window_tuple_projections_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a,b)",
        "INSERT INTO native VALUES(1,0,0),(2,0,0)",
        "CREATE TABLE lookup(n INTEGER,a INTEGER)",
        "INSERT INTO lookup VALUES(1,6),(1,11)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
        "CREATE TABLE lookup_docs",
        "INSERT INTO lookup_docs(n,a) SELECT n,a FROM lookup",
    ] {
        q(&c, sql);
    }
    for source in ["lookup", "lookup_docs"] {
        for (projection, window) in [
            ("row_number() OVER(ORDER BY x.a),sum(x.a) OVER()", ""),
            (
                "sum(x.a) FILTER(WHERE x.a>7) OVER(),count(*) FILTER(WHERE x.a>7) OVER()",
                "",
            ),
            (
                "row_number() OVER w,count(*) OVER w",
                " WINDOW w AS (ORDER BY x.a)",
            ),
        ] {
            for offset in [0, 1, 2] {
                q(&c, "BEGIN");
                let expected=q(&c,&format!("UPDATE native SET (a,b)=(SELECT {projection} FROM lookup x WHERE x.n=native.n{window} ORDER BY x.a DESC LIMIT 1 OFFSET {offset}) RETURNING n,a,b"));
                let sql=format!("UPDATE docs SET (a,b)=(SELECT {projection} FROM {source} x WHERE x.n=docs.n{window} ORDER BY x.a DESC LIMIT 1 OFFSET {offset}) RETURNING n,a,b");
                assert_eq!(q(&c, &sql).rows, expected.rows, "{sql}");
                q(&c, "ROLLBACK");
            }
        }
        for offset in [0, 1] {
            q(&c, "BEGIN");
            let expected=q(&c,&format!("UPDATE native SET (a,b)=(SELECT DISTINCT sum(x.a) OVER(),count(*) OVER() FROM lookup x WHERE x.n=native.n ORDER BY 1 LIMIT 1 OFFSET {offset}) RETURNING n,a,b"));
            let sql=format!("UPDATE docs SET (a,b)=(SELECT DISTINCT sum(x.a) OVER(),count(*) OVER() FROM {source} x WHERE x.n=docs.n ORDER BY 1 LIMIT 1 OFFSET {offset}) RETURNING n,a,b");
            assert_eq!(q(&c, &sql).rows, expected.rows, "{sql}");
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn window_tuple_parameters_preserve_binding_and_rollback() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) VALUES(1,0,0),(2,0,0)",
        "CREATE TABLE lookup",
        "INSERT INTO lookup(n,a) VALUES(1,6),(1,11)",
    ] {
        q(&c, sql);
    }
    let sql="UPDATE docs SET (a,b)=(SELECT row_number() OVER(ORDER BY x.a)+$delta,sum(x.a+$delta) OVER() FROM lookup x WHERE x.n=docs.n ORDER BY x.a DESC LIMIT 1) RETURNING n,a,b";
    q(&c, "BEGIN");
    let before = q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows;
    assert_eq!(
        c.execute(sql, &Parameters::new()).unwrap_err().code(),
        "FDB_PARAMETER"
    );
    assert_eq!(q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows, before);
    for delta in [0, 3, -2] {
        let params = Parameters::from([("$delta".into(), Value::Integer(delta))]);
        assert_eq!(
            c.execute(sql, &params).unwrap().rows,
            vec![
                vec![
                    Value::Integer(1),
                    Value::Integer(2 + delta),
                    Value::Integer(17 + 2 * delta)
                ],
                vec![Value::Integer(2), Value::Null, Value::Null]
            ]
        );
    }
    q(&c, "ROLLBACK");
    assert_eq!(q(&c, "SELECT n,a,b FROM docs ORDER BY n").rows, before);
}

#[test]
fn compound_tuple_selects_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a,b)",
        "INSERT INTO native VALUES(1,0,0),(2,0,0)",
        "CREATE TABLE lookup(n INTEGER,a INTEGER)",
        "INSERT INTO lookup VALUES(1,6),(1,11)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
        "CREATE TABLE lookup_docs",
        "INSERT INTO lookup_docs(n,a) SELECT n,a FROM lookup",
    ] {
        q(&c, sql);
    }
    for source in ["lookup", "lookup_docs"] {
        for operator in ["UNION ALL", "UNION", "INTERSECT", "EXCEPT"] {
            q(&c, "BEGIN");
            let body=format!("SELECT x.a,x.n FROM SOURCE x WHERE x.n=TARGET.n {operator} SELECT y.a,y.n FROM SOURCE y WHERE y.n=TARGET.n AND y.a=6 ORDER BY 1 DESC LIMIT 1");
            let native = body.replace("SOURCE", "lookup").replace("TARGET", "native");
            let expected = q(
                &c,
                &format!("UPDATE native SET (a,b)=(WITH selected(a,b) AS ({native}) SELECT a,b FROM selected) RETURNING n,a,b"),
            );
            let body = body.replace("SOURCE", source).replace("TARGET", "docs");
            let sql = format!("UPDATE docs SET (a,b)=({body}) RETURNING n,a,b");
            assert_eq!(q(&c, &sql).rows, expected.rows, "{sql}");
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn values_tuple_assignments_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,a,b)",
        "INSERT INTO native VALUES(1,6,11),(2,3,7)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,a,b) SELECT n,a,b FROM native",
    ] {
        q(&c, sql);
    }
    for body in ["VALUES(12,23)", "VALUES(b,a+1)"] {
        q(&c, "BEGIN");
        let expected = q(
            &c,
            &format!("UPDATE native SET (a,b)=({body}) RETURNING n,a,b"),
        );
        assert_eq!(
            q(
                &c,
                &format!("UPDATE docs SET (a,b)=({body}) RETURNING n,a,b")
            )
            .rows,
            expected.rows
        );
        q(&c, "ROLLBACK");
    }
}

#[test]
fn invalid_write_pagination_preserves_documents() {
    for write in ["UPDATE TARGET SET n=n+10", "DELETE FROM TARGET"] {
        for limit in ["NULL", "1.5", "'invalid'", "1 OFFSET NULL", "1 OFFSET 0.5"] {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            for sql in [
                "CREATE TABLE native(n INTEGER)",
                "INSERT INTO native VALUES(1),(2),(3)",
                "CREATE TABLE docs",
                "INSERT INTO docs(n) SELECT n FROM native",
                "CREATE UNIQUE INDEX docs_n ON docs(n)",
            ] {
                q(&c, sql);
            }
            let native = format!("{} LIMIT {limit}", write.replace("TARGET", "native"));
            let docs = format!("{} LIMIT {limit}", write.replace("TARGET", "docs"));
            let expected = c.execute(&native, &Parameters::new()).unwrap_err();
            let actual = c.execute(&docs, &Parameters::new()).unwrap_err();
            assert_eq!(actual.code(), expected.code(), "{docs}");
            assert_eq!(
                q(&c, "SELECT n FROM docs ORDER BY n").rows,
                q(&c, "SELECT n FROM native ORDER BY n").rows
            );
            assert_eq!(
                c.check_collection_integrity("docs", Default::default())
                    .unwrap()
                    .index_entries,
                3
            );
        }
    }
}

#[test]
fn native_update_from_resolves_duplicate_candidates() {
    for (input, chosen) in [("(1,10),(1,20),(2,30)", 20), ("(1,20),(1,10),(2,30)", 10)] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE target(n INTEGER PRIMARY KEY,v INTEGER)",
            "INSERT INTO target VALUES(1,0),(2,0),(3,0)",
            "CREATE TABLE source(k INTEGER,v INTEGER)",
        ] {
            q(&c, sql);
        }
        q(&c, &format!("INSERT INTO source VALUES {input}"));
        {
            q(&c, "BEGIN");
            let result = q(
                &c,
                "UPDATE target SET v=source.v FROM source WHERE source.k=target.n RETURNING n,v",
            );
            let expected = vec![
                vec![Value::Integer(1), Value::Integer(chosen)],
                vec![Value::Integer(2), Value::Integer(30)],
            ];
            assert_eq!(result.affected, expected.len() as i64);
            assert_eq!(result.rows, expected);
            q(&c, "ROLLBACK");
            assert_eq!(
                q(&c, "SELECT n,v FROM target ORDER BY n").rows,
                vec![
                    vec![Value::Integer(1), Value::Integer(0)],
                    vec![Value::Integer(2), Value::Integer(0)],
                    vec![Value::Integer(3), Value::Integer(0)]
                ]
            );
        }
    }
}

#[test]
fn collection_update_from_resolves_duplicate_candidates() {
    for input in ["(1,10),(1,20),(2,30)", "(1,20),(1,10),(2,30)"] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE native(n INTEGER PRIMARY KEY,v INTEGER)",
            "INSERT INTO native VALUES(1,0),(2,0),(3,0)",
            "CREATE TABLE docs",
            "INSERT INTO docs(n,v) SELECT n,v FROM native",
            "CREATE TABLE source(k INTEGER,v INTEGER)",
        ] {
            q(&c, sql);
        }
        q(&c, &format!("INSERT INTO source VALUES {input}"));
        q(&c, "CREATE TABLE source_docs");
        q(&c, "INSERT INTO source_docs(k,v) SELECT k,v FROM source");
        for source in ["source", "source_docs"] {
            for (prefix, from) in [
                (String::new(), format!("{source} AS s")),
                (String::new(), format!("(SELECT k,v FROM {source}) AS s")),
                (
                    format!("WITH chosen AS (SELECT k,v FROM {source}) "),
                    "chosen AS s".to_owned(),
                ),
            ] {
                q(&c, "BEGIN");
                let sql = format!(
                    "{prefix}UPDATE TARGET SET v=s.v FROM {from} WHERE s.k=TARGET.n RETURNING n,v"
                );
                let expected = q(
                    &c,
                    &sql.replace("TARGET", "native")
                        .replace("source_docs", "source"),
                );
                let actual = q(&c, &sql.replace("TARGET", "docs"));
                assert_eq!(actual.rows, expected.rows, "{sql}");
                assert_eq!(actual.affected, expected.affected);
                q(&c, "ROLLBACK");
            }
        }
    }
}

#[test]
fn update_from_validation_restores_indexes_and_prior_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs(n,v) VALUES(1,1),(2,2)",
        "DEFINE FIELD v ON docs TYPE integer CHECK(v<10)",
        "CREATE UNIQUE INDEX docs_v ON docs(v)",
        "CREATE TABLE source(k INTEGER,v INTEGER)",
        "INSERT INTO source VALUES(1,6),(1,7),(2,11)",
        "BEGIN",
        "INSERT INTO docs(n,v) VALUES(0,0)",
    ] {
        q(&c, sql);
    }
    let before = q(&c, "SELECT n,v FROM docs ORDER BY n").rows;
    let sql = "UPDATE docs SET v=s.v FROM source s WHERE s.k=docs.n RETURNING n,v";
    assert_eq!(
        c.execute(sql, &Parameters::new()).unwrap_err().code(),
        "FDB_VALIDATION"
    );
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(q(&c, "SELECT n,v FROM docs ORDER BY n").rows, before);
    assert!(c
        .lookup_index("docs", "docs_v", &Value::Integer(7))
        .unwrap()
        .is_empty());
    q(&c, "UPDATE source SET v=8 WHERE k=2");
    let result = q(&c, sql);
    assert_eq!(result.affected, 2);
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Integer(1), Value::Integer(7)],
            vec![Value::Integer(2), Value::Integer(8)]
        ]
    );
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .index_entries,
        3
    );
    q(&c, "ROLLBACK");
    assert_eq!(q(&c, "SELECT n,v FROM docs ORDER BY n").rows, before[1..]);
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .index_entries,
        2
    );
}

#[test]
fn update_from_inner_sources_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,v INTEGER)",
        "INSERT INTO native VALUES(1,0),(2,0),(3,0)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,v) SELECT n,v FROM native",
        "CREATE TABLE source(k INTEGER,v INTEGER)",
        "INSERT INTO source VALUES(1,7),(1,8),(2,9)",
        "CREATE TABLE source_docs",
        "INSERT INTO source_docs(k,v) SELECT k,v FROM source",
        "CREATE TABLE keys(k INTEGER)",
        "INSERT INTO keys VALUES(1),(2)",
    ] {
        q(&c, sql);
    }
    for source in ["source", "source_docs"] {
        for join in [
            "JOIN keys k ON k.k=s.k",
            "INNER JOIN keys k ON k.k=s.k",
            "CROSS JOIN keys k ON k.k=s.k",
        ] {
            q(&c, "BEGIN");
            let sql = format!(
                "UPDATE TARGET SET v=s.v FROM {source} s {join} WHERE s.k=TARGET.n RETURNING n,v"
            );
            let expected = q(
                &c,
                &sql.replace("TARGET", "native")
                    .replace("source_docs", "source"),
            );
            let actual = q(&c, &sql.replace("TARGET", "docs"));
            assert_eq!(actual.rows, expected.rows, "{sql}");
            assert_eq!(actual.affected, expected.affected);
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn native_update_from_candidate_error_preserves_rows() {
    {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE target(n INTEGER PRIMARY KEY,v INTEGER)",
            "INSERT INTO target VALUES(1,0),(2,0)",
            "CREATE TABLE source(k INTEGER,v INTEGER)",
            "INSERT INTO source VALUES(1,7),(2,-9223372036854775808)",
        ] {
            q(&c, sql);
        }
        q(&c, "CREATE TABLE docs");
        q(&c, "INSERT INTO docs(n,v) SELECT n,v FROM target");
        let sql = String::from(
            "UPDATE target SET v=abs(s.v) FROM source s WHERE s.k=target.n RETURNING n,v",
        );
        let error = c.execute(&sql, &Parameters::new()).unwrap_err();
        let collection_error = c
            .execute(&sql.replace("target", "docs"), &Parameters::new())
            .unwrap_err();
        assert_eq!(collection_error.code(), error.code());
        assert!(collection_error.to_string().contains("integer overflow"));
        assert_eq!(
            q(&c, "SELECT n,v FROM docs ORDER BY n").rows,
            q(&c, "SELECT n,v FROM target ORDER BY n").rows
        );
        assert_eq!(error.code(), "FDB_ENGINE");
        assert!(error.to_string().contains("integer overflow"));
        assert_eq!(
            q(&c, "SELECT n,v FROM target ORDER BY n").rows,
            vec![
                vec![Value::Integer(1), Value::Integer(0)],
                vec![Value::Integer(2), Value::Integer(0)]
            ]
        );
    }
}

#[test]
fn update_from_cte_pagination_preserves_bound_scopes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,v INTEGER)",
        "INSERT INTO native VALUES(1,0),(2,0),(3,0)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,v) SELECT n,v FROM native",
        "CREATE TABLE source(k INTEGER,v INTEGER)",
        "INSERT INTO source VALUES(1,7),(1,8),(2,9)",
        "CREATE TABLE source_docs",
        "INSERT INTO source_docs(k,v) SELECT k,v FROM source",
    ] {
        q(&c, sql);
    }
    for source in ["source", "source_docs"] {
        for skip in [0, 1, 2] {
            q(&c, "BEGIN");
            let params = Parameters::from([
                ("$delta".into(), Value::Integer(2)),
                ("$count".into(), Value::Integer(1)),
                ("$skip".into(), Value::Integer(skip)),
            ]);
            let sql=format!("WITH chosen AS (SELECT k,v+$delta AS v FROM {source} LIMIT $count OFFSET $skip) UPDATE TARGET SET v=s.v FROM chosen s WHERE s.k=TARGET.n RETURNING n,v");
            let expected = c
                .execute(
                    &sql.replace("TARGET", "native")
                        .replace("source_docs", "source"),
                    &params,
                )
                .unwrap();
            let actual = c.execute(&sql.replace("TARGET", "docs"), &params).unwrap();
            assert_eq!(actual.rows, expected.rows);
            assert_eq!(actual.affected, expected.affected);
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn update_from_left_source_join_preserves_unmatched_rows() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,v INTEGER)",
        "INSERT INTO native VALUES(1,0),(2,0),(3,0)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,v) SELECT n,v FROM native",
        "CREATE TABLE source(k INTEGER,v INTEGER)",
        "INSERT INTO source VALUES(1,7),(2,8)",
        "CREATE TABLE source_docs",
        "INSERT INTO source_docs(k,v) SELECT k,v FROM source",
        "CREATE TABLE extras(k INTEGER,delta INTEGER)",
        "INSERT INTO extras VALUES(1,10),(1,20)",
    ] {
        q(&c, sql);
    }
    for source in ["source", "source_docs"] {
        for join in ["LEFT JOIN", "LEFT OUTER JOIN"] {
            q(&c, "BEGIN");
            let sql=format!("UPDATE TARGET SET v=s.v+coalesce(e.delta,100) FROM {source} s {join} extras e ON e.k=s.k WHERE s.k=TARGET.n RETURNING n,v");
            let expected = q(
                &c,
                &sql.replace("TARGET", "native")
                    .replace("source_docs", "source"),
            );
            let actual = q(&c, &sql.replace("TARGET", "docs"));
            assert_eq!(actual.rows, expected.rows, "{sql}");
            assert_eq!(actual.affected, expected.affected);
            assert_eq!(
                q(&c, "SELECT n,v FROM docs ORDER BY n").rows,
                q(&c, "SELECT n,v FROM native ORDER BY n").rows
            );
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn update_from_right_source_join_preserves_unmatched_rows() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,v INTEGER)",
        "INSERT INTO native VALUES(1,0),(2,0),(3,0)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,v) SELECT n,v FROM native",
        "CREATE TABLE source(k INTEGER,v INTEGER)",
        "INSERT INTO source VALUES(1,7)",
        "CREATE TABLE source_docs",
        "INSERT INTO source_docs(k,v) SELECT k,v FROM source",
        "CREATE TABLE extras(k INTEGER,delta INTEGER)",
        "INSERT INTO extras VALUES(1,10),(2,20)",
    ] {
        q(&c, sql);
    }
    for source in ["source", "source_docs"] {
        for join in ["RIGHT JOIN", "RIGHT OUTER JOIN"] {
            q(&c, "BEGIN");
            let sql=format!("UPDATE TARGET SET v=coalesce(s.v,100)+e.delta FROM {source} s {join} extras e ON e.k=s.k WHERE e.k=TARGET.n RETURNING n,v");
            let expected = q(
                &c,
                &sql.replace("TARGET", "native")
                    .replace("source_docs", "source"),
            );
            let actual = q(&c, &sql.replace("TARGET", "docs"));
            assert_eq!(actual.rows, expected.rows, "{sql}");
            assert_eq!(actual.affected, expected.affected);
            assert_eq!(
                q(&c, "SELECT n,v FROM docs ORDER BY n").rows,
                q(&c, "SELECT n,v FROM native ORDER BY n").rows
            );
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn update_from_leading_right_join_preserves_following_joins() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,v INTEGER)",
        "INSERT INTO native VALUES(1,0),(2,0),(3,0)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,v) SELECT n,v FROM native",
        "CREATE TABLE source(k INTEGER,v INTEGER)",
        "INSERT INTO source VALUES(1,7)",
        "CREATE TABLE extras(k INTEGER,delta INTEGER)",
        "INSERT INTO extras VALUES(1,10),(2,20)",
        "CREATE TABLE final(k INTEGER,bonus INTEGER)",
        "INSERT INTO final VALUES(1,5)",
    ] {
        q(&c, sql);
    }
    for join in ["JOIN", "LEFT JOIN"] {
        q(&c, "BEGIN");
        let sql=format!("UPDATE TARGET SET v=coalesce(s.v,100)+e.delta+coalesce(f.bonus,50) FROM source s RIGHT JOIN extras e ON e.k=s.k {join} final f ON f.k=e.k WHERE e.k=TARGET.n RETURNING n,v");
        let expected = q(&c, &sql.replace("TARGET", "native"));
        let actual = q(&c, &sql.replace("TARGET", "docs"));
        assert_eq!(actual.rows, expected.rows, "{sql}");
        assert_eq!(actual.affected, expected.affected);
        q(&c, "ROLLBACK");
    }
}

#[test]
fn update_from_self_sources_preserve_original_snapshots_and_scopes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,v INTEGER)",
        "INSERT INTO native VALUES(1,10),(2,20),(3,30)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,v) SELECT n,v FROM native",
    ] {
        q(&c, sql);
    }
    for source in ["TARGET s", "(SELECT n,v FROM TARGET) s", "chosen s"] {
        {
            let limit = "";
            q(&c, "BEGIN");
            let sql = format!("WITH chosen AS (SELECT n,v FROM TARGET) UPDATE TARGET AS t SET v=s.v+1 FROM {source} WHERE s.n=t.n-1 RETURNING n,v{limit}");
            let expected = q(&c, &sql.replace("TARGET", "native"));
            let actual = q(&c, &sql.replace("TARGET", "docs"));
            assert_eq!(actual.rows, expected.rows, "{sql}");
            assert_eq!(actual.affected, expected.affected, "{sql}");
            assert_eq!(
                q(&c, "SELECT n,v FROM docs ORDER BY n").rows,
                q(&c, "SELECT n,v FROM native ORDER BY n").rows,
                "{sql}"
            );
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn update_from_target_named_ctes_keep_source_and_target_bindings_separate() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,v INTEGER)",
        "INSERT INTO native VALUES(1,10),(2,20),(3,30)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,v) SELECT n,v FROM native",
    ] {
        q(&c, sql);
    }
    for name in ["TARGET", "t"] {
        for alias in ["", " AS t"] {
            let target = if alias.is_empty() { "TARGET" } else { "t" };
            for hint in ["", "MATERIALIZED", "NOT MATERIALIZED"] {
                {
                    let limit = "";
                    q(&c, "BEGIN");
                    let sql = format!("WITH {name}(n,v) AS {hint} (SELECT 1,100 UNION ALL SELECT 2,200), chosen AS (SELECT n,v FROM {name}) UPDATE TARGET{alias} SET v=s.v+1 FROM chosen s WHERE s.n={target}.n RETURNING n,v{limit}");
                    let expected = q(&c, &sql.replace("TARGET", "native"));
                    let actual = q(&c, &sql.replace("TARGET", "docs"));
                    assert_eq!(actual.rows, expected.rows, "{sql}");
                    assert_eq!(actual.affected, expected.affected, "{sql}");
                    assert_eq!(
                        q(&c, "SELECT n,v FROM docs ORDER BY n").rows,
                        q(&c, "SELECT n,v FROM native ORDER BY n").rows,
                        "{sql}"
                    );
                    q(&c, "ROLLBACK");
                }
            }
        }
    }
}

#[test]
fn update_from_deduplicates_typed_record_ids_without_conflating_keys() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs(id,n,v) VALUES(type::record('docs',1),1,0),(type::record('docs','1'),2,0)",
        "CREATE INDEX docs_v ON docs(v)",
        "CREATE TABLE source",
        "INSERT INTO source(target,v) VALUES(type::record('docs',1),10),(type::record('docs',1),11),(type::record('docs','1'),20),(type::record('docs','1'),21)",
    ] {
        q(&c, sql);
    }
    let before = q(&c, "SELECT id,n,v FROM docs ORDER BY n").rows;
    {
        let limit = "";
        q(&c, "BEGIN");
        let result = q(&c, &format!("UPDATE docs AS d SET v=s.v FROM source s WHERE d.id=s.target RETURNING id,n,v{limit}"));
        let expected = 2;
        assert_eq!(result.affected, expected);
        assert_eq!(result.rows.len(), expected as usize);
        let stored = q(&c, "SELECT id,n,v FROM docs ORDER BY n").rows;
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0][0], before[0][0]);
        assert_eq!(stored[1][0], before[1][0]);
        assert_ne!(stored[0][0], stored[1][0]);
        for row in &result.rows {
            let n = if row[1] == Value::Integer(1) { 0 } else { 1 };
            assert_eq!(row, &stored[n]);
            let valid = if n == 0 { [10, 11] } else { [20, 21] };
            assert!(valid.iter().any(|v| row[2] == Value::Integer(*v)));
        }
        assert_eq!(
            q(&c, "SELECT count(*) FROM docs WHERE v>0").rows,
            vec![vec![Value::Integer(expected)]]
        );
        q(&c, "ROLLBACK");
        assert_eq!(q(&c, "SELECT id,n,v FROM docs ORDER BY n").rows, before);
        assert_eq!(
            q(&c, "SELECT count(*) FROM docs WHERE v>0").rows,
            vec![vec![Value::Integer(0)]]
        );
    }
}

#[test]
fn grouped_update_from_retains_pinned_rejection_and_pending_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,v INTEGER)",
        "INSERT INTO native VALUES(1,10),(2,20)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,v) SELECT n,v FROM native",
        "CREATE TABLE source(k INTEGER,w INTEGER)",
        "INSERT INTO source VALUES(1,100)",
    ] {
        q(&c, sql);
    }
    q(&c, "BEGIN");
    q(&c, "INSERT INTO native VALUES(3,30)");
    q(&c, "INSERT INTO docs(n,v) VALUES(3,30)");
    for table in ["native", "docs"] {
        let before = q(&c, &format!("SELECT n,v FROM {table} ORDER BY n")).rows;
        for alias in ["", " AS g"] {
            let sql=format!("UPDATE {table} SET v=100 FROM (source a LEFT JOIN source b ON a.k=b.k){alias} WHERE {table}.n=1");
            let error = c.execute(&sql, &Parameters::new()).unwrap_err();
            assert!(
                matches!(error.code(), "FDB_ENGINE" | "FDB_UNSUPPORTED"),
                "{sql}: {error}"
            );
            if table == "native" {
                assert!(error
                    .to_string()
                    .contains("Parenthesized FROM clause subqueries are not supported"));
            }
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
            assert_eq!(
                q(&c, &format!("SELECT n,v FROM {table} ORDER BY n")).rows,
                before
            );
        }
    }
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        q(&c, "SELECT count(*) FROM native").rows,
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn update_from_unconstrained_right_join_preserves_empty_sources() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,v INTEGER)",
        "INSERT INTO native VALUES(1,0),(2,0),(3,0)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,v) SELECT n,v FROM native",
        "CREATE TABLE source(v INTEGER)",
        "CREATE TABLE source_docs",
        "CREATE TABLE keys(k INTEGER)",
    ] {
        q(&c, sql);
    }
    for left_rows in [0, 1, 2] {
        for right_rows in [0, 2] {
            for source in ["source", "source_docs"] {
                for join in ["RIGHT JOIN", "RIGHT OUTER JOIN"] {
                    q(&c, "BEGIN");
                    for n in 0..left_rows {
                        q(&c, &format!("INSERT INTO source VALUES({})", n + 10));
                        q(
                            &c,
                            &format!("INSERT INTO source_docs(v) VALUES({})", n + 10),
                        );
                    }
                    for n in 0..right_rows {
                        q(&c, &format!("INSERT INTO keys VALUES({})", n + 1));
                    }
                    let sql=format!("UPDATE TARGET SET v=coalesce(s.v,100)+k.k FROM {source} s {join} keys k WHERE k.k=TARGET.n RETURNING n,v");
                    let expected = q(
                        &c,
                        &sql.replace("TARGET", "native")
                            .replace("source_docs", "source"),
                    );
                    let actual = q(&c, &sql.replace("TARGET", "docs"));
                    assert_eq!(actual.rows, expected.rows, "{sql}");
                    assert_eq!(actual.affected, expected.affected, "{sql}");
                    assert_eq!(
                        q(&c, "SELECT n,v FROM docs ORDER BY n").rows,
                        q(&c, "SELECT n,v FROM native ORDER BY n").rows
                    );
                    q(&c, "ROLLBACK");
                }
            }
        }
    }
}

#[test]
fn update_from_json_iterators_preserves_bound_candidates_and_pagination() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,v INTEGER)",
        "INSERT INTO native VALUES(1,0),(2,0),(3,0)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,v) SELECT n,v FROM native",
    ] {
        q(&c, sql);
    }
    for iterator in ["json_each", "main.json_each"] {
        for skip in [0, 1, 2] {
            q(&c, "BEGIN");
            let params = Parameters::from([
                (
                    "$json".into(),
                    Value::String(r#"[{"n":1,"v":10},{"n":1,"v":11},{"n":2,"v":20}]"#.into()),
                ),
                ("$skip".into(), Value::Integer(skip)),
            ]);
            let sql=format!("UPDATE TARGET SET v=json_extract(j.value,'$.v') FROM (SELECT * FROM {iterator}($json) LIMIT 1 OFFSET $skip) j WHERE TARGET.n=json_extract(j.value,'$.n') RETURNING n,v");
            let expected = c
                .execute(&sql.replace("TARGET", "native"), &params)
                .unwrap();
            let actual = c.execute(&sql.replace("TARGET", "docs"), &params).unwrap();
            assert_eq!(actual.rows, expected.rows, "{sql}");
            assert_eq!(actual.affected, expected.affected);
            assert_eq!(
                q(&c, "SELECT n,v FROM docs ORDER BY n").rows,
                q(&c, "SELECT n,v FROM native ORDER BY n").rows
            );
            q(&c, "ROLLBACK");
        }
    }
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs(n,v) VALUES(4,40)");
    let before = q(&c, "SELECT n,v FROM docs ORDER BY n").rows;
    let sql = "UPDATE docs SET v=j.value FROM json_each($json) j WHERE docs.n=j.key RETURNING n,v";
    assert_eq!(
        c.execute(sql, &Parameters::new()).unwrap_err().code(),
        "FDB_PARAMETER"
    );
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(q(&c, "SELECT n,v FROM docs ORDER BY n").rows, before);
    let record = Value::Record(Record {
        table: "docs".into(),
        key: Key::Integer(i64::MAX),
    });
    let params = Parameters::from([
        ("$json".into(), Value::String("[7]".into())),
        ("$record".into(), record.clone()),
    ]);
    let typed=c.execute("UPDATE docs SET payload=array::new(j.value,$record) FROM json_each($json) j WHERE docs.n=j.key+1 RETURNING n,payload",&params).unwrap();
    assert_eq!(typed.affected, 1);
    assert_eq!(
        typed.rows,
        vec![vec![
            Value::Integer(1),
            Value::Array(vec![Value::Integer(7), record])
        ]]
    );
    q(&c, "ROLLBACK");
}

#[test]
fn malformed_joined_iterator_matches_native_transaction_abort_and_retry() {
    for table in ["native", "docs"] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE native(n INTEGER)");
        q(&c, "CREATE TABLE docs");
        q(&c, "CREATE INDEX docs_n ON docs(n)");
        q(&c, &format!("INSERT INTO {table}(n) VALUES(1)"));
        q(&c, "BEGIN");
        q(&c, &format!("INSERT INTO {table}(n) VALUES(2)"));
        let sql = format!("UPDATE {table} SET n=j.value FROM json_each($json) j RETURNING n");
        let bad = Parameters::from([("$json".into(), Value::String("bad".into()))]);
        assert_eq!(c.execute(&sql, &bad).unwrap_err().code(), "FDB_ENGINE");
        assert_eq!(c.transaction_state(), romedb::TransactionState::Autocommit);
        assert_eq!(
            q(&c, &format!("SELECT n FROM {table}")).rows,
            vec![vec![Value::Integer(1)]]
        );
        q(&c, "BEGIN");
        let valid = Parameters::from([("$json".into(), Value::String("[7]".into()))]);
        assert_eq!(
            c.execute(&sql, &valid).unwrap().rows,
            vec![vec![Value::Integer(7)]]
        );
        q(&c, "COMMIT");
        assert_eq!(
            q(&c, &format!("SELECT n FROM {table}")).rows,
            vec![vec![Value::Integer(7)]]
        );
        if table == "docs" {
            assert_eq!(
                c.check_collection_integrity("docs", Default::default())
                    .unwrap()
                    .index_entries,
                1
            );
        }
    }
}

#[test]
fn update_from_source_scope_excludes_target_and_preserves_local_aliases() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,v INTEGER)",
        "INSERT INTO native VALUES(1,0),(2,0)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,v) SELECT n,v FROM native",
        "CREATE TABLE a(k INTEGER)",
        "INSERT INTO a VALUES(1),(2)",
        "CREATE TABLE b(v INTEGER)",
        "INSERT INTO b VALUES(10)",
    ] {
        q(&c, sql);
    }
    for table in ["native", "docs"] {
        q(&c, "BEGIN");
        q(&c, &format!("INSERT INTO {table}(n,v) VALUES(3,0)"));
        let before = q(&c, &format!("SELECT n,v FROM {table} ORDER BY n")).rows;
        for predicate in ["a.k=t.n", "a.k=(SELECT t.n)"] {
            let sql =
                format!("UPDATE {table} AS t SET v=b.v FROM a JOIN b ON {predicate} RETURNING n,v");
            let error = c.execute(&sql, &Parameters::new()).unwrap_err();
            assert!(
                matches!(error.code(), "FDB_ENGINE" | "FDB_UNSUPPORTED"),
                "{error}"
            );
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
            assert_eq!(
                q(&c, &format!("SELECT n,v FROM {table} ORDER BY n")).rows,
                before
            );
        }
        let result = q(
            &c,
            &format!(
                "UPDATE {table} AS target SET v=b.v FROM a AS t JOIN b ON t.k=1 RETURNING n,v"
            ),
        );
        assert_eq!(result.affected, 3);
        assert_eq!(
            q(&c, &format!("SELECT n,v FROM {table} ORDER BY n")).rows,
            vec![
                vec![Value::Integer(1), Value::Integer(10)],
                vec![Value::Integer(2), Value::Integer(10)],
                vec![Value::Integer(3), Value::Integer(10)]
            ]
        );
        q(&c, "ROLLBACK");
    }
}

#[test]
fn update_from_target_correlated_iterators_match_native() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,j TEXT)",
        "INSERT INTO native VALUES(1,'[7,8]'),(2,'[9]'),(3,'[]')",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,j) SELECT n,j FROM native",
    ] {
        q(&c, sql);
    }
    for iterator in ["json_each", "main.json_each", "json_tree"] {
        {
            let params = Parameters::new();
            q(&c, "BEGIN");
            let sql=format!("UPDATE TARGET AS t SET n=s.value FROM {iterator}(t.j) s WHERE s.type='integer' RETURNING n,j");
            let expected = c
                .execute(&sql.replace("TARGET", "native"), &params)
                .unwrap();
            let actual = c.execute(&sql.replace("TARGET", "docs"), &params).unwrap();
            assert_eq!(actual.rows, expected.rows, "{sql}");
            assert_eq!(actual.affected, expected.affected);
            assert_eq!(
                q(&c, "SELECT n,j FROM docs ORDER BY n").rows,
                q(&c, "SELECT n,j FROM native ORDER BY n").rows
            );
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn update_from_derived_join_sources_preserve_merged_scope() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER,v INTEGER)",
        "INSERT INTO native VALUES(1,0),(2,0),(3,0)",
        "CREATE TABLE docs",
        "INSERT INTO docs(n,v) SELECT n,v FROM native",
        "CREATE TABLE a(k INTEGER,v INTEGER)",
        "INSERT INTO a VALUES(1,10),(2,20)",
        "CREATE TABLE source_docs",
        "INSERT INTO source_docs(k,v) SELECT k,v FROM a",
        "CREATE TABLE b(k INTEGER,w INTEGER)",
        "INSERT INTO b VALUES(2,200),(3,300)",
    ] {
        q(&c, sql);
    }
    for source in ["a", "source_docs"] {
        for (key, join) in [
            ("k", "JOIN b USING(k)"),
            ("k", "LEFT JOIN b USING(k)"),
            ("coalesce(a.k,b.k)", "FULL JOIN b ON a.k=b.k"),
        ] {
            q(&c, "BEGIN");
            let sql=format!("UPDATE TARGET AS t SET v=s.v FROM (SELECT {key} AS k,coalesce(a.v,0)+coalesce(b.w,0) AS v FROM {source} AS a {join}) s WHERE t.n=s.k RETURNING n,v");
            let expected = q(
                &c,
                &sql.replace("TARGET", "native").replace("source_docs", "a"),
            );
            let actual = q(&c, &sql.replace("TARGET", "docs"));
            assert_eq!(actual.affected, expected.affected, "{sql}");
            let sort = |mut rows: Vec<Vec<Value>>| {
                rows.sort_by_key(|row| match row[0] {
                    Value::Integer(n) => n,
                    _ => panic!("integer key"),
                });
                rows
            };
            assert_eq!(sort(actual.rows), sort(expected.rows), "{sql}");
            assert_eq!(
                q(&c, "SELECT n,v FROM docs ORDER BY n").rows,
                q(&c, "SELECT n,v FROM native ORDER BY n").rows
            );
            q(&c, "ROLLBACK");
        }
    }
}

#[test]
fn explicit_update_abort_preserves_native_statement_recovery() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE native(n INTEGER UNIQUE,v INTEGER CHECK(v<5))",
        "INSERT INTO native VALUES(1,0),(2,0)",
        "CREATE TABLE docs",
        "DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
        "INSERT INTO docs(n,v) SELECT n,v FROM native",
    ] {
        q(&c, sql);
    }
    for from in ["", " FROM (SELECT 1 AS k) source"] {
        for table in ["native", "docs"] {
            q(&c, "BEGIN");
            q(&c, &format!("INSERT INTO {table}(n,v) VALUES(3,0)"));
            let before = q(&c, &format!("SELECT n,v FROM {table} ORDER BY n")).rows;
            let error = c
                .execute(
                    &format!("UPDATE OR ABORT {table} SET n=1{from} WHERE {table}.n>1 RETURNING n"),
                    &Parameters::new(),
                )
                .unwrap_err();
            assert_eq!(error.code(), "FDB_CONSTRAINT", "{table}/{from}: {error}");
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
            assert_eq!(
                q(&c, &format!("SELECT n,v FROM {table} ORDER BY n")).rows,
                before
            );
            let error = c
                .execute(
                    &format!(
                        "UPDATE OR ABORT {table} SET v=10{from} WHERE {table}.n>1 RETURNING n,v"
                    ),
                    &Parameters::new(),
                )
                .unwrap_err();
            assert_eq!(
                error.code(),
                if table == "docs" {
                    "FDB_VALIDATION"
                } else {
                    "FDB_CONSTRAINT"
                }
            );
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
            assert_eq!(
                q(&c, &format!("SELECT n,v FROM {table} ORDER BY n")).rows,
                before
            );
            let result = q(
                &c,
                &format!("UPDATE OR ABORT {table} SET v={table}.v+1{from} WHERE {table}.n=2 RETURNING n,v"),
            );
            assert_eq!(result.affected, 1);
            assert_eq!(
                result.rows,
                vec![vec![Value::Integer(2), Value::Integer(1)]]
            );
            q(&c, "ROLLBACK");
        }
    }
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .index_entries,
        2
    );
}

#[test]
fn pinned_update_conflict_policies_have_distinct_recovery() {
    for (policy, values, active, returned) in [
        ("ABORT", vec![1, 2, 3, 4], true, None),
        ("FAIL", vec![10, 2, 3, 4], true, None),
        ("IGNORE", vec![10, 2, 3, 4], true, Some(1)),
        ("ROLLBACK", vec![1, 2, 3], false, None),
        ("REPLACE", vec![10, 3, 4], true, Some(2)),
    ] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE native(id INTEGER PRIMARY KEY,n INTEGER UNIQUE)",
            "INSERT INTO native VALUES(1,1),(2,2),(3,3)",
            "BEGIN",
            "INSERT INTO native VALUES(4,4)",
        ] {
            q(&c, sql);
        }
        let result = c.execute(
            &format!("UPDATE OR {policy} native SET n=10 WHERE id<3 RETURNING id,n"),
            &Parameters::new(),
        );
        if let Some(count) = returned {
            let result = result.unwrap();
            assert_eq!(result.affected, count);
            assert_eq!(result.rows.len(), count as usize);
        } else {
            assert_eq!(result.unwrap_err().code(), "FDB_CONSTRAINT");
        }
        assert_eq!(
            c.transaction_state(),
            if active {
                romedb::TransactionState::Active
            } else {
                romedb::TransactionState::Autocommit
            }
        );
        assert_eq!(
            q(&c, "SELECT n FROM native ORDER BY id").rows,
            values
                .into_iter()
                .map(|n| vec![Value::Integer(n)])
                .collect::<Vec<_>>(),
            "{policy}"
        );
    }
}

#[test]
fn explicit_update_rollback_aborts_mutation_failures_only() {
    for from in ["", " FROM (SELECT 1 AS k) source"] {
        for assignment in ["n=1", "v=10"] {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            for sql in [
                "CREATE TABLE docs",
                "DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)",
                "CREATE UNIQUE INDEX docs_n ON docs(n)",
                "INSERT INTO docs(n,v) VALUES(1,0),(2,0)",
                "BEGIN",
                "INSERT INTO docs(n,v) VALUES(3,0)",
            ] {
                q(&c, sql);
            }
            let bad = c
                .execute(
                    &format!("UPDATE OR ROLLBACK docs SET v=missing_function(1){from}"),
                    &Parameters::new(),
                )
                .unwrap_err();
            assert!(!matches!(bad.code(), "FDB_ROLLBACK"));
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
            let error=c.execute(&format!("UPDATE OR ROLLBACK docs SET {assignment}{from} WHERE docs.n>1 RETURNING n,v"),&Parameters::new()).unwrap_err();
            assert_eq!(
                error.code(),
                if assignment == "n=1" {
                    "FDB_CONSTRAINT"
                } else {
                    "FDB_VALIDATION"
                }
            );
            assert_eq!(c.transaction_state(), romedb::TransactionState::Autocommit);
            assert_eq!(
                q(&c, "SELECT n,v FROM docs ORDER BY n").rows,
                vec![
                    vec![Value::Integer(1), Value::Integer(0)],
                    vec![Value::Integer(2), Value::Integer(0)]
                ]
            );
            assert_eq!(
                c.check_collection_integrity("docs", Default::default())
                    .unwrap()
                    .index_entries,
                2
            );
            q(&c, "BEGIN");
            assert_eq!(
                q(
                    &c,
                    &format!("UPDATE OR ROLLBACK docs SET v=1{from} WHERE docs.n=2 RETURNING v")
                )
                .rows,
                vec![vec![Value::Integer(1)]]
            );
            q(&c, "COMMIT");
        }
    }
}

#[test]
fn update_conflict_recovery_matches_native_across_user_savepoints() {
    for collection in [false, true] {
        for policy in ["ABORT", "ROLLBACK"] {
            for begin in [false, true] {
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                if collection {
                    q(&c, "CREATE TABLE items");
                    q(&c, "CREATE UNIQUE INDEX items_n ON items(n)");
                } else {
                    q(&c, "CREATE TABLE items(n INTEGER UNIQUE)");
                }
                q(&c, "INSERT INTO items(n) VALUES(1),(2)");
                if begin {
                    q(&c, "BEGIN");
                }
                q(&c, "SAVEPOINT outer_work");
                q(&c, "INSERT INTO items(n) VALUES(3)");
                q(&c, "SAVEPOINT inner_work");
                q(&c, "INSERT INTO items(n) VALUES(4)");
                let error = c
                    .execute(
                        &format!("UPDATE OR {policy} items SET n=1 WHERE n>1 RETURNING n"),
                        &Parameters::new(),
                    )
                    .unwrap_err();
                assert_eq!(error.code(), "FDB_CONSTRAINT");
                if policy == "ROLLBACK" {
                    assert_eq!(c.transaction_state(), romedb::TransactionState::Autocommit);
                    for name in ["inner_work", "outer_work"] {
                        assert!(c
                            .execute(&format!("ROLLBACK TO {name}"), &Parameters::new())
                            .is_err());
                    }
                } else {
                    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
                    assert_eq!(q(&c, "SELECT n FROM items ORDER BY n").rows.len(), 4);
                    q(&c, "ROLLBACK TO inner_work");
                    assert_eq!(q(&c, "SELECT n FROM items ORDER BY n").rows.len(), 3);
                    q(&c, "ROLLBACK TO outer_work");
                    q(&c, "RELEASE outer_work");
                    if begin {
                        q(&c, "COMMIT");
                    }
                }
                assert_eq!(
                    q(&c, "SELECT n FROM items ORDER BY n").rows,
                    vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
                );
                // With no enclosing transaction, both policies restore the statement.
                assert_eq!(
                    c.execute(
                        &format!("UPDATE OR {policy} items SET n=10 RETURNING n"),
                        &Parameters::new()
                    )
                    .unwrap_err()
                    .code(),
                    "FDB_CONSTRAINT"
                );
                assert_eq!(c.transaction_state(), romedb::TransactionState::Autocommit);
                assert_eq!(
                    q(&c, "SELECT n FROM items ORDER BY n").rows,
                    vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
                );
                q(&c, "SAVEPOINT fresh_work");
                q(&c, "INSERT INTO items(n) VALUES(5)");
                q(&c, "RELEASE fresh_work");
                assert_eq!(q(&c, "SELECT n FROM items ORDER BY n").rows.len(), 3);
                if collection {
                    assert_eq!(
                        c.check_collection_integrity("items", Default::default())
                            .unwrap()
                            .index_entries,
                        3
                    );
                }
            }
        }
    }
}

#[test]
fn update_ignore_restores_skipped_documents_and_all_indexes() {
    for from in ["", " FROM (SELECT 1 AS k) source"] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE docs",
            "DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)",
            "CREATE INDEX docs_a ON docs(a)",
            "CREATE UNIQUE INDEX docs_n ON docs(n)",
            "INSERT INTO docs(n,v,a) VALUES(1,0,0),(2,0,0),(3,0,0)",
            "BEGIN",
            "INSERT INTO docs(n,v,a) VALUES(4,0,0)",
        ] {
            q(&c, sql);
        }
        let result = q(
            &c,
            &format!("UPDATE OR IGNORE docs SET n=10,a=7{from} WHERE docs.n<3 RETURNING n,a"),
        );
        assert_eq!(result.affected, 1);
        assert_eq!(
            result.rows,
            vec![vec![Value::Integer(10), Value::Integer(7)]]
        );
        assert_eq!(
            q(&c, "SELECT n,a FROM docs ORDER BY n").rows,
            vec![
                vec![Value::Integer(2), Value::Integer(0)],
                vec![Value::Integer(3), Value::Integer(0)],
                vec![Value::Integer(4), Value::Integer(0)],
                vec![Value::Integer(10), Value::Integer(7)]
            ]
        );
        let result = q(
            &c,
            &format!("UPDATE OR IGNORE docs SET v=10,a=9{from} RETURNING n"),
        );
        assert_eq!(result.affected, 0);
        assert!(result.rows.is_empty());
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            8
        );
        q(&c, "ROLLBACK");
        assert_eq!(
            q(&c, "SELECT n FROM docs ORDER BY n").rows,
            vec![
                vec![Value::Integer(1)],
                vec![Value::Integer(2)],
                vec![Value::Integer(3)]
            ]
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            6
        );
    }
}

#[test]
fn update_fail_retains_successful_prefix_without_partial_index_changes() {
    for collection in [false, true] {
        for active in [false, true] {
            for from in ["", " FROM (SELECT 1 AS k) source"] {
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                if collection {
                    q(&c, "CREATE TABLE items");
                    q(&c, "CREATE INDEX items_v ON items(v)");
                    q(&c, "CREATE UNIQUE INDEX items_n ON items(n)");
                } else {
                    q(&c, "CREATE TABLE items(n INTEGER UNIQUE,v INTEGER)");
                }
                q(&c, "INSERT INTO items(n,v) VALUES(1,0),(2,0),(3,0)");
                if active {
                    q(&c, "BEGIN");
                    q(&c, "INSERT INTO items(n,v) VALUES(4,0)");
                }
                let error = c
                    .execute(
                        &format!(
                            "UPDATE OR FAIL items SET n=10,v=7{from} WHERE items.n<3 RETURNING n,v"
                        ),
                        &Parameters::new(),
                    )
                    .unwrap_err();
                assert_eq!(error.code(), "FDB_CONSTRAINT");
                assert_eq!(
                    c.transaction_state(),
                    if active {
                        romedb::TransactionState::Active
                    } else {
                        romedb::TransactionState::Autocommit
                    }
                );
                let mut expected = vec![
                    vec![Value::Integer(2), Value::Integer(0)],
                    vec![Value::Integer(3), Value::Integer(0)],
                ];
                if active {
                    expected.push(vec![Value::Integer(4), Value::Integer(0)]);
                }
                expected.push(vec![Value::Integer(10), Value::Integer(7)]);
                assert_eq!(q(&c, "SELECT n,v FROM items ORDER BY n").rows, expected);
                if collection {
                    assert_eq!(
                        c.check_collection_integrity("items", Default::default())
                            .unwrap()
                            .index_entries,
                        if active { 8 } else { 6 }
                    );
                }
                if active {
                    q(&c, "ROLLBACK");
                    assert_eq!(
                        q(&c, "SELECT n,v FROM items ORDER BY n").rows,
                        vec![
                            vec![Value::Integer(1), Value::Integer(0)],
                            vec![Value::Integer(2), Value::Integer(0)],
                            vec![Value::Integer(3), Value::Integer(0)]
                        ]
                    );
                }
            }
        }
    }
}

#[test]
fn update_fail_validation_retains_prefix_but_limited_api_restores_statement() {
    for limited in [false, true] {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE docs",
            "DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)",
            "CREATE INDEX docs_v ON docs(v)",
            "INSERT INTO docs(n,v) VALUES(1,0),(2,0),(3,0)",
            "BEGIN",
            "INSERT INTO docs(n,v) VALUES(4,0)",
        ] {
            q(&c, sql);
        }
        let sql = "UPDATE OR FAIL docs SET v=CASE WHEN n=2 THEN 10 ELSE 1 END RETURNING n,v";
        let error = if limited {
            c.write_with_result_limits(
                sql,
                &Parameters::new(),
                romedb::ResultLimits {
                    max_rows: 10,
                    max_payload_bytes: 10000,
                },
            )
            .unwrap_err()
        } else {
            c.execute(sql, &Parameters::new()).unwrap_err()
        };
        assert_eq!(error.code(), "FDB_VALIDATION");
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(
            q(&c, "SELECT n,v FROM docs ORDER BY n").rows,
            vec![
                vec![
                    Value::Integer(1),
                    Value::Integer(if limited { 0 } else { 1 })
                ],
                vec![Value::Integer(2), Value::Integer(0)],
                vec![Value::Integer(3), Value::Integer(0)],
                vec![Value::Integer(4), Value::Integer(0)]
            ]
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            4
        );
        q(&c, "ROLLBACK");
        assert_eq!(
            q(&c, "SELECT v FROM docs ORDER BY n").rows,
            vec![vec![Value::Integer(0)]; 3]
        );
    }
}

#[test]
fn fail_and_ignore_preserve_user_savepoint_recovery_like_native() {
    for collection in [false, true] {
        for policy in ["FAIL", "IGNORE"] {
            for begin in [false, true] {
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                if collection {
                    q(&c, "CREATE TABLE items");
                    q(&c, "CREATE UNIQUE INDEX items_n ON items(n)");
                } else {
                    q(&c, "CREATE TABLE items(n INTEGER UNIQUE)");
                }
                q(&c, "INSERT INTO items(n) VALUES(1),(2)");
                if begin {
                    q(&c, "BEGIN");
                }
                q(&c, "SAVEPOINT outer_work");
                q(&c, "INSERT INTO items(n) VALUES(3)");
                q(&c, "SAVEPOINT inner_work");
                let result = c.execute(
                    &format!("UPDATE OR {policy} items SET n=10 WHERE n<3 RETURNING n"),
                    &Parameters::new(),
                );
                if policy == "FAIL" {
                    assert_eq!(result.unwrap_err().code(), "FDB_CONSTRAINT");
                } else {
                    assert_eq!(result.unwrap().rows, vec![vec![Value::Integer(10)]]);
                }
                assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
                assert_eq!(
                    q(&c, "SELECT n FROM items ORDER BY n").rows,
                    vec![
                        vec![Value::Integer(2)],
                        vec![Value::Integer(3)],
                        vec![Value::Integer(10)]
                    ]
                );
                q(&c, "ROLLBACK TO inner_work");
                assert_eq!(
                    q(&c, "SELECT n FROM items ORDER BY n").rows,
                    vec![
                        vec![Value::Integer(1)],
                        vec![Value::Integer(2)],
                        vec![Value::Integer(3)]
                    ]
                );
                q(&c, "RELEASE inner_work");
                q(&c, "ROLLBACK TO outer_work");
                assert_eq!(
                    q(&c, "SELECT n FROM items ORDER BY n").rows,
                    vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
                );
                q(&c, "INSERT INTO items(n) VALUES(4)");
                q(&c, "RELEASE outer_work");
                if begin {
                    q(&c, "COMMIT");
                }
                assert_eq!(c.transaction_state(), romedb::TransactionState::Autocommit);
                assert_eq!(
                    q(&c, "SELECT n FROM items ORDER BY n").rows,
                    vec![
                        vec![Value::Integer(1)],
                        vec![Value::Integer(2)],
                        vec![Value::Integer(4)]
                    ]
                );
                if collection {
                    assert_eq!(
                        c.check_collection_integrity("items", Default::default())
                            .unwrap()
                            .index_entries,
                        3
                    );
                    assert!(c
                        .lookup_index("items", "items_n", &Value::Integer(10))
                        .unwrap()
                        .is_empty());
                }
            }
        }
    }
}

#[test]
fn update_replace_matches_native_deleted_candidates_and_multiple_conflicts() {
    for from in ["", " FROM (SELECT 1 AS k) source"] {
        for assignment in ["n=items.n+1", "n=10", "n=2,v=3"] {
            let mut baseline = None;
            for collection in [false, true] {
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                if collection {
                    q(&c, "CREATE TABLE items");
                    q(&c, "CREATE UNIQUE INDEX items_n ON items(n)");
                    q(&c, "CREATE UNIQUE INDEX items_v ON items(v)");
                    q(&c, "CREATE INDEX items_tag ON items(tag)");
                } else {
                    q(
                        &c,
                        "CREATE TABLE items(n INTEGER UNIQUE,v INTEGER UNIQUE,tag TEXT)",
                    );
                }
                q(
                    &c,
                    "INSERT INTO items(n,v,tag) VALUES(1,1,'a'),(2,2,'b'),(3,3,'c')",
                );
                q(&c, "BEGIN");
                let result=q(&c,&format!("UPDATE OR REPLACE items SET {assignment}{from} WHERE items.n<3 RETURNING n,v,tag"));
                let after = q(&c, "SELECT n,v,tag FROM items ORDER BY n").rows;
                let observed = (result.rows, result.affected, after);
                if collection {
                    assert_eq!(Some(&observed), baseline.as_ref());
                    assert_eq!(
                        c.check_collection_integrity("items", Default::default())
                            .unwrap()
                            .index_entries,
                        observed.2.len() as u64 * 3
                    );
                } else {
                    baseline = Some(observed);
                }
                q(&c, "ROLLBACK");
                assert_eq!(
                    q(&c, "SELECT n FROM items ORDER BY n").rows,
                    vec![
                        vec![Value::Integer(1)],
                        vec![Value::Integer(2)],
                        vec![Value::Integer(3)]
                    ]
                );
                if collection {
                    assert_eq!(
                        c.check_collection_integrity("items", Default::default())
                            .unwrap()
                            .index_entries,
                        9
                    );
                }
            }
        }
    }
}

#[test]
fn update_replace_validation_failure_restores_deleted_conflicts() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
        "CREATE INDEX docs_v ON docs(v)",
        "INSERT INTO docs(n,v) VALUES(1,0),(2,0),(3,0)",
        "BEGIN",
        "INSERT INTO docs(n,v) VALUES(4,0)",
    ] {
        q(&c, sql);
    }
    let before = q(&c, "SELECT * FROM docs ORDER BY n").rows;
    for from in ["", " FROM (SELECT 1 AS k) source"] {
        let error=c.execute(&format!("UPDATE OR REPLACE docs SET n=docs.n+1,v=CASE WHEN docs.n=3 THEN 10 ELSE 1 END{from} WHERE docs.n<4 RETURNING n"),&Parameters::new()).unwrap_err();
        assert_eq!(error.code(), "FDB_VALIDATION");
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(q(&c, "SELECT * FROM docs ORDER BY n").rows, before);
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            8
        );
    }
    // Null unique keys do not conflict; self matches must not delete targets.
    assert_eq!(
        q(&c, "UPDATE OR REPLACE docs SET n=n RETURNING n").affected,
        4
    );
    assert_eq!(
        q(&c, "UPDATE OR REPLACE docs SET n=NULL RETURNING n").affected,
        4
    );
    assert_eq!(
        q(&c, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(4)]]
    );
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .index_entries,
        8
    );
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(3)]
        ]
    );
}

#[test]
fn replacement_conflicts_preserve_typed_nested_index_identity() {
    let reference = Value::Record(Record {
        table: "refs".into(),
        key: Key::Integer(1),
    });
    let keys = vec![
        reference.clone(),
        Value::Binary(
            b"FDB\x01{\"type\":\"Record\",\"value\":{\"table\":\"refs\",\"key\":{\"Integer\":1}}}"
                .to_vec(),
        ),
        Value::Record(Record {
            table: "refs".into(),
            key: Key::String("1".into()),
        }),
        Value::Integer(1),
        Value::String("1".into()),
    ];
    for (selected, key) in keys.iter().enumerate() {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(&c, "CREATE UNIQUE INDEX docs_key ON docs(data.k)");
        q(&c, "CREATE INDEX docs_label ON docs(label)");
        for (i, value) in keys.iter().enumerate() {
            c.insert(
                "docs",
                Document::from([
                    (
                        "id".into(),
                        Value::Record(Record {
                            table: "docs".into(),
                            key: Key::Integer(i as i64),
                        }),
                    ),
                    (
                        "data".into(),
                        Value::Object(Document::from([("k".into(), value.clone())])),
                    ),
                    ("label".into(), Value::Integer(i as i64)),
                ]),
            )
            .unwrap();
        }
        q(
            &c,
            "INSERT INTO docs(id,label) VALUES(type::record('docs',100),100)",
        );
        let before = q(&c, "SELECT * FROM docs ORDER BY label").rows;
        q(&c, "BEGIN");
        let result = c
            .execute(
                "UPDATE OR REPLACE docs SET data.k=$key WHERE label=100 RETURNING docs.data.k",
                &Parameters::from([("$key".into(), key.clone())]),
            )
            .unwrap();
        assert_eq!(result.affected, 1);
        assert_eq!(result.rows, vec![vec![key.clone()]]);
        for (i, value) in keys.iter().enumerate() {
            let found = c.lookup_index("docs", "docs_key", value).unwrap();
            assert_eq!(found.len(), 1);
            assert_eq!(
                found[0]["label"],
                Value::Integer(if i == selected { 100 } else { i as i64 })
            );
        }
        assert!(c
            .lookup_index("docs", "docs_label", &Value::Integer(selected as i64))
            .unwrap()
            .is_empty());
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            10
        );
        q(&c, "ROLLBACK");
        assert_eq!(q(&c, "SELECT * FROM docs ORDER BY label").rows, before);
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            12
        );
    }
}

#[test]
fn replacement_numeric_unique_keys_match_native_comparisons() {
    for (original, replacement) in [
        (Value::Integer(1), Value::Number(1.0)),
        (Value::Number(1.0), Value::Integer(1)),
        (Value::Integer(0), Value::Number(-0.0)),
        (
            Value::Integer(9_007_199_254_740_993),
            Value::Number(9_007_199_254_740_992.0),
        ),
        (
            Value::Integer(i64::MAX),
            Value::Number(9_223_372_036_854_775_808.0),
        ),
    ] {
        let mut baseline = None;
        for collection in [false, true] {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            if collection {
                q(&c, "CREATE TABLE items");
                q(&c, "CREATE UNIQUE INDEX items_key ON items(k)");
                q(&c, "CREATE INDEX items_label ON items(label)");
            } else {
                q(&c, "CREATE TABLE items(k UNIQUE,label INTEGER)");
            }
            c.execute(
                "INSERT INTO items(k,label) VALUES($key,1)",
                &Parameters::from([("$key".into(), original.clone())]),
            )
            .unwrap();
            q(&c, "INSERT INTO items(k,label) VALUES('spare',2)");
            q(&c, "BEGIN");
            let result = c
                .execute(
                    "UPDATE OR REPLACE items SET k=$key WHERE label=2 RETURNING label",
                    &Parameters::from([("$key".into(), replacement.clone())]),
                )
                .unwrap();
            assert_eq!(result.affected, 1);
            assert_eq!(result.rows, vec![vec![Value::Integer(2)]]);
            let labels = q(&c, "SELECT label FROM items ORDER BY label").rows;
            if collection {
                assert_eq!(
                    Some(&labels),
                    baseline.as_ref(),
                    "{original:?} -> {replacement:?}"
                );
                assert_eq!(
                    c.check_collection_integrity("items", Default::default())
                        .unwrap()
                        .index_entries,
                    labels.len() as u64 * 2
                );
                assert_eq!(
                    c.lookup_index("items", "items_key", &replacement)
                        .unwrap()
                        .len(),
                    1
                );
            } else {
                baseline = Some(labels);
            }
            q(&c, "ROLLBACK");
            assert_eq!(
                q(&c, "SELECT label FROM items ORDER BY label").rows,
                vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
            );
        }
    }
}

#[test]
fn insert_abort_preserves_prior_work_for_values_and_select_sources() {
    for source in [
        "VALUES(2,0),(1,0)",
        "SELECT n,v FROM (SELECT 2 AS n,0 AS v UNION ALL SELECT 1,0)",
        "SELECT n,v FROM (SELECT 2 AS n,0 AS v UNION ALL SELECT 3,10)",
    ] {
        for collection in [false, true] {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            if collection {
                q(&c, "CREATE TABLE items");
                q(
                    &c,
                    "DEFINE FIELD v ON items TYPE integer REQUIRED CHECK(v<5)",
                );
                q(&c, "CREATE UNIQUE INDEX items_n ON items(n)");
            } else {
                q(
                    &c,
                    "CREATE TABLE items(n INTEGER UNIQUE,v INTEGER CHECK(v<5))",
                );
            }
            q(&c, "INSERT INTO items(n,v) VALUES(1,0)");
            q(&c, "BEGIN");
            q(&c, "INSERT INTO items(n,v) VALUES(4,0)");
            let error = c
                .execute(
                    &format!("INSERT OR ABORT INTO items(n,v) {source} RETURNING n,v"),
                    &Parameters::new(),
                )
                .unwrap_err();
            assert_eq!(
                error.code(),
                if collection && source.ends_with("3,10)") {
                    "FDB_VALIDATION"
                } else {
                    "FDB_CONSTRAINT"
                }
            );
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
            assert_eq!(
                q(&c, "SELECT n,v FROM items ORDER BY n").rows,
                vec![
                    vec![Value::Integer(1), Value::Integer(0)],
                    vec![Value::Integer(4), Value::Integer(0)]
                ]
            );
            assert_eq!(
                q(
                    &c,
                    "INSERT OR ABORT INTO items(n,v) VALUES(2,1) RETURNING n,v"
                )
                .rows,
                vec![vec![Value::Integer(2), Value::Integer(1)]]
            );
            if collection {
                assert_eq!(
                    c.check_collection_integrity("items", Default::default())
                        .unwrap()
                        .index_entries,
                    3
                );
            }
            q(&c, "ROLLBACK");
            assert_eq!(
                q(&c, "SELECT n FROM items").rows,
                vec![vec![Value::Integer(1)]]
            );
        }
    }
}

#[test]
fn insert_rollback_discards_prior_transaction_on_candidate_failure() {
    for source in [
        "VALUES(2,0),(1,0)",
        "SELECT 2,0 UNION ALL SELECT 1,0",
        "SELECT 2,0 UNION ALL SELECT 3,10",
    ] {
        for collection in [false, true] {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            if collection {
                q(&c, "CREATE TABLE items");
                q(
                    &c,
                    "DEFINE FIELD v ON items TYPE integer REQUIRED CHECK(v<5)",
                );
                q(&c, "CREATE UNIQUE INDEX items_n ON items(n)");
            } else {
                q(
                    &c,
                    "CREATE TABLE items(n INTEGER UNIQUE,v INTEGER CHECK(v<5))",
                );
            }
            q(&c, "INSERT INTO items(n,v) VALUES(1,0)");
            q(&c, "BEGIN");
            q(&c, "INSERT INTO items(n,v) VALUES(4,0)");
            let error = c
                .execute(
                    &format!("INSERT OR ROLLBACK INTO items(n,v) {source} RETURNING n,v"),
                    &Parameters::new(),
                )
                .unwrap_err();
            assert_eq!(
                error.code(),
                if collection && source.ends_with("3,10") {
                    "FDB_VALIDATION"
                } else {
                    "FDB_CONSTRAINT"
                }
            );
            assert_eq!(c.transaction_state(), romedb::TransactionState::Autocommit);
            assert_eq!(
                q(&c, "SELECT n,v FROM items").rows,
                vec![vec![Value::Integer(1), Value::Integer(0)]]
            );
            if collection {
                assert_eq!(
                    c.check_collection_integrity("items", Default::default())
                        .unwrap()
                        .index_entries,
                    1
                );
            }
            q(&c, "BEGIN");
            assert_eq!(
                q(
                    &c,
                    "INSERT OR ROLLBACK INTO items(n,v) VALUES(2,1) RETURNING n,v"
                )
                .rows,
                vec![vec![Value::Integer(2), Value::Integer(1)]]
            );
            q(&c, "COMMIT");
        }
    }
}

#[test]
fn insert_policies_match_native_user_savepoint_disposition() {
    for collection in [false, true] {
        for policy in ["ABORT", "ROLLBACK"] {
            for begin in [false, true] {
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                if collection {
                    q(&c, "CREATE TABLE items");
                    q(&c, "CREATE UNIQUE INDEX items_n ON items(n)");
                } else {
                    q(&c, "CREATE TABLE items(n INTEGER UNIQUE)");
                }
                q(&c, "INSERT INTO items(n) VALUES(1)");
                if begin {
                    q(&c, "BEGIN");
                }
                q(&c, "SAVEPOINT outer_work");
                q(&c, "INSERT INTO items(n) VALUES(4)");
                q(&c, "SAVEPOINT inner_work");
                q(&c, "INSERT INTO items(n) VALUES(5)");
                let error = c
                    .execute(
                        &format!("INSERT OR {policy} INTO items(n) VALUES(2),(1) RETURNING n"),
                        &Parameters::new(),
                    )
                    .unwrap_err();
                assert_eq!(error.code(), "FDB_CONSTRAINT");
                if policy == "ROLLBACK" {
                    assert_eq!(c.transaction_state(), romedb::TransactionState::Autocommit);
                    for name in ["inner_work", "outer_work"] {
                        assert!(c
                            .execute(&format!("ROLLBACK TO {name}"), &Parameters::new())
                            .is_err());
                    }
                } else {
                    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
                    assert_eq!(
                        q(&c, "SELECT n FROM items ORDER BY n").rows,
                        vec![
                            vec![Value::Integer(1)],
                            vec![Value::Integer(4)],
                            vec![Value::Integer(5)]
                        ]
                    );
                    q(&c, "ROLLBACK TO inner_work");
                    assert_eq!(
                        q(&c, "SELECT n FROM items ORDER BY n").rows,
                        vec![vec![Value::Integer(1)], vec![Value::Integer(4)]]
                    );
                    q(&c, "ROLLBACK TO outer_work");
                    q(&c, "RELEASE outer_work");
                    if begin {
                        q(&c, "COMMIT");
                    }
                }
                assert_eq!(
                    q(&c, "SELECT n FROM items").rows,
                    vec![vec![Value::Integer(1)]]
                );
                q(&c, "SAVEPOINT fresh_work");
                q(&c, &format!("INSERT OR {policy} INTO items(n) VALUES(2)"));
                q(&c, "RELEASE fresh_work");
                assert_eq!(c.transaction_state(), romedb::TransactionState::Autocommit);
                assert_eq!(
                    q(&c, "SELECT n FROM items ORDER BY n").rows,
                    vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
                );
                if collection {
                    assert_eq!(
                        c.check_collection_integrity("items", Default::default())
                            .unwrap()
                            .index_entries,
                        2
                    );
                }
            }
        }
    }
}

#[test]
fn insert_conflict_policies_preserve_typed_record_primary_keys() {
    for policy in ["ABORT", "ROLLBACK"] {
        for key in [Key::Integer(1), Key::String("1".into())] {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            q(&c, "CREATE TABLE docs");
            q(&c, "CREATE UNIQUE INDEX docs_n ON docs(n)");
            q(&c,"INSERT INTO docs(id,n) VALUES(type::record('docs',1),1),(type::record('docs','1'),2)");
            q(&c, "BEGIN");
            q(&c, "INSERT INTO docs(id,n) VALUES(docs:pending,3)");
            let id = Value::Record(Record {
                table: "docs".into(),
                key,
            });
            let error=c.execute(&format!("INSERT OR {policy} INTO docs(id,n) VALUES(docs:new,4),($id,5) RETURNING id,n"),&Parameters::from([("$id".into(),id)])).unwrap_err();
            assert_eq!(error.code(), "FDB_CONSTRAINT");
            let active = policy == "ABORT";
            assert_eq!(
                c.transaction_state(),
                if active {
                    romedb::TransactionState::Active
                } else {
                    romedb::TransactionState::Autocommit
                }
            );
            assert!(c
                .get(&Record {
                    table: "docs".into(),
                    key: Key::String("new".into())
                })
                .unwrap()
                .is_none());
            let mut expected = vec![vec![Value::Integer(1)], vec![Value::Integer(2)]];
            if active {
                expected.push(vec![Value::Integer(3)]);
            }
            assert_eq!(q(&c, "SELECT n FROM docs ORDER BY n").rows, expected);
            assert_eq!(
                c.check_collection_integrity("docs", Default::default())
                    .unwrap()
                    .index_entries,
                if active { 3 } else { 2 }
            );
            if active {
                q(&c, "ROLLBACK");
            }
            assert_eq!(
                q(&c, "SELECT id,n FROM docs ORDER BY n").rows,
                vec![
                    vec![
                        Value::Record(Record {
                            table: "docs".into(),
                            key: Key::Integer(1)
                        }),
                        Value::Integer(1)
                    ],
                    vec![
                        Value::Record(Record {
                            table: "docs".into(),
                            key: Key::String("1".into())
                        }),
                        Value::Integer(2)
                    ],
                ]
            );
        }
    }
}

#[test]
fn insert_ignore_skips_conflicts_and_validation_without_partial_indexes() {
    for source in [
        "VALUES(2,1),(1,2),(3,10),(5,3)",
        "SELECT n,v FROM (SELECT 2 AS n,1 AS v UNION ALL SELECT 1,2 UNION ALL SELECT 3,10 UNION ALL SELECT 5,3)",
    ] {
        let mut baseline=None;
        for collection in [false,true] {
            let db=Database::open(":memory:").unwrap();
            let c=db.connect().unwrap();
            if collection {
                q(&c,"CREATE TABLE items");
                q(&c,"DEFINE FIELD v ON items TYPE integer REQUIRED CHECK(v<5)");
                q(&c,"CREATE INDEX items_v ON items(v)");
                q(&c,"CREATE UNIQUE INDEX items_n ON items(n)");
            } else { q(&c,"CREATE TABLE items(n INTEGER UNIQUE,v INTEGER CHECK(v<5))"); }
            q(&c,"INSERT INTO items(n,v) VALUES(1,0)");
            q(&c,"BEGIN");
            q(&c,"INSERT INTO items(n,v) VALUES(4,0)");
            let result=q(&c,&format!("INSERT OR IGNORE INTO items(n,v) {source} RETURNING n,v"));
            assert_eq!(result.affected,2);
            assert_eq!(result.rows,vec![vec![Value::Integer(2),Value::Integer(1)],vec![Value::Integer(5),Value::Integer(3)]]);
            assert_eq!(c.transaction_state(),romedb::TransactionState::Active);
            let rows=q(&c,"SELECT n,v FROM items ORDER BY n").rows;
            if collection {
                assert_eq!(Some(rows),baseline);
                assert_eq!(c.check_collection_integrity("items",Default::default()).unwrap().index_entries,8);
            } else { baseline=Some(rows); }
            q(&c,"ROLLBACK");
            assert_eq!(q(&c,"SELECT n,v FROM items").rows,vec![vec![Value::Integer(1),Value::Integer(0)]]);
        }
    }
}

#[test]
fn insert_fail_retains_successful_candidates_like_native() {
    for source in [
        "VALUES(2,1),(1,2),(5,3)",
        "SELECT 2,1 UNION ALL SELECT 1,2 UNION ALL SELECT 5,3",
        "VALUES(2,1),(3,10),(5,3)",
    ] {
        for active in [false, true] {
            for collection in [false, true] {
                let db = Database::open(":memory:").unwrap();
                let c = db.connect().unwrap();
                if collection {
                    q(&c, "CREATE TABLE items");
                    q(
                        &c,
                        "DEFINE FIELD v ON items TYPE integer REQUIRED CHECK(v<5)",
                    );
                    q(&c, "CREATE INDEX items_v ON items(v)");
                    q(&c, "CREATE UNIQUE INDEX items_n ON items(n)");
                } else {
                    q(
                        &c,
                        "CREATE TABLE items(n INTEGER UNIQUE,v INTEGER CHECK(v<5))",
                    );
                }
                q(&c, "INSERT INTO items(n,v) VALUES(1,0)");
                if active {
                    q(&c, "BEGIN");
                    q(&c, "INSERT INTO items(n,v) VALUES(4,0)");
                }
                let error = c
                    .execute(
                        &format!("INSERT OR FAIL INTO items(n,v) {source} RETURNING n,v"),
                        &Parameters::new(),
                    )
                    .unwrap_err();
                assert_eq!(
                    error.code(),
                    if collection && source.contains("3,10") {
                        "FDB_VALIDATION"
                    } else {
                        "FDB_CONSTRAINT"
                    }
                );
                assert_eq!(
                    c.transaction_state(),
                    if active {
                        romedb::TransactionState::Active
                    } else {
                        romedb::TransactionState::Autocommit
                    }
                );
                let mut expected = vec![
                    vec![Value::Integer(1), Value::Integer(0)],
                    vec![Value::Integer(2), Value::Integer(1)],
                ];
                if active {
                    expected.push(vec![Value::Integer(4), Value::Integer(0)]);
                }
                assert_eq!(q(&c, "SELECT n,v FROM items ORDER BY n").rows, expected);
                if collection {
                    assert_eq!(
                        c.check_collection_integrity("items", Default::default())
                            .unwrap()
                            .index_entries,
                        expected.len() as u64 * 2
                    );
                }
                if active {
                    q(&c, "ROLLBACK");
                    assert_eq!(
                        q(&c, "SELECT n FROM items").rows,
                        vec![vec![Value::Integer(1)]]
                    );
                }
            }
        }
    }
}

#[test]
fn insert_replace_removes_identity_and_multiple_unique_conflicts_atomically() {
    for source in ["VALUES(1,20,30)", "SELECT 1,20,30"] {
        let mut baseline = None;
        for collection in [false, true] {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            if collection {
                q(&c, "CREATE TABLE items");
                q(&c, "CREATE UNIQUE INDEX items_n ON items(n)");
                q(&c, "CREATE UNIQUE INDEX items_v ON items(v)");
                q(&c, "CREATE INDEX items_label ON items(label)");
                q(&c,"INSERT INTO items(id,n,v,label) VALUES(type::record('items',1),10,10,1),(type::record('items',2),20,20,2),(type::record('items',3),30,30,3)");
            } else {
                q(&c,"CREATE TABLE items(id INTEGER PRIMARY KEY,n INTEGER UNIQUE,v INTEGER UNIQUE,label INTEGER)");
                q(
                    &c,
                    "INSERT INTO items VALUES(1,10,10,1),(2,20,20,2),(3,30,30,3)",
                );
            }
            let before = q(&c, "SELECT * FROM items ORDER BY n").rows;
            q(&c, "BEGIN");
            let source = if collection {
                source.replace("1,20,30", "type::record('items',1),20,30")
            } else {
                source.to_owned()
            };
            let result = q(
                &c,
                &format!("INSERT OR REPLACE INTO items(id,n,v) {source} RETURNING n,v,label"),
            );
            assert_eq!(result.affected, 1);
            assert_eq!(
                result.rows,
                vec![vec![Value::Integer(20), Value::Integer(30), Value::Null]]
            );
            let rows = q(&c, "SELECT n,v,label FROM items").rows;
            if collection {
                assert_eq!(Some(rows), baseline);
                assert_eq!(
                    c.check_collection_integrity("items", Default::default())
                        .unwrap()
                        .index_entries,
                    3
                );
            } else {
                baseline = Some(rows);
            }
            q(&c, "ROLLBACK");
            assert_eq!(q(&c, "SELECT * FROM items ORDER BY n").rows, before);
        }
    }
}

#[test]
fn later_insert_replace_validation_failure_restores_prior_replacements() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD v ON docs TYPE integer REQUIRED CHECK(v<5)",
        "CREATE UNIQUE INDEX docs_n ON docs(n)",
        "INSERT INTO docs(id,n,v,extra) VALUES(docs:a,1,0,'old')",
        "BEGIN",
        "INSERT INTO docs(id,n,v) VALUES(docs:pending,2,0)",
    ] {
        q(&c, sql);
    }
    let before = q(&c, "SELECT * FROM docs ORDER BY n").rows;
    for sql in [
        "INSERT OR REPLACE INTO docs(id,n,v) VALUES(docs:a,1,1),(docs:b,3,10)",
        "INSERT OR REPLACE INTO docs(n,v) VALUES(1,1),(3,10)",
    ] {
        assert_eq!(
            c.execute(sql, &Parameters::new()).unwrap_err().code(),
            "FDB_VALIDATION"
        );
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(q(&c, "SELECT * FROM docs ORDER BY n").rows, before);
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .index_entries,
            2
        );
    }
    q(&c, "ROLLBACK");
    assert_eq!(
        q(
            &c,
            "INSERT OR REPLACE INTO docs(n,v) VALUES(1,1) RETURNING n,v"
        )
        .rows,
        vec![vec![Value::Integer(1), Value::Integer(1)]]
    );
    assert!(c
        .get(&Record {
            table: "docs".into(),
            key: Key::String("a".into())
        })
        .unwrap()
        .is_none());
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .index_entries,
        1
    );
}

#[test]
fn joined_update_pagination_coercion_matches_native_with_empty_sources() {
    for empty in [false, true] {
        for limit in [
            "NULL",
            "1.5",
            "'invalid'",
            "1 OFFSET NULL",
            "1 OFFSET 0.5",
            "'1' OFFSET -2",
            "-1 OFFSET 1",
            "0 OFFSET NULL",
        ] {
            let db = Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            for sql in [
                "CREATE TABLE native(n INTEGER PRIMARY KEY,v INTEGER)",
                "INSERT INTO native VALUES(1,0),(2,0)",
                "CREATE TABLE docs",
                "INSERT INTO docs(n,v) SELECT n,v FROM native",
                "CREATE UNIQUE INDEX docs_n ON docs(n)",
                "CREATE TABLE source(k INTEGER,v INTEGER)",
                "BEGIN",
                "INSERT INTO native VALUES(3,0)",
                "INSERT INTO docs(n,v) VALUES(3,0)",
            ] {
                q(&c, sql);
            }
            if !empty {
                q(&c, "INSERT INTO source VALUES(1,7),(1,8),(2,9)");
            }
            let sql = format!("UPDATE TARGET SET v=s.v FROM source s WHERE s.k=TARGET.n RETURNING n,v LIMIT {limit}");
            let expected = c.execute(&sql.replace("TARGET", "native"), &Parameters::new());
            let native_state = c.transaction_state();
            let actual = c.execute(&sql.replace("TARGET", "docs"), &Parameters::new());
            match (expected, actual) {
                (Ok(expected), Ok(actual)) => {
                    assert_eq!(actual.rows, expected.rows, "{empty}: {sql}");
                    assert_eq!(actual.affected, expected.affected);
                }
                (Err(expected), Err(actual)) => {
                    assert_eq!(actual.code(), expected.code(), "{empty}: {sql}");
                }
                pair => panic!("{empty}: {sql}: {pair:?}"),
            }
            assert_eq!(c.transaction_state(), native_state, "{empty}: {sql}");
            assert_eq!(
                q(&c, "SELECT n,v FROM docs ORDER BY n").rows,
                q(&c, "SELECT n,v FROM native ORDER BY n").rows
            );
            assert_eq!(
                c.check_collection_integrity("docs", Default::default())
                    .unwrap()
                    .index_entries,
                3
            );
            q(&c, "ROLLBACK");
            assert_eq!(
                q(&c, "SELECT n,v FROM docs ORDER BY n").rows,
                vec![
                    vec![Value::Integer(1), Value::Integer(0)],
                    vec![Value::Integer(2), Value::Integer(0)]
                ]
            );
        }
    }
}
