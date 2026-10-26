use romedb::{Database, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
fn setup() -> (Database, romedb::Connection) {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(
        &c,
        "INSERT INTO docs {id:docs:a,n:1,flag:true,profile:{city:'A'},tags:[1,2],ref:docs:b}",
    );
    q(
        &c,
        "INSERT INTO docs {id:docs:b,n:2,flag:false,profile:{city:'B'},tags:[],ref:docs:a}",
    );
    q(&c, "UPDATE docs SET data=X'31'");
    (db, c)
}
#[test]
fn derived_columns_preserve_types_paths_parameters_and_nested_stars() {
    let (_db, c) = setup();
    let expected = q(
        &c,
        "SELECT id,flag,docs.profile.city,tags,data,ref FROM docs ORDER BY id",
    );
    let result=q(&c,"SELECT q.id,q.flag,q.profile.city,q.tags,q.data,q.ref FROM (SELECT id,flag,profile,tags,data,ref FROM docs) q ORDER BY q.id");
    assert_eq!(result.rows, expected.rows);
    assert_eq!(q(&c,"SELECT r.* FROM (SELECT q.id,q.flag,q.profile.city AS city,q.tags,q.data,q.ref FROM (SELECT id,flag,profile,tags,data,ref FROM docs) q) r ORDER BY r.id").rows,expected.rows);
    assert_eq!(
        q(
            &c,
            "SELECT q.document.profile.city FROM (SELECT * FROM docs) q ORDER BY q.document.id"
        )
        .rows,
        vec![
            vec![Value::String("A".into())],
            vec![Value::String("B".into())]
        ]
    );
    let params = Parameters::from([
        ("$min".into(), Value::Integer(2)),
        ("$offset".into(), Value::Integer(10)),
    ]);
    assert_eq!(
        c.execute(
            "SELECT q.n+$offset FROM (SELECT n FROM docs WHERE n >= $min) q",
            &params
        )
        .unwrap()
        .rows,
        vec![vec![Value::Integer(12)]]
    );
    assert_eq!(q(&c,"SELECT q.* FROM (SELECT n+1 AS next,doc::get(profile,'$.city') AS city FROM docs) q ORDER BY q.next").rows,vec![vec![Value::Integer(2),Value::String("A".into())],vec![Value::Integer(3),Value::String("B".into())]]);
    assert_eq!(
        q(&c, "SELECT * FROM (SELECT 42 AS answer) q").rows,
        vec![vec![Value::Integer(42)]]
    );
    let positional = Parameters::from([
        ("?1".into(), Value::Integer(10)),
        ("?2".into(), Value::Integer(2)),
    ]);
    assert_eq!(
        c.execute(
            "SELECT q.n+? FROM (SELECT n FROM docs WHERE n>=?) q",
            &positional
        )
        .unwrap()
        .rows,
        vec![vec![Value::Integer(12)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT record::fetch(q.ref) FROM (SELECT id,ref FROM docs) q ORDER BY q.id"
        )
        .rows,
        q(&c, "SELECT record::fetch(ref) FROM docs ORDER BY id").rows
    );
    q(&c, "UPDATE docs SET embedding=vector32('[1,0]')");
    assert_eq!(
        q(
            &c,
            "SELECT q.embedding FROM (SELECT id,embedding FROM docs) q ORDER BY q.id"
        )
        .rows,
        q(&c, "SELECT embedding FROM docs ORDER BY id").rows
    );
    let empty = q(&c, "SELECT q.* FROM (SELECT id,flag FROM docs WHERE n=0) q");
    assert_eq!(empty.columns, vec!["id", "flag"]);
    assert!(empty.rows.is_empty());
}
#[test]
fn derived_sources_support_grouping_distinct_and_outer_joins() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE labels(n INTEGER,label TEXT)");
    q(&c, "INSERT INTO labels VALUES (1,'one'),(3,'three')");
    assert_eq!(q(&c,"SELECT q.n,l.label FROM (SELECT n FROM docs) q LEFT JOIN labels l ON q.n=l.n ORDER BY q.n").rows,q(&c,"SELECT d.n,l.label FROM docs d LEFT JOIN labels l ON d.n=l.n ORDER BY d.n").rows);
    assert_eq!(q(&c,"SELECT l.n,q.flag,q.data FROM labels l LEFT JOIN (SELECT n,flag,data FROM docs) q ON l.n=q.n ORDER BY l.n").rows,vec![vec![Value::Integer(1),Value::Boolean(true),Value::Binary(vec![49])],vec![Value::Integer(3),Value::Null,Value::Null]]);
    assert_eq!(
        q(
            &c,
            "SELECT q.total FROM (SELECT sum(n) AS total FROM docs) q WHERE q.total>2"
        )
        .rows,
        vec![vec![Value::Integer(3)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT q.data,count(*) FROM (SELECT DISTINCT data FROM docs) q GROUP BY q.data"
        )
        .rows,
        vec![vec![Value::Binary(vec![49]), Value::Integer(1)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT q.n FROM (SELECT n FROM docs ORDER BY n DESC LIMIT 1) q"
        )
        .rows,
        vec![vec![Value::Integer(2)]]
    );
}
#[test]
fn derived_write_sources_validate_and_keep_unsupported_forms_guarded() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE copied");
    q(
        &c,
        "DEFINE FIELD n ON copied TYPE integer REQUIRED CHECK (n<2)",
    );
    assert!(c
        .execute(
            "INSERT INTO copied (n) SELECT q.n FROM (SELECT n FROM docs ORDER BY n) q",
            &Parameters::new()
        )
        .is_err());
    assert!(q(&c, "SELECT * FROM copied").rows.is_empty());
    q(&c, "CREATE TABLE native(n INTEGER)");
    q(&c, "BEGIN");
    let params = Parameters::from([("$min".into(), Value::Integer(2))]);
    assert_eq!(
        c.execute(
            "INSERT INTO native SELECT q.n FROM (SELECT n FROM docs WHERE n >= $min) q RETURNING n",
            &params
        )
        .unwrap()
        .rows,
        vec![vec![Value::Integer(2)]]
    );
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT * FROM native").rows.is_empty());
    for sql in [
        "SELECT q.* FROM (SELECT record::fetch(ref) AS target FROM docs) q",
        "SELECT q.* FROM (SELECT '__romedb_pack'(n) FROM docs) q",
        "SELECT q.* FROM (SELECT * FROM '__romedb_catalog') q",
        "SELECT q.missing FROM (SELECT n FROM docs) q",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
}

#[test]
fn unnamed_derived_collections_preserve_values_and_join_scope() {
    let (_db, c) = setup();
    for (anonymous, named) in [
        ("SELECT n,flag,data FROM (SELECT n,flag,data FROM docs) ORDER BY n", "SELECT n,flag,data FROM (SELECT n,flag,data FROM docs) d ORDER BY n"),
        ("SELECT n,m FROM (SELECT n FROM docs) JOIN (SELECT n AS m FROM docs) ON n=m ORDER BY n", "SELECT n,m FROM (SELECT n FROM docs) a JOIN (SELECT n AS m FROM docs) b ON n=m ORDER BY n"),
        ("SELECT * FROM (SELECT flag,data FROM docs WHERE n=1)", "SELECT * FROM (SELECT flag,data FROM docs WHERE n=1) d"),
        ("SELECT flag,data FROM (SELECT flag,data FROM (SELECT flag,data FROM docs))", "SELECT flag,data FROM docs"),
        ("SELECT n,m FROM (SELECT n+0 AS n FROM docs) JOIN (SELECT n+0 AS m FROM docs) ON n=m ORDER BY n", "SELECT a.n AS n,b.m AS m FROM (SELECT n+0 AS n FROM docs) a JOIN (SELECT n+0 AS m FROM docs) b ON a.n=b.m ORDER BY a.n"),
    ] {
        let expected = q(&c, named);
        let actual = q(&c, anonymous);
        assert_eq!(actual.columns, expected.columns, "{anonymous}");
        assert_eq!(actual.rows, expected.rows, "{anonymous}");
        assert_eq!(c.profile_select(anonymous, &Parameters::new()).unwrap().result.rows, expected.rows, "{anonymous}");
    }
    assert!(c
        .execute(
            "SELECT n FROM (SELECT n FROM docs) __romedb_anonymous_0",
            &Parameters::new()
        )
        .is_err());
    assert!(c
        .execute(
            "SELECT n FROM (SELECT n FROM docs) JOIN (SELECT n FROM docs) ON 1=1",
            &Parameters::new()
        )
        .is_err());
    q(&c, "CREATE TABLE copied");
    q(
        &c,
        "INSERT INTO copied (n,flag,data) SELECT n,flag,data FROM (SELECT n,flag,data FROM docs)",
    );
    assert_eq!(
        q(&c, "SELECT n,flag,data FROM copied ORDER BY n").rows,
        q(&c, "SELECT n,flag,data FROM docs ORDER BY n").rows
    );
}

#[test]
fn unnamed_derived_sources_preserve_parameters_and_correlation() {
    let (_db, c) = setup();
    let mut params = Parameters::new();
    params.insert("$min".into(), Value::Integer(1));
    params.insert("$extra".into(), Value::Integer(5));
    for source in ["docs d", "(SELECT n FROM docs) d"] {
        for (anonymous, named) in [
            ("SELECT n+$extra FROM (SELECT n FROM docs WHERE n>=$min) WHERE n>=d.n ORDER BY n LIMIT 1", "SELECT n+$extra FROM (SELECT n FROM docs WHERE n>=$min) i WHERE n>=d.n ORDER BY n LIMIT 1"),
            ("SELECT flag FROM (SELECT n,flag FROM docs WHERE n>=$min) WHERE n=d.n AND $extra=5", "SELECT flag FROM (SELECT n,flag FROM docs WHERE n>=$min) i WHERE n=d.n AND $extra=5"),
        ] {
            let expected_sql = format!("SELECT n,({named}) FROM {source} ORDER BY n");
            let sql = format!("SELECT n,({anonymous}) FROM {source} ORDER BY n");
            let expected = c.execute(&expected_sql, &params).unwrap().rows;
            let values = if anonymous.starts_with("SELECT flag") {
                [Value::Boolean(true), Value::Boolean(false)]
            } else {
                [Value::Integer(6), Value::Integer(7)]
            };
            assert_eq!(expected, vec![vec![Value::Integer(1), values[0].clone()], vec![Value::Integer(2), values[1].clone()]]);
            let mut missing = params.clone();
            missing.remove("$min");
            assert!(matches!(c.execute(&sql, &missing), Err(romedb::Error::Parameter(_))));
            assert!(matches!(c.profile_select(&sql, &missing), Err(romedb::Error::Parameter(_))));

            assert_eq!(c.execute(&sql, &params).expect(&sql).rows, expected, "{sql}");
            assert_eq!(c.profile_select(&sql, &params).expect(&sql).result.rows, expected, "{sql}");
        }
    }
}

#[test]
fn mixed_unnamed_derived_sources_preserve_native_columns() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE labels(m INTEGER,label TEXT)");
    q(&c, "INSERT INTO labels VALUES(1,'A'),(3,'C')");
    for sql in [
        "SELECT n,flag,label FROM (SELECT n,flag FROM docs) LEFT JOIN (SELECT m,label FROM labels) ON n=m ORDER BY n",
        "SELECT n,flag,label FROM (SELECT m,label FROM labels) RIGHT JOIN (SELECT n,flag FROM docs) ON n=m ORDER BY n",
    ] {
        let expected = vec![vec![Value::Integer(1),Value::Boolean(true),Value::String("A".into())],vec![Value::Integer(2),Value::Boolean(false),Value::Null]];
        assert_eq!(q(&c,sql).rows, expected, "{sql}");
        assert_eq!(c.profile_select(sql,&Parameters::new()).unwrap().result.rows,expected,"{sql}");
    }
    let sql = "SELECT * FROM (SELECT flag FROM docs WHERE n=1) CROSS JOIN (SELECT label FROM labels WHERE m=1)";
    let result = q(&c, sql);
    assert_eq!(result.columns, vec!["flag", "label"]);
    assert_eq!(
        result.rows,
        vec![vec![Value::Boolean(true), Value::String("A".into())]]
    );
}

#[test]
fn mixed_derived_comparisons_preserve_native_affinity_and_collation() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs {n:1,v:'a'}",
        "INSERT INTO docs {n:2,v:'01'}",
        "INSERT INTO docs {n:3,v:null}",
        "INSERT INTO docs {n:6,v:1}",
        "INSERT INTO docs {n:7,v:1.5}",
        "INSERT INTO docs (n,v) VALUES(4,X'61'),(5,X'464442017061796C6F6164')",
        "CREATE TABLE labels(m INTEGER,label TEXT COLLATE NOCASE)",
        "INSERT INTO labels VALUES(1,'A'),(2,'1'),(3,NULL),(4,X'61'),(5,X'464442017061796C6F6164'),(6,'1.5')",
    ] {
        q(&c, sql);
    }
    for native_source in [
        "(SELECT m,label FROM labels)",
        "(SELECT m,label FROM (SELECT m,label FROM labels))",
        "(SELECT m,label COLLATE BINARY AS label FROM labels)",
        "(WITH l AS MATERIALIZED (SELECT m,label FROM labels) SELECT m,label FROM l)",
        "(SELECT m,label FROM labels ORDER BY m LIMIT 3)",
        "(SELECT m,label FROM labels WHERE m<=1 UNION ALL SELECT m,label FROM labels WHERE m>1)",
        "(SELECT m,label FROM labels UNION SELECT m,label FROM labels)",
        "(SELECT m,label COLLATE BINARY AS label FROM labels WHERE m<=1 UNION ALL SELECT m,label FROM labels WHERE m>1)",
        "(SELECT m,label FROM labels WHERE m<=1 UNION ALL SELECT m,label COLLATE BINARY AS label FROM labels WHERE m>1)",
    ] {
        for predicate in [
            "v=label",
            "label=v",
            "v IS label",
            "v IS NOT label",
            "v COLLATE BINARY IS label",
            "v COLLATE NOCASE IS label",
            "v COLLATE BINARY IS NOT label",
            "label IS v",
            "label IS NOT v",
            "v<label",
            "label IN (v)",
            "(+label) IN (v)",
            "((+label)) NOT IN (v,NULL)",
            "(+label) COLLATE BINARY IN (v)",
            "label NOT IN (v)",
            "label COLLATE BINARY IN (v)",
            "label COLLATE NOCASE IN (v,NULL)",
            "label NOT IN (v,NULL)",
            "label IN (v,NULL)",
            "label IN (v,'z')",
            "v=+label",
            "v=(+label)",
            "v=((+label))",
        ] {
            let sql = format!("SELECT n,m FROM (SELECT n,v FROM docs) CROSS JOIN {native_source} WHERE {predicate} ORDER BY n,m");
            // Document values have no declared SQL column affinity/collation.
            // Literal operands isolate the native right-hand column semantics.
            let mut expected = Vec::new();
            for (n, value) in [(1, "'a'"), (2, "'01'"), (3, "NULL"), (4, "X'61'"), (5, "X'464442017061796C6F6164'"), (6, "1"), (7, "1.5")] {
                let condition = predicate.replace('v', value);
                expected.extend(
                    q(
                        &c,
                        &format!("SELECT {n},m FROM {native_source} WHERE {condition} ORDER BY m"),
                    )
                    .rows,
                );
            }
            assert_eq!(q(&c, &sql).rows, expected, "{sql}");
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected,
                "{sql}"
            );
        }
    }
}

