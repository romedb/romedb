use romedb::{Database, Parameters, Value};

fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

#[test]
fn targeted_conflicts_keep_identity_old_values_and_typed_excluded_candidates() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(
        &c,
        "INSERT INTO docs {id:docs:a,email:'one',tenant:1,n:3,flag:true}",
    );
    q(&c, "CREATE UNIQUE INDEX address ON docs(tenant,email)");
    let result=q(&c,"INSERT INTO docs(id,tenant,email,n,flag) VALUES(docs:b,1,'one',5,false) ON CONFLICT(tenant,email) DO UPDATE SET n=n+excluded.n, flag=excluded.flag RETURNING id,n,flag,doc::before(),doc::after(),doc::diff()");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], q(&c, "SELECT id FROM docs").rows[0][0]);
    assert_eq!(result.rows[0][1], Value::Integer(8));
    assert_eq!(result.rows[0][2], Value::Integer(0));
    let Value::Object(before) = &result.rows[0][3] else {
        panic!()
    };
    let Value::Object(after) = &result.rows[0][4] else {
        panic!()
    };
    assert_eq!(before["n"], Value::Integer(3));
    assert_eq!(after["n"], Value::Integer(8));
    assert_eq!(before["id"], after["id"]);
    assert_eq!(q(&c, "SELECT docs:b").rows.len(), 0);
    c.execute("INSERT INTO docs(id,tenant,email,n,flag) VALUES(docs:c,1,'one',9,$flag) ON CONFLICT(tenant,email) DO UPDATE SET flag=excluded.flag,n=excluded.n RETURNING flag", &Parameters::from([("$flag".into(),Value::Boolean(false))])).map(|result|assert_eq!(result.rows,vec![vec![Value::Boolean(false)]])).unwrap();
    assert!(q(&c,"INSERT INTO docs(tenant,email,n) VALUES(1,'one',2) ON CONFLICT(tenant,email) DO UPDATE SET n=excluded.n WHERE n<excluded.n RETURNING *").rows.is_empty());
    assert_eq!(
        q(&c, "SELECT n FROM docs").rows,
        vec![vec![Value::Integer(9)]]
    );
    q(
        &c,
        "INSERT INTO docs(id,n) VALUES(docs:a,11) ON CONFLICT(id) DO UPDATE SET n=excluded.n",
    );
    assert_eq!(
        q(&c, "SELECT n FROM docs").rows,
        vec![vec![Value::Integer(11)]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn conflict_clauses_handle_nulls_source_rows_and_multiple_unique_targets() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(&c, "INSERT INTO docs {id:docs:a,email:'one',code:'x',n:1}");
    q(&c, "INSERT INTO docs {id:docs:b,email:'two',code:'y',n:2}");
    q(&c, "CREATE UNIQUE INDEX email ON docs(email)");
    q(&c, "CREATE UNIQUE INDEX code ON docs(code)");
    q(&c,"INSERT INTO docs(email,code,n) VALUES('one','y',5) ON CONFLICT(email) DO UPDATE SET n=excluded.n ON CONFLICT(code) DO NOTHING");
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY id").rows,
        vec![vec![Value::Integer(5)], vec![Value::Integer(2)]]
    );
    assert!(q(&c,"INSERT INTO docs(email,code,n) VALUES('three','y',99) ON CONFLICT(email) DO UPDATE SET n=excluded.n ON CONFLICT DO NOTHING RETURNING *").rows.is_empty());
    q(
        &c,
        "INSERT INTO docs(email,n) VALUES(NULL,1),(NULL,2) ON CONFLICT(email) DO NOTHING",
    );
    assert_eq!(
        q(&c, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(4)]]
    );
    q(&c,"INSERT INTO docs(email,n) SELECT email,n+10 FROM docs WHERE email IS NOT NULL ON CONFLICT(email) DO UPDATE SET n=excluded.n");
    assert_eq!(
        q(&c, "SELECT n FROM docs WHERE email IS NOT NULL ORDER BY id").rows,
        vec![vec![Value::Integer(15)], vec![Value::Integer(12)]]
    );
    q(&c,"INSERT INTO docs(email,n) VALUES('one',3),('one',4) ON CONFLICT DO UPDATE SET n=n+excluded.n");
    assert_eq!(
        q(&c, "SELECT n FROM docs WHERE email='one'").rows,
        vec![vec![Value::Integer(22)]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn conflict_updates_enforce_defaults_computed_readonly_and_all_indexes_atomically() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD n ON docs TYPE integer DEFAULT 2",
        "DEFINE FIELD doubled ON docs TYPE integer VALUE(n*2)",
        "DEFINE FIELD fixed ON docs TYPE string READONLY",
        "CREATE UNIQUE INDEX email ON docs(email)",
        "CREATE UNIQUE INDEX doubled ON docs(doubled)",
        "CREATE SEARCH INDEX tags ON docs(tags) USING ARRAY",
        "CREATE SEARCH INDEX body ON docs(body) USING FULLTEXT",
        "INSERT INTO docs {id:docs:a,email:'one',n:1,fixed:'a',body:'old',tags:[1]}",
        "INSERT INTO docs {id:docs:b,email:'two',n:4,fixed:'b',body:'other',tags:[4]}",
    ] {
        q(&c, sql);
    }
    q(&c,"INSERT INTO docs(email,body,tags) VALUES('one','new',array::new(2)) ON CONFLICT(email) DO UPDATE SET n=excluded.n,body=excluded.body,tags=excluded.tags");
    assert_eq!(
        q(&c, "SELECT n,doubled FROM docs WHERE email='one'").rows,
        vec![vec![Value::Integer(2), Value::Integer(4)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT count(*) FROM docs WHERE array::contains(tags,2)"
        )
        .rows,
        vec![vec![Value::Integer(1)]]
    );
    q(&c, "BEGIN");
    q(&c, "INSERT INTO prior {n:7}");
    for sql in ["INSERT INTO docs(email,n,body) VALUES('three',3,'prefix'),('one',4,'conflict') ON CONFLICT(email) DO UPDATE SET n=excluded.n,body=excluded.body", "INSERT INTO docs(email,fixed) VALUES('one','changed') ON CONFLICT(email) DO UPDATE SET fixed=excluded.fixed"] {
        assert!(c.execute(sql,&Parameters::new()).is_err(),"{sql}");
        assert_eq!(q(&c,"SELECT count(*) FROM docs").rows,vec![vec![Value::Integer(2)]]);
        assert_eq!(q(&c,"SELECT n,body FROM docs WHERE email='one'").rows,vec![vec![Value::Integer(2),Value::String("new".into())]]);
        c.check_collection_integrity("docs",Default::default()).unwrap();
    }
    q(&c, "COMMIT");
    assert_eq!(
        q(&c, "SELECT n FROM prior").rows,
        vec![vec![Value::Integer(7)]]
    );
}

#[test]
fn conflict_validation_precedes_mutation_and_parameter_names_do_not_collide() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    q(
        &c,
        "INSERT INTO docs {id:docs:a,email:'one',n:1,nested:{a:{b:{c:'one'}}}}",
    );
    q(&c, "CREATE UNIQUE INDEX email ON docs(email)");
    q(&c, "CREATE UNIQUE INDEX deep ON docs(nested.a.b.c)");
    for sql in [
        "INSERT INTO docs(n) VALUES(2) ON CONFLICT(n) DO NOTHING",
        "INSERT INTO docs(n) VALUES(2) ON CONFLICT(email) DO UPDATE SET id=docs:x",
        "INSERT INTO docs(n) VALUES(2) ON CONFLICT(email) DO UPDATE SET n=1,n=2",
        "INSERT INTO docs(n) VALUES(2) ON CONFLICT(lower(email)) DO NOTHING",
        "INSERT INTO docs(n) VALUES(2) ON CONFLICT(email) WHERE n>0 DO NOTHING",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
        assert_eq!(
            q(&c, "SELECT count(*) FROM docs").rows,
            vec![vec![Value::Integer(1)]]
        );
    }
    let nested = Value::Object(
        [(
            "a".into(),
            Value::Object(
                [(
                    "b".into(),
                    Value::Object([("c".into(), Value::String("one".into()))].into()),
                )]
                .into(),
            ),
        )]
        .into(),
    );
    c.execute("INSERT INTO docs(nested,n) VALUES($nested,5) ON CONFLICT(nested.a.b.c) DO UPDATE SET email=excluded.nested.a.b.c,n=excluded.n+$romedb_conflict_1", &Parameters::from([("$nested".into(),nested),("$romedb_conflict_1".into(),Value::Integer(2))])).unwrap();
    assert_eq!(
        q(&c, "SELECT n FROM docs").rows,
        vec![vec![Value::Integer(7)]]
    );
}

