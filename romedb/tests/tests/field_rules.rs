use romedb::{Database, Document, Parameters, Value};

fn query(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

#[test]
fn defaults_are_typed_insert_only_and_preserve_explicit_nulls_and_nested_absence() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD stamp ON docs TYPE integer REQUIRED DEFAULT 7 READONLY CHECK(stamp>0)",
        "DEFINE FIELD enabled ON docs TYPE boolean DEFAULT true",
        "DEFINE FIELD note ON docs TYPE string NULLABLE DEFAULT 'fallback'",
        "DEFINE FIELD profile ON docs TYPE object DEFAULT {}",
        "DEFINE FIELD profile.city ON docs TYPE string DEFAULT 'Paris'",
        "DEFINE FIELD optional.flag ON docs TYPE boolean DEFAULT true",
    ] {
        query(&c, sql);
    }
    let typed = Value::Array(vec![
        Value::Integer(i64::MAX),
        Value::Binary(vec![0, 255]),
        Value::vector32(&[1.0, 2.0]).unwrap(),
        Value::Record(romedb::Record {
            table: "refs".into(),
            key: romedb::Key::String("a".into()),
        }),
    ]);
    c.execute(
        "DEFINE FIELD payload ON docs TYPE array DEFAULT $value",
        &Parameters::from([("$value".into(), typed.clone())]),
    )
    .unwrap();
    let result=query(&c,"INSERT INTO docs {id:docs:a,note:null} RETURNING stamp,enabled,note,profile,payload,optional");
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Integer(7),
            Value::Boolean(true),
            Value::Null,
            Value::Object(Document::from([(
                "city".into(),
                Value::String("Paris".into())
            )])),
            typed,
            Value::Null
        ]]
    );
    query(&c, "UPDATE docs:a UNSET enabled");
    assert_eq!(
        query(&c, "SELECT enabled FROM docs").rows,
        vec![vec![Value::Null]]
    );
    query(&c, "UPDATE docs:a CONTENT {stamp:7}");
    assert_eq!(
        query(&c, "SELECT enabled,profile,payload FROM docs").rows,
        vec![vec![Value::Null; 3]]
    );
    query(&c, "INSERT INTO docs(id) VALUES(docs:b)");
    query(&c, "UPSERT docs:c {}");
    query(&c, "INSERT INTO docs(id) SELECT type::record('docs','d')");
    assert_eq!(
        query(&c, "SELECT stamp FROM docs ORDER BY id").rows,
        vec![vec![Value::Integer(7)]; 4]
    );
    let Value::Object(info) = &query(&c, "INFO FOR TABLE docs").rows[0][0] else {
        panic!()
    };
    let Value::Array(fields) = &info["fields"] else {
        panic!()
    };
    assert!(fields.iter().any(|value|matches!(value,Value::Object(field) if field["path"]==Value::Array(vec![Value::String("stamp".into())]) && field["has_default"]==Value::Boolean(true) && field["readonly"]==Value::Boolean(true))));
}