#[test]
fn mixed_compound_comparison_writes_preserve_atomicity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs {n:1,v:'a'}",
        "INSERT INTO docs {n:2,v:'a'}",
        "CREATE TABLE labels(m INTEGER,label TEXT COLLATE NOCASE)",
        "INSERT INTO labels VALUES(1,'A'),(2,'1')",
        "CREATE TABLE copied",
        "DEFINE FIELD n ON copied TYPE integer REQUIRED CHECK (n<2)",
        "BEGIN",
        "INSERT INTO copied {n:0}",
    ] {
        q(&c, sql);
    }
    let source = "FROM (SELECT n,v FROM docs) CROSS JOIN (SELECT m,label COLLATE BINARY AS label FROM labels WHERE m=1 UNION ALL SELECT m,label FROM labels WHERE m=2)";
    let sql = format!(
        "INSERT INTO copied (n) SELECT n {source} WHERE v COLLATE NOCASE IS label ORDER BY n"
    );
    assert!(c.execute(&sql, &Parameters::new()).is_err());
    assert_eq!(
        q(&c, "SELECT n FROM copied").rows,
        vec![vec![Value::Integer(0)]]
    );
    q(
        &c,
        &format!("INSERT INTO copied (n) SELECT n {source} WHERE label IN (v)"),
    );
    assert_eq!(
        q(&c, "SELECT n FROM copied").rows,
        vec![vec![Value::Integer(0)]]
    );
    q(
        &c,
        &format!(
            "INSERT INTO copied (n) SELECT n {source} WHERE v COLLATE NOCASE IS label AND n=1"
        ),
    );
    assert_eq!(
        q(&c, "SELECT n FROM copied ORDER BY n").rows,
        vec![vec![Value::Integer(0)], vec![Value::Integer(1)]]
    );
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT n FROM copied").rows.is_empty());
}