#[test]
fn conflict_limits_reopen_and_reader_snapshots_preserve_atomic_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conflicts.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        let reader = db.connect().unwrap();
        q(&c, "INSERT INTO docs {id:docs:a,email:'one',n:1,tags:[1]}");
        q(&c, "CREATE UNIQUE INDEX email ON docs(email)");
        q(&c, "CREATE SEARCH INDEX tags ON docs(tags) USING ARRAY");
        q(&reader, "BEGIN");
        q(&reader, "SELECT * FROM docs");
        q(&c, "BEGIN");
        q(&c, "INSERT INTO prior {n:7}");
        let sql="INSERT INTO docs(email,n,tags) VALUES('one',2,array::new(2)),('two',3,array::new(3)) ON CONFLICT(email) DO UPDATE SET n=excluded.n,tags=excluded.tags RETURNING *";
        let limits = romedb::ResultLimits {
            max_rows: 10,
            max_payload_bytes: 65536,
        };
        let failure = c.write_metered(
            sql,
            &Parameters::new(),
            limits,
            romedb::WriteWorkLimits {
                max_row_mutations: Some(1),
                ..Default::default()
            },
        );
        assert_eq!(failure.outcome.unwrap_err().code(), "FDB_CANCELLED");
        assert!(failure.work.mutation_budget_exhausted);
        let failure = c.write_with_result_limits(
            sql,
            &Parameters::new(),
            romedb::ResultLimits {
                max_rows: 1,
                ..limits
            },
        );
        assert_eq!(failure.unwrap_err().code(), "FDB_LIMIT");
        assert_eq!(
            q(&c, "SELECT n FROM docs").rows,
            vec![vec![Value::Integer(1)]]
        );
        assert_eq!(
            q(&c, "SELECT n FROM prior").rows,
            vec![vec![Value::Integer(7)]]
        );
        let result = c.write_metered(sql, &Parameters::new(), limits, Default::default());
        assert_eq!(result.outcome.unwrap().rows.len(), 2);
        assert_eq!(result.work.row_mutations, 2);
        q(&c, "COMMIT");
        assert_eq!(
            q(&reader, "SELECT n FROM docs").rows,
            vec![vec![Value::Integer(1)]]
        );
        q(&reader, "COMMIT");
        assert_eq!(
            q(&reader, "SELECT n FROM docs ORDER BY n").rows,
            vec![vec![Value::Integer(2)], vec![Value::Integer(3)]]
        );
    }
    let c = Database::open(path.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(
        q(&c, "SELECT n FROM docs WHERE array::contains(tags,2)").rows,
        vec![vec![Value::Integer(2)]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}
