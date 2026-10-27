use romedb::{Database, Document, Parameters, Value};

fn query(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

#[test]
fn strict_schema_rejects_unknown_fields_and_preserves_flexible_typed_children() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD name ON docs TYPE string REQUIRED",
        "DEFINE FIELD profile.city ON docs TYPE string",
        "DEFINE FIELD settings ON docs TYPE object FLEXIBLE",
        "DEFINE FIELD settings.count ON docs TYPE integer",
        "DEFINE FIELD items ON docs TYPE array<object> FLEXIBLE",
        "DEFINE FIELD empty ON docs TYPE object",
        "DEFINE FIELD n ON docs TYPE integer DEFAULT 7 READONLY",
        "DEFINE SCHEMA ON docs STRICT",
        "INSERT INTO docs {id:docs:a,name:'a',profile:{city:'Paris'},settings:{count:1,extra:{deep:true}},items:[{anything:2}],empty:{}}",
    ] { query(&c, sql); }
    let before = query(&c, "SELECT * FROM docs").rows;
    for sql in [
        "UPDATE docs:a SET surprise=1",
        "UPDATE docs:a {profile:{city:'Paris',extra:1}}",
        "UPDATE docs:a MERGE {empty:{extra:1}}",
        "UPDATE docs:a {settings:{count:'bad',extra:true}}",
        "UPDATE docs:a {items:[{ok:1},4]}",
        "UPDATE docs:a PATCH [{op:'add',path:'/other',value:1}]",
        "UPDATE docs:a CONTENT {name:'a',n:7,extra:1}",
        "UPSERT docs:a {extra:1}",
        "INSERT OR REPLACE INTO docs(id,name,n,extra) VALUES(docs:a,'a',7,1)",
    ] {
        let error = c.execute(sql, &Parameters::new()).unwrap_err();
        assert_eq!(error.code(), "FDB_VALIDATION", "{sql}: {error}");
        assert_eq!(query(&c, "SELECT * FROM docs").rows, before, "{sql}");
    }
    query(
        &c,
        "UPDATE docs:a {settings:{count:2,new_field:[{arbitrary:1}]}}",
    );
    query(&c, "DEFINE SCHEMA ON docs FLEXIBLE");
    query(&c, "UPDATE docs:a SET extra=1");
    let error = c
        .execute("DEFINE SCHEMA ON docs STRICT", &Parameters::new())
        .unwrap_err();
    assert_eq!(error.code(), "FDB_VALIDATION");
    query(&c, "UPDATE docs:a SET another=2");
    let Value::Object(info) = &query(&c, "INFO FOR TABLE docs").rows[0][0] else {
        panic!()
    };
    assert_eq!(info["strict"], Value::Boolean(false));
}

#[test]
fn schema_changes_validate_existing_data_atomically_and_removal_cannot_orphan_fields() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    for sql in [
        "INSERT INTO docs {id:docs:a,n:1,profile:{city:'Paris'}}",
        "DEFINE FIELD n ON docs TYPE integer",
    ] {
        query(&c, sql);
    }
    assert!(c
        .execute("DEFINE SCHEMA ON docs STRICT", &Parameters::new())
        .is_err());
    query(&c, "DEFINE FIELD profile.city ON docs TYPE string");
    query(&c, "DEFINE SCHEMA ON docs STRICT");
    for sql in [
        "REMOVE FIELD n ON docs",
        "REMOVE FIELD profile.city ON docs",
        "DEFINE FIELD profile ON docs TYPE string",
    ] {
        assert_eq!(
            c.execute(sql, &Parameters::new()).unwrap_err().code(),
            "FDB_VALIDATION",
            "{sql}"
        );
    }
    query(&c, "UPDATE docs:a SET n=2");
    query(&c, "UPDATE docs:a UNSET n");
    query(&c, "REMOVE FIELD n ON docs");
    assert!(c
        .execute("UPDATE docs:a SET n=3", &Parameters::new())
        .is_err());
    query(&c, "CREATE TABLE impossible");
    query(&c, "DEFINE FIELD parent ON impossible TYPE array");
    query(&c, "DEFINE FIELD parent.child ON impossible TYPE integer");
    assert!(c
        .execute("DEFINE SCHEMA ON impossible STRICT", &Parameters::new())
        .is_err());
    query(&c, "CREATE TABLE arrays");
    query(&c, "DEFINE FIELD items ON arrays TYPE array<object>");
    query(&c, "DEFINE SCHEMA ON arrays STRICT");
    query(&c, "INSERT INTO arrays {items:[{}]}");
    assert!(c
        .execute("INSERT INTO arrays {items:[{n:1}]}", &Parameters::new())
        .is_err());
    query(
        &c,
        "DEFINE FIELD OVERWRITE items ON arrays TYPE array<object> FLEXIBLE",
    );
    query(&c, "INSERT INTO arrays {items:[{n:1}]}");
    assert!(c
        .execute(
            "DEFINE FIELD OVERWRITE items ON arrays TYPE array<object>",
            &Parameters::new()
        )
        .is_err());
    query(&c, "INSERT INTO arrays {items:[{another:2}]}");
}