#[test]
fn mixed_derived_metadata_preserves_positional_bindings() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE labels(m INTEGER,label TEXT)");
    q(&c, "INSERT INTO labels VALUES(1,'A'),(2,'B')");
    q(&c, "CREATE TABLE baseline(n INTEGER)");
    q(&c, "INSERT INTO baseline VALUES(1),(2)");
    let params = Parameters::from([
        ("?1".into(), Value::Integer(10)),
        ("?2".into(), Value::Integer(1)),
        ("?3".into(), Value::String("B".into())),
    ]);
    for sql in [
        "SELECT n+?,label FROM (SELECT n FROM docs WHERE n>=?) JOIN (SELECT m,label FROM labels WHERE label=?) ON n=m",
        "SELECT n+?1,label FROM (SELECT n FROM docs WHERE n>=?2) JOIN (SELECT m,label FROM labels WHERE label=?3) ON n=m",
    ] {
        let expected = vec![vec![Value::Integer(12),Value::String("B".into())]];
        assert_eq!(c.execute(sql,&params).expect(sql).rows,expected,"{sql}");
        assert_eq!(c.profile_select(sql,&params).expect(sql).result.rows,expected,"{sql}");
        let mut missing = params.clone(); missing.remove("?3");
        let native = sql.replace("FROM docs", "FROM baseline");
        let unbound = c.execute(&native,&missing).unwrap().rows;
        assert!(unbound.is_empty());
        assert_eq!(c.execute(sql,&missing).unwrap().rows,unbound);
        assert_eq!(c.profile_select(sql,&missing).unwrap().result.rows,unbound);

        assert_eq!(c.execute(sql,&params).expect(sql).rows,expected,"{sql}");
    }
}