#[test]
fn readonly_is_enforced_across_mutations_replacements_and_public_rust_methods() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD locked ON docs TYPE integer READONLY",
        "DEFINE FIELD absent ON docs TYPE string NULLABLE READONLY",
        "CREATE UNIQUE INDEX docs_name ON docs(name)",
        "INSERT INTO docs {id:docs:a,locked:1,name:'a',counter:0}",
        "INSERT INTO docs {id:docs:b,locked:2,name:'b',counter:0}",
    ] {
        query(&c, sql);
    }
    let before = query(&c, "SELECT * FROM docs ORDER BY id").rows;
    for sql in [
        "UPDATE docs:a {locked:2}",
        "UPDATE docs {locked:2} WHERE id=docs:a",
        "UPDATE docs:a SET locked=2",
        "UPDATE docs SET locked=2 WHERE id=docs:a",
        "UPDATE docs:a UNSET locked",
        "UPDATE docs:a CONTENT {name:'a'}",
        "UPDATE docs:a MERGE {locked:2}",
        "UPDATE docs:a PATCH [{op:'replace',path:'/locked',value:2}]",
        "UPSERT docs:a {locked:2}",
        "INSERT OR REPLACE INTO docs(id,name,locked) VALUES(docs:a,'a',2)",
        "UPDATE OR REPLACE docs SET locked=2 WHERE id=docs:a",
        "UPDATE docs:a SET absent=null",
        "UPDATE docs SET locked=1,counter=counter+1",
    ] {
        let error = c.execute(sql, &Parameters::new()).unwrap_err();
        assert_eq!(error.code(), "FDB_VALIDATION", "{sql}: {error}");
        assert_eq!(
            query(&c, "SELECT * FROM docs ORDER BY id").rows,
            before,
            "{sql}"
        );
    }
    let id = romedb::Record {
        table: "docs".into(),
        key: romedb::Key::String("a".into()),
    };
    assert_eq!(
        c.patch(&id, Document::from([("locked".into(), Value::Integer(9))]))
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    assert_eq!(
        c.upsert(
            "docs",
            Document::from([
                ("id".into(), Value::Record(id)),
                ("locked".into(), Value::Integer(9))
            ])
        )
        .unwrap_err()
        .code(),
        "FDB_VALIDATION"
    );
    query(&c, "UPDATE docs:a SET locked=1,counter=1");
    assert_eq!(
        c.lookup_index("docs", "docs_name", &Value::String("a".into()))
            .unwrap()
            .len(),
        1
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn field_rules_reject_invalid_definitions_existing_data_and_excessive_defaults() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    query(&c, "INSERT INTO docs {id:docs:a,n:1}");
    for sql in [
        "DEFINE FIELD bad ON docs TYPE integer DEFAULT 'x'",
        "DEFINE FIELD bad ON docs TYPE integer DEFAULT null",
        "DEFINE FIELD bad ON docs TYPE integer DEFAULT n",
        "DEFINE FIELD bad ON docs TYPE integer DEFAULT random()",
        "DEFINE FIELD missing ON docs TYPE integer REQUIRED DEFAULT 7",
        "DEFINE FIELD id ON docs TYPE string DEFAULT 'x'",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert_eq!(
        c.execute(
            "DEFINE FIELD large ON docs TYPE string DEFAULT $value",
            &Parameters::from([("$value".into(), Value::String("x".repeat(65536)))])
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    query(&c, "DEFINE FIELD n ON docs TYPE integer DEFAULT 2 READONLY");
    query(&c, "BEGIN");
    query(
        &c,
        "DEFINE FIELD OVERWRITE n ON docs TYPE integer DEFAULT 3",
    );
    query(&c, "UPDATE docs:a SET n=4");
    query(&c, "ROLLBACK");
    assert!(c
        .execute("UPDATE docs:a SET n=4", &Parameters::new())
        .is_err());
    query(&c, "REMOVE FIELD n ON docs");
    query(&c, "UPDATE docs:a SET n=4");
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn field_rules_persist_with_snapshots_import_defaults_and_atomic_result_failures() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("rules.db");
    {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let a = db.connect().unwrap();
        let b = db.connect().unwrap();
        query(&a, "CREATE TABLE docs");
        query(
            &a,
            "DEFINE FIELD stamp ON docs TYPE integer REQUIRED DEFAULT 7 READONLY",
        );
        query(&a, "CREATE INDEX docs_stamp ON docs(stamp)");
        query(&a, "INSERT INTO docs {id:docs:a,name:'old'}");
        query(&a, "BEGIN");
        let old = query(&a, "SELECT * FROM docs").rows;
        query(&b, "UPDATE docs:a SET name='new'");
        assert_eq!(query(&a, "SELECT * FROM docs").rows, old);
        query(&a, "COMMIT");
        query(&a, "BEGIN");
        query(&a, "INSERT INTO docs {id:docs:prior}");
        assert_eq!(
            a.write_with_result_limits(
                "INSERT INTO docs {id:docs:bad} RETURNING *",
                &Parameters::new(),
                romedb::ResultLimits {
                    max_rows: 0,
                    max_payload_bytes: 100
                }
            )
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        let token = romedb::CancellationToken::new();
        token.cancel();
        assert_eq!(
            a.execute_cancellable(
                "INSERT INTO docs {id:docs:cancelled}",
                &Parameters::new(),
                &token
            )
            .unwrap_err()
            .code(),
            "FDB_CANCELLED"
        );
        assert_eq!(
            query(&a, "SELECT count(*) FROM docs").rows,
            vec![vec![Value::Integer(2)]]
        );
        query(&a, "COMMIT");
        let source = Database::open(":memory:").unwrap().connect().unwrap();
        query(&source, "INSERT INTO docs {id:docs:imported}");
        let exported = source
            .export_documents("docs", romedb::TransferFormat::Json)
            .unwrap();
        assert_eq!(
            a.import_documents("docs", &exported, romedb::TransferFormat::Json)
                .unwrap(),
            1
        );
    }
    let c = Database::open(file.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(
        query(&c, "SELECT stamp FROM docs ORDER BY id").rows,
        vec![vec![Value::Integer(7)]; 3]
    );
    assert!(c
        .execute("UPDATE docs SET stamp=8", &Parameters::new())
        .is_err());
    assert_eq!(
        c.lookup_index("docs", "docs_stamp", &Value::Integer(7))
            .unwrap()
            .len(),
        3
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn field_rules_version_metadata_rejects_downgrades_or_corruption_and_stays_monotonic() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("rules-version.db");
    let path = file.to_str().unwrap();
    let c = Database::open(path).unwrap().connect().unwrap();
    query(&c, "CREATE TABLE docs");
    query(&c, "DEFINE FIELD n ON docs TYPE integer DEFAULT 7 READONLY");
    let engine = turso_core::Database::open_file(
        turso_core::Database::io_for_path(path).unwrap(),
        path,
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap();
    let raw = engine.connect().unwrap();
    let version = || {
        let mut values = Vec::new();
        raw.prepare(
            "SELECT json_extract(metadata,'$.version') FROM __romedb_catalog WHERE name='docs'",
        )
        .unwrap()
        .run_with_row_callback(|row| {
            values.extend(row.get_values().cloned());
            Ok(())
        })
        .unwrap();
        assert_eq!(values, vec![turso_core::Value::from_i64(5)]);
    };
    version();
    raw.execute("CREATE TABLE metadata_backup AS SELECT metadata FROM __romedb_catalog")
        .unwrap();
    for expression in [
        "json_set(metadata,'$.version',4)",
        "json_set(metadata,'$.field_policies[0].path[0]','missing')",
        r#"json_set(metadata,'$.field_policies[0].options.default',json('{"type":"String","value":"bad"}'))"#,
        "json_set(metadata,'$.field_policies[0].options.unknown',1)",
        "json_insert(metadata,'$.field_policies[#]',json_extract(metadata,'$.field_policies[0]'))",
    ] {
        raw.execute(format!("UPDATE __romedb_catalog SET metadata={expression}"))
            .unwrap();
        let error = c
            .execute("INFO FOR TABLE docs", &Parameters::new())
            .unwrap_err();
        assert_eq!(error.code(), "FDB_STORAGE", "{expression}: {error}");
        raw.execute("UPDATE __romedb_catalog SET metadata=(SELECT metadata FROM metadata_backup)")
            .unwrap();
    }
    query(&c, "CREATE INDEX docs_n ON docs(n)");
    version();
    query(
        &c,
        "CREATE SEARCH INDEX docs_text ON docs(title) USING FULLTEXT",
    );
    version();
    query(&c, "REMOVE FIELD n ON docs");
    version();
    query(&c, "DROP INDEX docs_text");
    version();
}

#[test]
fn default_values_participate_in_checks_unique_indexes_and_statement_rollback() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD stamp ON docs TYPE integer REQUIRED DEFAULT 7",
        "CREATE UNIQUE INDEX docs_stamp ON docs(stamp)",
        "BEGIN",
        "INSERT INTO docs {id:docs:prior,stamp:1}",
    ] {
        query(&c, sql);
    }
    assert_eq!(
        c.execute(
            "INSERT INTO docs(id) VALUES(docs:a),(docs:b)",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_CONSTRAINT"
    );
    assert_eq!(
        query(&c, "SELECT stamp FROM docs").rows,
        vec![vec![Value::Integer(1)]]
    );
    assert!(c
        .lookup_index("docs", "docs_stamp", &Value::Integer(7))
        .unwrap()
        .is_empty());
    query(&c, "COMMIT");
    query(&c, "CREATE TABLE bad");
    query(
        &c,
        "DEFINE FIELD n ON bad TYPE integer DEFAULT 0 CHECK(n>0)",
    );
    assert_eq!(
        c.execute("INSERT INTO bad {}", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    assert_eq!(
        query(&c, "SELECT count(*) FROM bad").rows,
        vec![vec![Value::Integer(0)]]
    );
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn defaults_and_readonly_keep_all_managed_indexes_consistent_in_strict_collections() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("rule-indexes.db");
    let probes=[
        "SELECT id FROM docs WHERE uid=7",
        "SELECT id FROM docs WHERE author=writers:one ORDER BY id",
        "SELECT id,score FROM search::text('docs_text','default',10) ORDER BY id",
        "SELECT id,distance FROM search::vector('docs_vec',vector32('[1,0]'),2) ORDER BY distance,id",
        "SELECT id FROM search::near('docs_geo',geo::point(0,0),100) ORDER BY id",
    ];
    {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        query(&c, "CREATE TABLE docs");
        let location = query(&c, "SELECT geo::point(0,0)").rows.remove(0).remove(0);
        for (name, kind, value, flexible) in [
            ("uid", romedb::FieldType::Integer, Value::Integer(7), false),
            (
                "counter",
                romedb::FieldType::Integer,
                Value::Integer(0),
                false,
            ),
            (
                "title",
                romedb::FieldType::String,
                Value::String("default text".into()),
                false,
            ),
            (
                "author",
                romedb::FieldType::Record("writers".into()),
                Value::Record(romedb::Record {
                    table: "writers".into(),
                    key: romedb::Key::String("one".into()),
                }),
                false,
            ),
            (
                "v",
                romedb::FieldType::Vector(2),
                Value::vector32(&[1.0, 0.0]).unwrap(),
                false,
            ),
            ("location", romedb::FieldType::Object, location, true),
        ] {
            let mut options = romedb::FieldOptions::default();
            options.default = Some(value);
            options.readonly = name != "counter";
            options.flexible = flexible;
            c.define_field_with_options(
                "docs",
                romedb::Field {
                    path: vec![name.into()],
                    kind,
                    required: true,
                    nullable: false,
                    check: None,
                },
                options,
                false,
            )
            .unwrap();
        }
        for sql in [
            "DEFINE SCHEMA ON docs STRICT",
            "CREATE UNIQUE INDEX docs_uid ON docs(uid)",
            "CREATE INDEX docs_author ON docs(author)",
            "CREATE SEARCH INDEX docs_text ON docs(title) USING FULLTEXT",
            "CREATE SEARCH INDEX docs_vec ON docs(v) USING VECTOR WITH (metric='l2',dimensions=2)",
            "CREATE SEARCH INDEX docs_geo ON docs(location) USING SPATIAL",
            "INSERT INTO docs {id:docs:a}",
            "INSERT INTO docs {id:docs:b,uid:8}",
        ] {
            query(&c, sql);
        }
        let before = query(&c, "SELECT * FROM docs ORDER BY id").rows;
        let expected = probes.map(|sql| query(&c, sql).rows);
        assert!(expected.iter().all(|rows| !rows.is_empty()));
        let reader = db.connect().unwrap();
        query(&reader, "BEGIN");
        assert_eq!(
            query(&reader, "SELECT * FROM docs ORDER BY id").rows,
            before
        );
        query(&c, "BEGIN");
        query(&c, "CREATE TABLE prior(n INTEGER)");
        query(&c, "INSERT INTO prior VALUES(9)");
        for sql in [
            "UPDATE docs SET counter=counter+1,title=CASE WHEN uid=7 THEN title ELSE 'changed' END",
            "UPDATE docs:a MERGE {author:writers:other}",
            "UPDATE docs:a MERGE {v:vector32('[0,1]')}",
            "UPDATE docs:a MERGE {location:geo::point(1,1)}",
            "INSERT OR REPLACE INTO docs(id,title) VALUES(docs:a,'changed')",
        ] {
            assert_eq!(
                c.execute(sql, &Parameters::new()).expect_err(sql).code(),
                "FDB_VALIDATION"
            );
            assert_eq!(query(&c, "SELECT * FROM docs ORDER BY id").rows, before);
            for (probe, expected) in probes.iter().zip(&expected) {
                assert_eq!(&query(&c, probe).rows, expected);
            }
            c.check_collection_integrity("docs", Default::default())
                .unwrap();
        }
        query(&c, "UPDATE docs SET counter=counter+1");
        query(&c, "COMMIT");
        assert_eq!(
            query(&reader, "SELECT * FROM docs ORDER BY id").rows,
            before
        );
        query(&reader, "COMMIT");
        assert_eq!(
            query(&c, "SELECT n FROM prior").rows,
            vec![vec![Value::Integer(9)]]
        );
    }
    let c = Database::open(file.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(
        query(&c, "SELECT counter FROM docs ORDER BY id").rows,
        vec![vec![Value::Integer(1)]; 2]
    );
    for probe in probes {
        assert!(!query(&c, probe).rows.is_empty());
    }
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}