#[test]
fn typed_arrays_validate_exact_types_nullability_defaults_and_existing_records() {
    let c = Database::open(":memory:").unwrap().connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD ints ON docs TYPE array<integer?> NULLABLE",
        "DEFINE FIELD nums ON docs TYPE array<number>",
        "DEFINE FIELD refs ON docs TYPE array<record<writers>>",
        "DEFINE FIELD vectors ON docs TYPE array<vector<2>>",
        "DEFINE FIELD booleans ON docs TYPE array<boolean>",
        "DEFINE FIELD strings ON docs TYPE array<string>",
        "DEFINE FIELD nested ON docs TYPE array<array>",
        "DEFINE FIELD anything ON docs TYPE array<any>",
        "INSERT INTO docs {id:docs:a,ints:[9223372036854775807,null],nums:[1,1.5],refs:[writers:a],booleans:[true,false],strings:['s'],nested:[[1,'a']],anything:[null,1,'a']}",
    ] {query(&c,sql);}
    c.execute(
        "UPDATE docs:a SET vectors=$v",
        &Parameters::from([(
            "$v".into(),
            Value::Array(vec![Value::vector32(&[1.0, 2.0]).unwrap()]),
        )]),
    )
    .unwrap();
    let before = query(&c, "SELECT * FROM docs").rows;
    for sql in [
        "UPDATE docs:a {ints:[1.5]}",
        "UPDATE docs:a {nums:[null]}",
        "UPDATE docs:a {refs:[other:a]}",
        "UPDATE docs:a {refs:['writers:a']}",
        "UPDATE docs:a {booleans:[1]}",
        "UPDATE docs:a {strings:[true]}",
        "UPDATE docs:a {nested:[1]}",
        "UPDATE docs:a {vectors:[[1,2]]}",
        "DEFINE FIELD OVERWRITE ints ON docs TYPE array<integer>",
        "DEFINE FIELD invalid ON docs TYPE integer FLEXIBLE",
    ] {
        assert_eq!(
            c.execute(sql, &Parameters::new()).expect_err(sql).code(),
            "FDB_VALIDATION",
            "{sql}"
        );
        assert_eq!(query(&c, "SELECT * FROM docs").rows, before);
    }
    for vector in [Value::vector32(&[1.0]).unwrap(), Value::Null] {
        assert_eq!(
            c.execute(
                "UPDATE docs:a SET vectors=$v",
                &Parameters::from([("$v".into(), Value::Array(vec![vector]))])
            )
            .unwrap_err()
            .code(),
            "FDB_VALIDATION"
        );
    }
    query(&c, "UPDATE docs:a SET ints=null");
    for (value, valid) in [
        (Value::Array(vec![Value::Integer(7)]), true),
        (Value::Array(vec![Value::String("7".into())]), false),
    ] {
        let result = c.execute(
            "DEFINE FIELD defaults ON docs TYPE array<integer> DEFAULT $v",
            &Parameters::from([("$v".into(), value)]),
        );
        if valid {
            result.unwrap();
            query(&c, "REMOVE FIELD defaults ON docs");
        } else {
            assert_eq!(result.unwrap_err().code(), "FDB_VALIDATION");
        }
    }
    let Value::Object(info) = &query(&c, "INFO FOR TABLE docs").rows[0][0] else {
        panic!()
    };
    let Value::Array(fields) = &info["fields"] else {
        panic!()
    };
    assert!(fields.iter().any(|f|matches!(f,Value::Object(f) if f["element_type"]==Value::String("integer".into()) && f["element_nullable"]==Value::Boolean(true))));
}