#[test]
fn derived_collection_joins_resolve_relational_columns() {
    let (_db, c) = setup();
    q(
        &c,
        "CREATE TABLE labels(m INTEGER,label TEXT COLLATE NOCASE)",
    );
    q(&c, "INSERT INTO labels VALUES(1,'A'),(2,'B')");
    q(&c, "CREATE VIEW label_view AS SELECT m,label FROM labels");
    for source in ["labels", "label_view"] {
        let sql = format!(
            "SELECT n,flag,label FROM (SELECT n,flag FROM docs) JOIN {source} ON n=m ORDER BY n"
        );
        let expected = q(
            &c,
            &format!(
                "SELECT d.n,d.flag,l.label FROM docs d JOIN {source} l ON d.n=l.m ORDER BY d.n"
            ),
        )
        .rows;
        assert_eq!(q(&c, &sql).rows, expected, "{sql}");
        assert_eq!(
            c.profile_select(&sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected,
            "{sql}"
        );
    }
    q(&c, "DELETE FROM labels WHERE m=2");
    for source in ["labels", "label_view"] {
        let sql = format!("SELECT n,flag,label FROM (SELECT n,flag FROM docs) LEFT JOIN {source} ON n=m ORDER BY n");
        let expected = vec![
            vec![
                Value::Integer(1),
                Value::Boolean(true),
                Value::String("A".into()),
            ],
            vec![Value::Integer(2), Value::Boolean(false), Value::Null],
        ];
        assert_eq!(q(&c, &sql).rows, expected, "{sql}");
        assert_eq!(
            c.profile_select(&sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected,
            "{sql}"
        );
        let ambiguous = format!("SELECT m FROM (SELECT n AS m FROM docs) JOIN {source} ON 1=1");
        assert!(
            matches!(
                c.execute(&ambiguous, &Parameters::new()),
                Err(romedb::Error::Validation(_))
            ),
            "{ambiguous}"
        );
        assert!(
            matches!(
                c.profile_select(&ambiguous, &Parameters::new()),
                Err(romedb::Error::Validation(_))
            ),
            "{ambiguous}"
        );
        assert_eq!(q(&c, &sql).rows, expected, "{sql}");
    }
    assert!(c
        .execute(
            "SELECT n FROM (SELECT n FROM docs) JOIN (SELECT m AS n FROM labels) ON 1=1",
            &Parameters::new()
        )
        .is_err());
}

#[test]
fn relational_join_metadata_refreshes_after_schema_change() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE labels(m INTEGER,label TEXT)");
    q(&c, "INSERT INTO labels VALUES(1,'A')");
    let sql = "SELECT n,label FROM (SELECT n FROM docs) JOIN labels ON n=m";
    let expected = vec![vec![Value::Integer(1), Value::String("A".into())]];
    assert_eq!(q(&c, sql).rows, expected);
    q(&c, "ALTER TABLE labels ADD COLUMN n INTEGER");
    assert!(matches!(
        c.execute(sql, &Parameters::new()),
        Err(romedb::Error::Validation(_))
    ));
    assert!(matches!(
        c.profile_select(sql, &Parameters::new()),
        Err(romedb::Error::Validation(_))
    ));
    let qualified = "SELECT d.n,label FROM (SELECT n FROM docs) d JOIN labels l ON d.n=l.m";
    assert_eq!(q(&c, qualified).rows, expected);
    q(&c, "CREATE VIEW label_view AS SELECT m,label FROM labels");
    let view_sql = sql.replace("JOIN labels", "JOIN label_view");
    assert_eq!(q(&c, &view_sql).rows, expected);
    q(&c, "DROP VIEW label_view");
    q(&c, "CREATE VIEW label_view AS SELECT m,label,n FROM labels");
    assert!(matches!(
        c.execute(&view_sql, &Parameters::new()),
        Err(romedb::Error::Validation(_))
    ));
    q(&c, "DROP VIEW label_view");
    q(&c, "CREATE VIEW label_view AS SELECT m,label FROM labels");
    assert_eq!(
        c.profile_select(&view_sql, &Parameters::new())
            .unwrap()
            .result
            .rows,
        expected
    );
}

#[test]
fn relational_join_metadata_refreshes_across_connections() {
    let (db, c) = setup();
    let writer = db.connect().unwrap();
    q(&c, "CREATE TABLE labels(m INTEGER,label TEXT)");
    q(&c, "INSERT INTO labels VALUES(1,'A')");
    let sql = "SELECT n,label FROM (SELECT n FROM docs) JOIN labels ON n=m";
    let expected = vec![vec![Value::Integer(1), Value::String("A".into())]];
    assert_eq!(q(&c, sql).rows, expected);
    q(&writer, "ALTER TABLE labels ADD COLUMN n INTEGER");
    assert!(matches!(
        c.execute(sql, &Parameters::new()),
        Err(romedb::Error::Validation(_))
    ));
    assert!(matches!(
        c.profile_select(sql, &Parameters::new()),
        Err(romedb::Error::Validation(_))
    ));
    let qualified = "SELECT d.n,label FROM (SELECT n FROM docs) d JOIN labels l ON d.n=l.m";
    assert_eq!(q(&c, qualified).rows, expected);
    q(
        &writer,
        "CREATE VIEW label_view AS SELECT m,label FROM labels",
    );
    let view_sql = sql.replace("JOIN labels", "JOIN label_view");
    assert_eq!(q(&c, &view_sql).rows, expected);
    q(&writer, "DROP VIEW label_view");
    q(
        &writer,
        "CREATE VIEW label_view AS SELECT m,label,n FROM labels",
    );
    assert!(matches!(
        c.execute(&view_sql, &Parameters::new()),
        Err(romedb::Error::Validation(_))
    ));
}

#[test]
fn relational_join_metadata_tracks_schema_rollback() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE labels(m INTEGER,label TEXT)");
    q(&c, "INSERT INTO labels VALUES(1,'A')");
    let sql = "SELECT n,label FROM (SELECT n FROM docs) JOIN labels ON n=m";
    let expected = q(&c, sql).rows;
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs {n:3}");
    q(&c, "ALTER TABLE labels ADD COLUMN n INTEGER");
    assert!(matches!(
        c.execute(sql, &Parameters::new()),
        Err(romedb::Error::Validation(_))
    ));
    q(&c, "ROLLBACK");
    assert_eq!(q(&c, sql).rows, expected);
    assert_eq!(
        c.profile_select(sql, &Parameters::new())
            .unwrap()
            .result
            .rows,
        expected
    );
    assert_eq!(
        q(&c, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(2)]]
    );
    q(&c, "CREATE VIEW label_view AS SELECT m,label FROM labels");
    let view_sql = sql.replace("JOIN labels", "JOIN label_view");
    q(&c, "BEGIN");
    q(&c, "DROP VIEW label_view");
    q(
        &c,
        "CREATE VIEW label_view AS SELECT m AS n,label FROM labels",
    );
    assert!(c.execute(&view_sql, &Parameters::new()).is_err());
    q(&c, "ROLLBACK");
    assert_eq!(q(&c, &view_sql).rows, expected);
}

#[test]
fn relational_join_metadata_includes_computed_view_columns() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE label_data(m INTEGER)");
    q(
        &c,
        "CREATE VIEW labels AS SELECT m,'item-' || m AS label FROM label_data",
    );
    q(&c, "INSERT INTO label_data(m) VALUES(1),(2)");
    for sql in [
        "SELECT n,label FROM (SELECT n FROM docs) JOIN labels ON n=m ORDER BY n",
        "SELECT n,label FROM (SELECT n FROM docs) JOIN (SELECT * FROM labels) ON n=m ORDER BY n",
    ] {
        let expected = vec![
            vec![Value::Integer(1), Value::String("item-1".into())],
            vec![Value::Integer(2), Value::String("item-2".into())],
        ];
        assert_eq!(q(&c, sql).rows, expected, "{sql}");
        assert_eq!(
            c.profile_select(sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected,
            "{sql}"
        );
    }
    let star = q(
        &c,
        "SELECT * FROM (SELECT n FROM docs WHERE n=1) JOIN labels ON n=m",
    );
    assert_eq!(star.columns, vec!["n", "m", "label"]);
    assert_eq!(
        star.rows,
        vec![vec![
            Value::Integer(1),
            Value::Integer(1),
            Value::String("item-1".into())
        ]]
    );
}

#[test]
fn relational_join_metadata_preserves_rowid_resolution() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE labels(m INTEGER,label TEXT)");
    q(
        &c,
        "INSERT INTO labels(rowid,m,label) VALUES(10,1,'A'),(20,2,'B')",
    );
    q(&c, "CREATE TABLE baseline(n INTEGER)");
    q(&c, "INSERT INTO baseline VALUES(1),(2)");
    let ambiguous = "SELECT n,rowid,label FROM (SELECT n FROM docs) JOIN labels ON n=m ORDER BY n";
    let native_error = c
        .execute(
            &ambiguous.replace("FROM docs", "FROM baseline"),
            &Parameters::new(),
        )
        .unwrap_err()
        .to_string();
    assert!(native_error.contains("ROWID is ambiguous"));
    assert_eq!(
        c.execute(ambiguous, &Parameters::new())
            .unwrap_err()
            .to_string(),
        native_error
    );
    let sql = "SELECT n,labels.rowid,label FROM (SELECT n FROM docs) JOIN labels ON n=m ORDER BY n";
    let expected = vec![
        vec![
            Value::Integer(1),
            Value::Integer(10),
            Value::String("A".into()),
        ],
        vec![
            Value::Integer(2),
            Value::Integer(20),
            Value::String("B".into()),
        ],
    ];
    assert_eq!(q(&c, sql).rows, expected);
    assert_eq!(
        c.profile_select(sql, &Parameters::new())
            .unwrap()
            .result
            .rows,
        expected
    );
    let explicit =
        "SELECT rowid FROM (SELECT n AS rowid FROM docs) JOIN labels ON rowid=m ORDER BY rowid";
    assert_eq!(
        q(&c, explicit).rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}

#[test]
fn closed_derived_sources_preserve_group_alias_precedence() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE baseline(n INTEGER)");
    q(&c, "INSERT INTO baseline VALUES(1),(2)");
    q(&c, "CREATE TABLE labels(m INTEGER)");
    q(&c, "INSERT INTO labels VALUES(1),(2)");
    for sql in [
        "SELECT 0 AS n,count(*) AS total FROM (SELECT n FROM docs) GROUP BY n ORDER BY total",
        "SELECT 0 AS n,count(*) AS total FROM (SELECT n FROM docs) WHERE n=1 GROUP BY n",
        "SELECT 0 AS N,count(*) AS total FROM (SELECT n FROM docs) GROUP BY (n) ORDER BY total",
        "SELECT 0 AS m,count(*) AS total FROM (SELECT n FROM docs) JOIN labels ON n=m GROUP BY m ORDER BY total",
        "SELECT 0 AS n,count(*) AS total FROM (SELECT n FROM docs) JOIN labels ON n=m GROUP BY n ORDER BY total",
        "SELECT n%2 AS parity,count(*) AS total FROM (SELECT n FROM docs) GROUP BY parity ORDER BY parity",
        "SELECT 0 AS n,count(*) AS total FROM (SELECT n FROM docs) GROUP BY n HAVING n=0 ORDER BY total",
        "SELECT 0 AS n,count(*) AS total FROM (SELECT n FROM docs) GROUP BY n HAVING n=1 ORDER BY total",
        "SELECT count(*) AS n FROM (SELECT n FROM docs) HAVING n=2",
        "SELECT count(*) AS n FROM (SELECT n FROM docs) HAVING abs(n)=2",

    ] {
        let expected = q(&c, &sql.replace("FROM docs", "FROM baseline")).rows;
        assert_eq!(q(&c, sql).rows, expected, "{sql}");
        assert_eq!(c.profile_select(sql, &Parameters::new()).unwrap().result.rows, expected, "{sql}");
    }
}

#[test]
fn closed_source_group_alias_writes_restore_unique_indexes() {
    let (_db, c) = setup();
    for sql in [
        "CREATE TABLE copied",
        "CREATE UNIQUE INDEX copied_n ON copied(n)",
        "BEGIN",
        "INSERT INTO copied {n:9,total:9}",
    ] {
        q(&c, sql);
    }
    let insert = "INSERT INTO copied(n,total) SELECT 0 AS n,count(*) AS total FROM (SELECT n FROM docs) GROUP BY n";
    // Two distinct source groups project the same unique key. The second write
    // must fail and restore the first write as well as its managed index entry.
    assert!(c.execute(insert, &Parameters::new()).is_err());
    assert_eq!(
        q(&c, "SELECT n,total FROM copied").rows,
        vec![vec![Value::Integer(9), Value::Integer(9)]]
    );
    assert!(q(&c, "SELECT total FROM copied WHERE n=0").rows.is_empty());
    q(&c, "INSERT INTO copied(n,total) SELECT 0 AS n,count(*) AS total FROM (SELECT n FROM docs) WHERE n=1 GROUP BY n");
    assert_eq!(
        q(&c, "SELECT total FROM copied WHERE n=0").rows,
        vec![vec![Value::Integer(1)]]
    );
    assert_eq!(
        c.lookup_index("copied", "copied_n", &Value::Integer(0))
            .unwrap()
            .len(),
        1
    );
    q(&c, "ROLLBACK");
    assert!(c
        .lookup_index("copied", "copied_n", &Value::Integer(0))
        .unwrap()
        .is_empty());
    assert!(q(&c, "SELECT n FROM copied").rows.is_empty());
    q(&c, "INSERT INTO copied {n:0,total:2}");
    assert_eq!(
        q(&c, "SELECT total FROM copied WHERE n=0").rows,
        vec![vec![Value::Integer(2)]]
    );
}

#[test]
fn projection_aliases_do_not_replace_nested_source_columns() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE baseline(n INTEGER)");
    q(&c, "INSERT INTO baseline VALUES(1),(2)");
    q(&c, "CREATE TABLE labels(m INTEGER)");
    q(&c, "INSERT INTO labels VALUES(1)");
    for predicate in [
        "EXISTS (SELECT 1 FROM labels WHERE m=1)",
        "(SELECT m FROM labels)=1",
        "n IN (SELECT m FROM labels)",
        "m IN (SELECT m FROM labels)",
        "m NOT IN (SELECT m FROM labels)",
        "(m+1) IN (SELECT m FROM labels)",
        "(m IN (SELECT m FROM labels)) IN (SELECT m FROM labels)",
    ] {
        let sql = format!("SELECT n,0 AS m FROM (SELECT n FROM docs) WHERE {predicate} ORDER BY n");
        let expected = q(&c, &sql.replace("FROM docs", "FROM baseline")).rows;
        assert_eq!(q(&c, &sql).rows, expected, "{sql}");
        assert_eq!(
            c.profile_select(&sql, &Parameters::new())
                .unwrap()
                .result
                .rows,
            expected,
            "{sql}"
        );
    }
}

#[test]
fn membership_aliases_preserve_correlated_bindings() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE baseline(n INTEGER)");
    q(&c, "INSERT INTO baseline VALUES(1),(2)");
    q(&c, "CREATE TABLE labels(m INTEGER)");
    q(&c, "INSERT INTO labels VALUES(1),(2)");
    for source in [
        "SELECT m FROM labels WHERE m=d.n AND m>$minimum",
        "SELECT x.n FROM docs x WHERE x.n=d.n AND x.n>$minimum",
    ] {
        for operand in ["candidate", "candidate+1"] {
            for operator in ["IN", "NOT IN"] {
                let sql = format!("SELECT d.n,$value AS candidate FROM (SELECT n FROM docs) d WHERE {operand} {operator} ({source}) ORDER BY d.n");
                for (value, minimum) in [(0, 0), (1, 0), (2, 0), (1, 2)] {
                    let params = Parameters::from([
                        ("$value".into(), Value::Integer(value)),
                        ("$minimum".into(), Value::Integer(minimum)),
                    ]);
                    let expected = c
                        .execute(&sql.replace("FROM docs", "FROM baseline"), &params)
                        .unwrap()
                        .rows;
                    assert_eq!(
                        c.execute(&sql, &params).unwrap().rows,
                        expected,
                        "{sql}, {value}"
                    );
                    assert_eq!(
                        c.profile_select(&sql, &params).unwrap().result.rows,
                        expected,
                        "{sql}, {value}"
                    );
                }
            }
        }
    }
}

#[test]
fn membership_aliases_preserve_document_value_types() {
    let (_db, c) = setup();
    q(&c, "UPDATE docs SET data=X'46444201' WHERE n=2");
    for (index, values) in q(&c, "SELECT ref,flag,data FROM docs ORDER BY n")
        .rows
        .into_iter()
        .enumerate()
    {
        for (field, value) in ["ref", "flag", "data"].into_iter().zip(values) {
            let params = Parameters::from([("$value".into(), value.clone())]);
            for (operator, selected) in [("IN", index as i64 + 1), ("NOT IN", 2 - index as i64)] {
                let sql = format!("SELECT d.n,$value AS candidate FROM (SELECT n FROM docs) d WHERE candidate {operator} (SELECT x.{field} FROM docs x WHERE x.n=d.n) ORDER BY d.n");
                let expected = vec![vec![Value::Integer(selected), value.clone()]];
                assert_eq!(
                    c.execute(&sql, &params).unwrap().rows,
                    expected,
                    "{field} {operator}"
                );
                assert_eq!(
                    c.profile_select(&sql, &params).unwrap().result.rows,
                    expected,
                    "{field} {operator}"
                );
            }
        }
    }
}