#[test]
fn schema_enforcement_survives_rollback_import_reopen_and_indexed_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("schema.db");
    {
        let db = Database::open(file.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE docs",
            "DEFINE FIELD n ON docs TYPE integer REQUIRED DEFAULT 7",
            "DEFINE FIELD values ON docs TYPE array<integer>",
            "DEFINE SCHEMA ON docs STRICT",
            "CREATE UNIQUE INDEX docs_n ON docs(n)",
            "BEGIN",
            "INSERT INTO docs {id:docs:prior,n:1}",
        ] {
            query(&c, sql);
        }
        assert!(c
            .execute(
                "INSERT INTO docs(id,n,values) VALUES(docs:a,2,[1]),(docs:b,3,['bad'])",
                &Parameters::new()
            )
            .is_err());
        assert_eq!(
            query(&c, "SELECT n FROM docs").rows,
            vec![vec![Value::Integer(1)]]
        );
        assert!(c
            .lookup_index("docs", "docs_n", &Value::Integer(2))
            .unwrap()
            .is_empty());
        let source = Database::open(":memory:").unwrap().connect().unwrap();
        query(&source, "INSERT INTO docs {id:docs:imported,n:2,extra:1}");
        let exported = source
            .export_documents("docs", romedb::TransferFormat::Json)
            .unwrap();
        assert!(c
            .import_documents("docs", &exported, romedb::TransferFormat::Json)
            .is_err());
        query(&c, "INSERT INTO docs {id:docs:defaulted}");
        query(&c, "COMMIT");
    }
    let c = Database::open(file.to_str().unwrap())
        .unwrap()
        .connect()
        .unwrap();
    assert_eq!(
        query(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(7)]]
    );
    assert!(c
        .insert(
            "docs",
            Document::from([("extra".into(), Value::Integer(1))])
        )
        .is_err());
    assert!(c
        .execute("UPDATE docs {values:['bad']}", &Parameters::new())
        .is_err());
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}

#[test]
fn schema_limits_cancellation_and_metadata_fail_closed_without_losing_caller_work() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("schema-limits.db");
    let path = file.to_str().unwrap();
    let c = Database::open(path).unwrap().connect().unwrap();
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD n ON docs TYPE integer",
        "DEFINE FIELD a ON docs TYPE array<integer>",
        "INSERT INTO docs {id:docs:a,n:1}",
        "INSERT INTO docs {id:docs:b,n:2}",
        "BEGIN",
        "INSERT INTO docs {id:docs:prior,n:3}",
    ] {
        query(&c, sql);
    }
    let m = c
        .schema_metered(
            "DEFINE SCHEMA ON docs STRICT",
            &Parameters::new(),
            romedb::ResultLimits {
                max_rows: 0,
                max_payload_bytes: 65536,
            },
            romedb::SchemaWorkLimits {
                max_rows_read: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(m.outcome.unwrap_err().code(), "FDB_CANCELLED");
    assert_eq!(m.work.row_mutations, 0);
    assert!(m.work.read_budget_exhausted);
    assert_eq!(
        query(&c, "SELECT count(*) FROM docs").rows,
        vec![vec![Value::Integer(3)]]
    );
    let token = romedb::CancellationToken::new();
    token.cancel();
    assert_eq!(
        c.execute_cancellable("DEFINE SCHEMA ON docs STRICT", &Parameters::new(), &token)
            .unwrap_err()
            .code(),
        "FDB_CANCELLED"
    );
    query(&c, "UPDATE docs:a {extra:1}");
    query(&c, "UPDATE docs:a UNSET extra");
    query(&c, "DEFINE SCHEMA ON docs STRICT");
    query(&c, "COMMIT");
    assert_eq!(
        c.execute(
            "INSERT INTO docs {a:$a}",
            &Parameters::from([(
                "$a".into(),
                Value::Array(vec![Value::Integer(1); 1_000_001])
            )])
        )
        .unwrap_err()
        .code(),
        "FDB_LIMIT"
    );
    let engine = turso_core::Database::open_file(
        turso_core::Database::io_for_path(path).unwrap(),
        path,
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap();
    let raw = engine.connect().unwrap();
    raw.execute("CREATE TABLE metadata_backup AS SELECT metadata FROM __romedb_catalog")
        .unwrap();
    for expression in [
        "json_set(metadata,'$.version',4)",
        "json_set(metadata,'$.strict','yes')",
        "json_set(metadata,'$.field_policies[0].options.element_nullable','yes')",
        r#"json_set(metadata,'$.field_policies[0].options.element_type',json('{"Record":"__romedb_invalid"}'))"#,
        r#"json_set(metadata,'$.field_policies[0].options.element_type',json('{"Vector":0}'))"#,
    ] {
        raw.execute(format!("UPDATE __romedb_catalog SET metadata={expression}"))
            .unwrap();
        assert_eq!(
            c.execute("INFO FOR TABLE docs", &Parameters::new())
                .unwrap_err()
                .code(),
            "FDB_STORAGE",
            "{expression}"
        );
        raw.execute("UPDATE __romedb_catalog SET metadata=(SELECT metadata FROM metadata_backup)")
            .unwrap();
    }
    query(&c, "UPDATE docs:a {a:[1,2]}");
    c.check_collection_integrity("docs", Default::default())
        .unwrap();
}