#[test]
fn membership_aliases_work_in_join_and_group_clauses() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE baseline(n INTEGER)");
    q(&c, "INSERT INTO baseline VALUES(1),(2)");
    q(&c, "CREATE TABLE labels(m INTEGER)");
    q(&c, "INSERT INTO labels VALUES(1)");
    for sql in [
        "SELECT d.n,labels.m FROM (SELECT n FROM docs) d LEFT JOIN labels ON d.n IN (SELECT m FROM labels) ORDER BY d.n",
        "SELECT d.n,labels.m FROM (SELECT n FROM docs) d LEFT JOIN labels ON d.n NOT IN (SELECT m FROM labels) ORDER BY d.n",
        "SELECT d.n,labels.m,1 AS candidate FROM (SELECT n FROM docs) d JOIN labels ON candidate IN (SELECT m FROM labels) ORDER BY d.n",
        "SELECT d.n,labels.m,0 AS candidate FROM (SELECT n FROM docs) d LEFT JOIN labels ON candidate IN (SELECT m FROM labels) ORDER BY d.n",
        "SELECT 1 AS candidate,count(*) AS total FROM (SELECT n FROM docs) GROUP BY candidate IN (SELECT m FROM labels)",
        "SELECT 0 AS candidate,count(*) AS total FROM (SELECT n FROM docs) GROUP BY candidate NOT IN (SELECT m FROM labels)",
    ] {
        let mut native = sql.replace("FROM docs", "FROM baseline");
        if sql.contains("ON candidate") {
            let error = c.execute(&native, &Parameters::new()).unwrap_err().to_string();
            assert!(error.contains("no such column: candidate"));
            native = native.replace("ON candidate", if sql.contains("0 AS candidate") { "ON 0" } else { "ON 1" });
        }
        let expected = q(&c, &native).rows;
        assert_eq!(q(&c, sql).rows, expected, "{sql}");
        assert_eq!(c.profile_select(sql, &Parameters::new()).unwrap().result.rows, expected, "{sql}");
    }
}

#[test]
fn join_membership_preserves_native_values_and_nulls() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE baseline(n INTEGER)");
    q(&c, "INSERT INTO baseline VALUES(1),(2)");
    q(&c, "CREATE TABLE labels(v TEXT COLLATE NOCASE)");
    q(
        &c,
        "INSERT INTO labels VALUES('A'),('1'),(NULL),(X'46444201')",
    );
    q(&c, "CREATE TABLE marker(m INTEGER)");
    q(&c, "INSERT INTO marker VALUES(7)");
    for rhs in [
        "SELECT v FROM labels",
        "SELECT v FROM labels WHERE v IS NOT NULL",
        "SELECT v FROM labels WHERE 0",
        "SELECT v FROM labels WHERE v=$value",
    ] {
        for operator in ["IN", "NOT IN"] {
            for value in [
                Value::Null,
                Value::String("a".into()),
                Value::Integer(1),
                Value::String("absent".into()),
                Value::Binary(vec![0x46, 0x44, 0x42, 1]),
            ] {
                let params = Parameters::from([("$value".into(), value)]);
                let sql = format!("SELECT d.n,marker.m FROM (SELECT n FROM docs) d LEFT JOIN marker ON $value {operator} ({rhs}) ORDER BY d.n");
                let expected = c
                    .execute(&sql.replace("FROM docs", "FROM baseline"), &params)
                    .unwrap()
                    .rows;
                assert_eq!(
                    c.execute(&sql, &params).unwrap().rows,
                    expected,
                    "{sql}, {params:?}"
                );
                assert_eq!(
                    c.profile_select(&sql, &params).unwrap().result.rows,
                    expected,
                    "{sql}, {params:?}"
                );
            }
        }
    }
}

#[test]
fn join_membership_writes_preserve_statement_atomicity() {
    let (_db, c) = setup();
    for sql in [
        "CREATE TABLE labels(m INTEGER)",
        "INSERT INTO labels VALUES(1),(2)",
        "CREATE TABLE marker(k INTEGER)",
        "INSERT INTO marker VALUES(1)",
        "CREATE TABLE copied",
        "DEFINE FIELD n ON copied TYPE integer REQUIRED CHECK (n<2)",
        "CREATE UNIQUE INDEX copied_n ON copied(n)",
        "BEGIN",
        "INSERT INTO copied {n:0}",
    ] {
        q(&c, sql);
    }
    let source = "FROM (SELECT n FROM docs) d JOIN marker ON d.n IN (SELECT m FROM labels)";
    let insert = format!("INSERT INTO copied(n) SELECT d.n {source} ORDER BY d.n");
    assert!(c.execute(&insert, &Parameters::new()).is_err());
    assert_eq!(
        q(&c, "SELECT n FROM copied").rows,
        vec![vec![Value::Integer(0)]]
    );
    assert!(c
        .lookup_index("copied", "copied_n", &Value::Integer(1))
        .unwrap()
        .is_empty());
    q(
        &c,
        &format!("INSERT INTO copied(n) SELECT d.n {source} WHERE d.n=1"),
    );
    assert_eq!(
        c.lookup_index("copied", "copied_n", &Value::Integer(1))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        q(&c, "SELECT n FROM copied ORDER BY n").rows,
        vec![vec![Value::Integer(0)], vec![Value::Integer(1)]]
    );
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT n FROM copied").rows.is_empty());
    assert!(c
        .lookup_index("copied", "copied_n", &Value::Integer(1))
        .unwrap()
        .is_empty());
}

#[test]
fn indexed_binary_parameters_preserve_record_separation() {
    let (_db, c) = setup();
    let record = q(&c, "SELECT ref FROM docs WHERE n=1").rows[0][0].clone();
    let bytes = Value::Binary(
        b"FDB\x01{\"type\":\"Record\",\"value\":{\"table\":\"docs\",\"key\":{\"String\":\"b\"}}}"
            .to_vec(),
    );
    let params = Parameters::from([("?1".into(), bytes), ("?2".into(), record)]);
    c.execute(
        "UPDATE docs SET data=?1 WHERE n=1",
        &Parameters::from([("?1".into(), params["?1"].clone())]),
    )
    .unwrap();
    c.execute(
        "UPDATE docs SET data=?2 WHERE n=2",
        &Parameters::from([("?2".into(), params["?2"].clone())]),
    )
    .unwrap();
    q(&c, "CREATE INDEX docs_data ON docs(data)");
    for predicate in ["data IN (?1,?2)", "data IN ((?1),(?2))", "data IN (+?1,?2)"] {
        let sql = format!("SELECT n FROM docs WHERE {predicate} ORDER BY n");
        let expected = vec![vec![Value::Integer(1)], vec![Value::Integer(2)]];
        assert_eq!(c.execute(&sql, &params).unwrap().rows, expected, "{sql}");
        assert_eq!(
            c.profile_select(&sql, &params).unwrap().result.rows,
            expected,
            "{sql}"
        );
    }
    for (name, n) in [("?1", 1), ("?2", 2)] {
        let bound = Parameters::from([(name.into(), params[name].clone())]);
        let sql = format!("SELECT n FROM docs WHERE data={name}");
        assert_eq!(
            c.execute(&sql, &bound).unwrap().rows,
            vec![vec![Value::Integer(n)]]
        );
    }
}

#[test]
fn native_subqueries_preserve_binary_parameters() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE baseline(n INTEGER)");
    q(&c, "INSERT INTO baseline VALUES(1),(2)");
    q(&c, "CREATE TABLE native(n INTEGER,data BLOB)");
    let bytes = Value::Binary(b"FDB\x01{\"type\":\"Integer\",\"value\":7}".to_vec());
    let params = Parameters::from([("$data".into(), bytes)]);
    c.execute("INSERT INTO native VALUES(1,$data)", &params)
        .unwrap();
    for expression in [
        "(SELECT data FROM native WHERE data=$data)",
        "(SELECT data FROM native WHERE data=$data AND n=d.n)",
        "EXISTS (SELECT 1 FROM native WHERE data=$data AND n=d.n)",
    ] {
        let sql =
            format!("SELECT d.n,{expression} AS found FROM (SELECT n FROM docs) d ORDER BY d.n");
        let expected = c
            .execute(&sql.replace("FROM docs", "FROM baseline"), &params)
            .unwrap()
            .rows;
        // EXISTS is a typed Boolean in logical queries; compare it numerically.
        let logical = if expression.starts_with("EXISTS") {
            sql.replace(
                &format!("{expression} AS found"),
                &format!("CAST({expression} AS INTEGER) AS found"),
            )
        } else {
            sql.clone()
        };
        assert_eq!(
            c.execute(&logical, &params).unwrap().rows,
            expected,
            "{logical}"
        );
        assert_eq!(
            c.profile_select(&logical, &params).unwrap().result.rows,
            expected,
            "{logical}"
        );
    }
}

#[test]
fn native_duplicate_derived_names_keep_first_column_collation() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE baseline(n INTEGER)");
    q(&c, "INSERT INTO baseline VALUES(1),(2)");
    q(&c, "CREATE TABLE labels(a TEXT COLLATE NOCASE,b TEXT)");
    q(&c, "INSERT INTO labels VALUES('A','B')");
    for projection in ["a AS x,b AS X", "b AS x,a AS X"] {
        for predicate in ["q.x IN('a')", "q.X IN('a','b')", "q.x NOT IN('a')"] {
            let query = |source: &str| {
                format!(
                "SELECT d.n,{predicate} AS matched FROM {source} d JOIN (SELECT {projection} FROM labels) q ON 1 ORDER BY d.n"
            )
            };
            let expected = q(&c, &query("baseline"));
            let sql = query("docs");
            assert_eq!(q(&c, &sql).rows, expected.rows, "{sql}");
            assert_eq!(
                c.profile_select(&sql, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected.rows,
                "{sql}"
            );
        }
    }
}

#[test]
fn native_duplicate_derived_stars_preserve_write_positions() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE baseline(n INTEGER)");
    q(&c, "INSERT INTO baseline VALUES(1),(2)");
    q(&c, "CREATE TABLE expected(a,b,c)");
    q(&c, "CREATE TABLE actual(a,b,c)");
    for projection in [
        "10 AS x,20 AS x,X'31' AS X",
        "'A' COLLATE NOCASE AS x,'B' AS X,NULL AS x",
    ] {
        q(&c, "DELETE FROM expected");
        q(&c, "DELETE FROM actual");
        let insert = |target: &str, source: &str| {
            format!(
                "INSERT INTO {target} SELECT q.* FROM {source} d JOIN (SELECT {projection}) q ON 1"
            )
        };
        q(&c, &insert("expected", "baseline"));
        q(&c, "BEGIN");
        q(&c, &insert("actual", "docs"));
        assert_eq!(
            q(&c, "SELECT * FROM actual").rows,
            q(&c, "SELECT * FROM expected").rows
        );
        q(&c, "ROLLBACK");
        assert!(q(&c, "SELECT * FROM actual").rows.is_empty());
    }
}

#[test]
fn duplicate_derived_star_writes_preserve_native_conflict_policies() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE baseline(n INTEGER)");
    q(&c, "INSERT INTO baseline VALUES(1),(2)");
    for target in ["expected", "actual"] {
        q(
            &c,
            &format!("CREATE TABLE {target}(a INTEGER UNIQUE,b INTEGER CHECK(b<>a))"),
        );
    }
    let params = Parameters::from([
        ("$first".into(), Value::Integer(10)),
        ("$second".into(), Value::Integer(20)),
    ]);
    for policy in ["ABORT", "FAIL", "IGNORE", "REPLACE"] {
        q(&c, "BEGIN");
        for target in ["expected", "actual"] {
            q(&c, &format!("INSERT INTO {target} VALUES(99,100)"));
        }
        let insert = |target: &str, source: &str| {
            format!(
            "WITH seed AS (SELECT $first AS x,$second AS X) INSERT OR {policy} INTO {target} SELECT q.* FROM {source} d JOIN (SELECT * FROM seed) q ON 1 ORDER BY d.n"
        )
        };
        let native = c.execute(&insert("expected", "baseline"), &params);
        let logical = c.execute(&insert("actual", "docs"), &params);
        assert_eq!(
            native.is_ok(),
            matches!(policy, "IGNORE" | "REPLACE"),
            "{policy}: {native:?}"
        );
        let mut expected_rows = vec![vec![Value::Integer(99), Value::Integer(100)]];
        if policy != "ABORT" {
            expected_rows.insert(0, vec![Value::Integer(10), Value::Integer(20)]);
        }
        assert_eq!(
            q(&c, "SELECT * FROM expected ORDER BY a").rows,
            expected_rows,
            "{policy}"
        );
        assert_eq!(
            logical.is_ok(),
            native.is_ok(),
            "{policy}: {logical:?}, {native:?}"
        );
        assert_eq!(
            q(&c, "SELECT * FROM actual ORDER BY a").rows,
            q(&c, "SELECT * FROM expected ORDER BY a").rows,
            "{policy}"
        );
        // The transaction remains usable after either conflict disposition.
        q(&c, "INSERT INTO actual VALUES(30,40)");
        q(&c, "ROLLBACK");
        assert!(q(&c, "SELECT * FROM actual").rows.is_empty());
        assert!(q(&c, "SELECT * FROM expected").rows.is_empty());
    }
}

#[test]
fn duplicate_public_projection_names_preserve_positions_and_ordering() {
    let (_db, c) = setup();
    q(&c, "CREATE TABLE baseline(n INTEGER)");
    q(&c, "INSERT INTO baseline VALUES(1),(2)");
    for projection in ["n AS x,3-n AS x", "n AS x,3-n AS X"] {
        for distinct in ["", "DISTINCT "] {
            for order in ["x", "2", "x+0"] {
                let query = |source: &str| {
                    format!("SELECT {distinct}{projection} FROM {source} ORDER BY {order}")
                };
                let expected = q(&c, &query("baseline"));
                let sql = query("docs");
                let actual = q(&c, &sql);
                assert_eq!(actual.columns, expected.columns, "{sql}");
                assert_eq!(actual.rows, expected.rows, "{sql}");
                assert_eq!(
                    c.profile_select(&sql, &Parameters::new())
                        .unwrap()
                        .result
                        .rows,
                    expected.rows,
                    "{sql}"
                );
            }
        }
    }
    for suffix in ["ORDER BY d.n", "WHERE d.n=0"] {
        let query = |source: &str| {
            format!("SELECT q.* FROM {source} d JOIN (SELECT 10 AS x,20 AS x) q ON 1 {suffix}")
        };
        let expected = q(&c, &query("baseline"));
        let actual = q(&c, &query("docs"));
        assert_eq!(actual.columns, expected.columns);
        assert_eq!(actual.rows, expected.rows);
    }
    let result = q(&c, "SELECT flag AS x,data AS x FROM docs ORDER BY n");
    assert_eq!(result.columns, vec!["x", "x"]);
    assert_eq!(
        result.rows,
        vec![
            vec![Value::Boolean(true), Value::Binary(vec![49])],
            vec![Value::Boolean(false), Value::Binary(vec![49])],
        ]
    );
}

#[test]
fn duplicate_logical_derived_names_preserve_types_and_first_lookup() {
    let (_db, c) = setup();
    for projection in [
        "flag AS x,data AS X",
        "data AS x,flag AS X",
        "n+0 AS x,flag AS X",
        "flag AS x,n+0 AS X",
    ] {
        let expected = q(&c, &format!("SELECT {projection} FROM docs ORDER BY n"));
        for wrapper in [
            format!("SELECT q.* FROM (SELECT {projection} FROM docs ORDER BY n) q"),
            format!(
                "SELECT r.* FROM (SELECT q.* FROM (SELECT {projection} FROM docs ORDER BY n) q) r"
            ),
        ] {
            let actual = q(&c, &wrapper);
            assert_eq!(actual.columns, expected.columns, "{wrapper}");
            assert_eq!(actual.rows, expected.rows, "{wrapper}");
            assert_eq!(
                c.profile_select(&wrapper, &Parameters::new())
                    .unwrap()
                    .result
                    .rows,
                expected.rows
            );
        }
        let first = q(
            &c,
            &format!("SELECT q.X FROM (SELECT {projection} FROM docs ORDER BY n) q"),
        );
        assert_eq!(
            first.rows,
            expected
                .rows
                .into_iter()
                .map(|row| vec![row[0].clone()])
                .collect::<Vec<_>>()
        );
    }
    q(&c, "CREATE TABLE copied");
    q(&c, "DEFINE FIELD flag ON copied TYPE boolean");
    q(&c, "BEGIN");
    q(
        &c,
        "INSERT INTO copied(flag,data) SELECT q.* FROM (SELECT flag AS x,data AS x FROM docs) q",
    );
    assert_eq!(
        q(&c, "SELECT flag,data FROM copied ORDER BY flag").rows,
        q(&c, "SELECT flag,data FROM docs ORDER BY flag").rows
    );
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT * FROM copied").rows.is_empty());
}

#[test]
fn duplicate_derived_composites_keep_paths_records_and_indexed_writes() {
    let (_db, c) = setup();
    let sql = "SELECT q.x.city FROM (SELECT profile AS x,ref AS X FROM docs ORDER BY n) q";
    assert_eq!(
        q(&c, sql).rows,
        q(&c, "SELECT docs.profile.city FROM docs ORDER BY n").rows
    );
    assert_eq!(
        c.profile_select(sql, &Parameters::new())
            .unwrap()
            .result
            .rows,
        q(&c, sql).rows
    );
    let sql = "SELECT record::id(q.X) FROM (SELECT ref AS x,profile AS X FROM docs ORDER BY n) q";
    assert_eq!(
        q(&c, sql).rows,
        q(&c, "SELECT record::id(ref) FROM docs ORDER BY n").rows
    );
    q(&c, "CREATE TABLE copied");
    q(&c, "DEFINE FIELD profile ON copied TYPE object");
    q(&c, "CREATE UNIQUE INDEX copied_ref ON copied(ref)");
    q(&c, "BEGIN");
    let insert = "INSERT INTO copied(profile,ref) SELECT q.* FROM (SELECT profile AS x,ref AS X FROM docs) q";
    q(&c, insert);
    let expected = q(&c, "SELECT profile,ref FROM docs ORDER BY ref");
    assert_eq!(
        q(&c, "SELECT profile,ref FROM copied ORDER BY ref").rows,
        expected.rows
    );
    assert!(c.execute(insert, &Parameters::new()).is_err());
    assert_eq!(
        q(&c, "SELECT profile,ref FROM copied ORDER BY ref").rows,
        expected.rows
    );
    assert_eq!(
        q(
            &c,
            "SELECT copied.profile.city FROM copied WHERE ref=docs:b"
        )
        .rows,
        vec![vec![Value::String("A".into())]]
    );
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT * FROM copied WHERE ref=docs:b")
        .rows
        .is_empty());
    q(&c, insert);
    assert_eq!(
        q(&c, "SELECT profile,ref FROM copied ORDER BY ref").rows,
        expected.rows
    );
}

#[test]
fn unprojected_derived_sort_keys_use_logical_numeric_order() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let p = Parameters::new();
    for sql in [
        "CREATE TABLE docs",
        "INSERT INTO docs(label,n) VALUES('six',6),('eleven',11),('negative',-2)",
    ] {
        c.execute(sql, &p).unwrap();
    }
    c.execute("UPDATE docs SET payload.n=n", &p).unwrap();
    for (source, key) in [
        ("(SELECT label,n FROM docs) x", "x.n"),
        ("chosen x", "x.n"),
        ("(SELECT label,payload FROM docs) x", "x.payload.n"),
    ] {
        let prefix = if source == "chosen x" {
            "WITH chosen AS (SELECT label,n FROM docs) "
        } else {
            ""
        };
        for (direction, labels) in [
            ("ASC", vec!["negative", "six", "eleven"]),
            ("DESC", vec!["eleven", "six", "negative"]),
        ] {
            let sql = format!("{prefix}SELECT x.label FROM {source} ORDER BY {key} {direction}");
            let expected = labels
                .into_iter()
                .map(|s| vec![Value::String(s.into())])
                .collect::<Vec<_>>();
            assert_eq!(c.execute(&sql, &p).unwrap().rows, expected);
            assert_eq!(c.profile_select(&sql, &p).unwrap().result.rows, expected);
        }
    }
}

#[test]
fn unprojected_derived_record_keys_sort_numerically() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    for (label, key) in [("six", 6), ("eleven", 11), ("negative", -2)] {
        let p = Parameters::from([
            ("$label".into(), Value::String(label.into())),
            (
                "$key".into(),
                Value::Record(romedb::Record {
                    table: "docs".into(),
                    key: romedb::Key::Integer(key),
                }),
            ),
        ]);
        c.execute("INSERT INTO docs(label,target) VALUES($label,$key)", &p)
            .unwrap();
    }
    for sql in ["SELECT x.label FROM (SELECT label,target FROM docs) x ORDER BY x.target", "WITH chosen AS (SELECT label,target FROM docs) SELECT x.label FROM chosen x ORDER BY x.target"] {
        let expected=vec![vec![Value::String("negative".into())],vec![Value::String("six".into())],vec![Value::String("eleven".into())]];
        assert_eq!(q(&c,sql).rows,expected);
        assert_eq!(c.profile_select(sql,&Parameters::new()).unwrap().result.rows,expected);
    }
}
