use romedb::{Database, Document, Key, Parameters, Record, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
fn setup() -> (Database, romedb::Connection) {
    let db = Database::open(":memory:").expect("open");
    let c = db.connect().expect("connect");
    q(&c, "CREATE TABLE users");
    q(
        &c,
        "DEFINE FIELD name ON users TYPE string REQUIRED CHECK (length(trim(name)) > 0)",
    );
    q(&c, "CREATE UNIQUE INDEX users_name ON users (name)");
    (db, c)
}
#[test]
fn every_write_path_checks_evaluated_values_and_preserves_indexes() {
    let (_db, c) = setup();
    q(&c, "INSERT INTO users {id:users:u1,name:'Alice'}");
    for sql in [
        "INSERT INTO users {id:users:bad,name:''}",
        "INSERT INTO users (id,name) VALUES (users:bad,'')",
        "UPDATE users SET name=lower('')",
        "UPDATE users:u1 {name:' '}",
        "UPSERT users:u1 {name:''}",
        "UPSERT users:bad {name:''}",
    ] {
        let err = c.execute(sql, &Parameters::new()).expect_err(sql);
        assert_eq!(err.code(), "FDB_VALIDATION");
    }
    let params = Parameters::from([(
        "$doc".into(),
        Value::Object(Document::from([
            (
                "id".into(),
                Value::Record(Record {
                    table: "users".into(),
                    key: Key::String("bad".into()),
                }),
            ),
            ("name".into(), Value::String("".into())),
        ])),
    )]);
    assert!(c
        .execute("INSERT INTO users DOCUMENT $doc", &params)
        .is_err());
    assert_eq!(
        q(&c, "SELECT name FROM users").rows,
        vec![vec![Value::String("Alice".into())]]
    );
    assert_eq!(
        c.lookup_index("users", "users_name", &Value::String("Alice".into()))
            .expect("old index")
            .len(),
        1
    );
    q(&c, "UPDATE users SET name=upper(name)");
    assert_eq!(
        c.lookup_index("users", "users_name", &Value::String("ALICE".into()))
            .expect("new index")
            .len(),
        1
    );
}
#[test]
fn multirow_failure_rolls_back_and_checks_use_final_candidate() {
    let (_db, c) = setup();
    q(
        &c,
        "DEFINE FIELD score ON users TYPE integer CHECK (score >= 0 AND score <= ceiling)",
    );
    q(
        &c,
        "INSERT INTO users {id:users:u1,name:'Alice',score:3,ceiling:5}",
    );
    q(
        &c,
        "INSERT INTO users {id:users:u2,name:'Bob',score:1,ceiling:5}",
    );
    q(&c, "BEGIN");
    assert!(c
        .execute("UPDATE users SET score=score-2", &Parameters::new())
        .is_err());
    assert_eq!(
        q(&c, "SELECT score FROM users ORDER BY name").rows,
        vec![vec![Value::Integer(3)], vec![Value::Integer(1)]]
    );
    assert!(c
        .execute("UPDATE users:u1 SET ceiling=2", &Parameters::new())
        .is_err());
    q(&c, "UPDATE users:u1 SET score=6, ceiling=7");
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT score FROM users WHERE id=users:u1").rows,
        vec![vec![Value::Integer(3)]]
    );
    assert!(c
        .execute(
            "INSERT INTO users {id:users:u3,name:'Carol',score:1}",
            &Parameters::new()
        )
        .is_err());
}
#[test]
fn definitions_validate_existing_data_and_nullable_checks_skip() {
    let (_db, c) = setup();
    q(&c, "INSERT INTO users {id:users:u1,name:'Alice'}");
    assert!(c
        .execute(
            "DEFINE FIELD OVERWRITE name ON users TYPE string REQUIRED CHECK (length(name) >= 10)",
            &Parameters::new()
        )
        .is_err());
    q(&c, "UPDATE users:u1 {name:'X'}");
    q(
        &c,
        "DEFINE FIELD note ON users TYPE string NULLABLE CHECK (length(note) >= 3)",
    );
    q(
        &c,
        "DEFINE FIELD profile.city ON users TYPE string CHECK (length(profile.city) >= 3)",
    );
    q(&c, "UPDATE users:u1 SET note=NULL");
    assert!(c
        .execute("UPDATE users:u1 SET note='x'", &Parameters::new())
        .is_err());
    q(&c, "UPDATE users:u1 SET profile.city='Berlin'");
    assert!(c
        .execute("UPDATE users:u1 SET profile.city='x'", &Parameters::new())
        .is_err());
    q(&c, "UPDATE users:u1 UNSET profile.city, note");
    let info = q(&c, "INFO FOR TABLE users");
    assert!(format!("{info:?}").contains("length(note) >= 3"));
    q(&c, "REMOVE FIELD name ON users");
    q(&c, "UPDATE users:u1 SET name=''");
}
#[test]
fn checks_reject_reads_parameters_and_nondeterminism_before_publication() {
    let (_db, c) = setup();
    for expr in [
        "random()>0",
        "uuid7_str() IS NOT NULL",
        "datetime('now') IS NOT NULL",
        "CURRENT_TIMESTAMP IS NOT NULL",
        "length(name)>$limit",
        "EXISTS(SELECT 1)",
        "name IN (SELECT name FROM users)",
        "sum(1)>0",
        "min(1)>0",
        "CASE WHEN 0 THEN random() ELSE 1 END",
        "length(name,1)>0",
        "unknown_function(name)",
        "CAST(name AS custom_type) IS NOT NULL",
        "CAST(name AS TEXT($size)) IS NOT NULL",
        "name LIKE 'A%'",
        "name COLLATE custom_collation = 'x'",
    ] {
        let sql = format!("DEFINE FIELD unsafe ON users TYPE string CHECK ({expr})");
        assert!(c.execute(&sql, &Parameters::new()).is_err(), "{sql}");
    }
    q(
        &c,
        "DEFINE FIELD unsafe ON users TYPE string CHECK (length(unsafe)>0)",
    );
    q(
        &c,
        "INSERT INTO users {id:users:u1,name:'Alice',unsafe:'valid'}",
    );
}
#[test]
fn check_metadata_and_enforcement_survive_reopen() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("checks.db");
    let path = path.to_str().expect("path");
    {
        let db = Database::open(path).expect("open");
        let c = db.connect().expect("connect");
        q(&c, "CREATE TABLE users");
        q(
            &c,
            "DEFINE FIELD name ON users TYPE string REQUIRED CHECK (length(name)>0)",
        );
        q(&c, "INSERT INTO users {id:users:u1,name:'Alice'}");
    }
    let db = Database::open(path).expect("reopen");
    let c = db.connect().expect("connect");
    assert!(c
        .execute("UPSERT users:u1 {name:''}", &Parameters::new())
        .is_err());
    assert_eq!(
        q(&c, "SELECT name FROM users").rows,
        vec![vec![Value::String("Alice".into())]]
    );
}

#[test]
fn built_in_casts_collations_and_fixed_matching_are_supported() {
    let (_db, c) = setup();
    q(&c,"DEFINE FIELD code ON users TYPE string CHECK (code GLOB '[A-Z]*' AND CAST(length(code) AS INTEGER) > 1 AND code COLLATE NOCASE != 'bad')");
    q(&c, "INSERT INTO users {id:users:u1,name:'Alice',code:'OK'}");
    assert!(c
        .execute("UPDATE users:u1 SET code='bad'", &Parameters::new())
        .is_err());
    assert!(c
        .execute("UPDATE users:u1 SET code='BAD'", &Parameters::new())
        .is_err());
    q(
        &c,
        "DEFINE FIELD quoted ON users TYPE string CHECK (length(quoted) > length(')'))",
    );
    q(&c, "UPDATE users:u1 SET quoted='valid'");
}

#[test]
fn adding_checks_upgrades_legacy_metadata_atomically() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("upgrade.db");
    let path = path.to_str().expect("path");
    let db = Database::open(path).expect("open");
    let c = db.connect().expect("connect");
    q(&c, "CREATE TABLE users");
    let engine = turso_core::Database::open_file(
        turso_core::Database::io_for_path(path).expect("io"),
        path,
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .expect("engine");
    let raw = engine.connect().expect("raw connection");
    raw.execute("UPDATE __romedb_catalog SET metadata=json_remove(metadata,'$.version')")
        .expect("legacy fixture");
    q(&c, "INSERT INTO users {id:users:u1,name:'Alice'}");
    q(&c, "BEGIN");
    q(
        &c,
        "DEFINE FIELD name ON users TYPE string CHECK(length(name)>0)",
    );
    q(&c, "ROLLBACK");
    let mut stmt = raw
        .prepare("SELECT json_extract(metadata,'$.version') FROM __romedb_catalog")
        .expect("version query");
    assert!(matches!(
        stmt.run_collect_rows().expect("rows")[0][0],
        turso_core::Value::Null
    ));
    drop(stmt);
    q(
        &c,
        "DEFINE FIELD name ON users TYPE string CHECK(length(name)>0)",
    );
    let mut stmt = raw
        .prepare("SELECT json_extract(metadata,'$.version') FROM __romedb_catalog")
        .expect("version query");
    assert!(matches!(
        stmt.run_collect_rows().expect("rows")[0][0],
        turso_core::Value::Numeric(turso_core::Numeric::Integer(2))
    ));
    drop(stmt);
    raw.execute("UPDATE __romedb_catalog SET metadata=json_set(metadata,'$.version',1)")
        .expect("invalid version fixture");
    assert!(c
        .execute("SELECT * FROM users", &Parameters::new())
        .expect_err("cannot ignore CHECK")
        .to_string()
        .contains("requires catalog version 2"));
}

#[test]
fn candidate_record_range_checks_validate_and_reopen_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("record-check.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(
            &c,
            "INSERT INTO docs {id:docs:saved,ref:docs:2,low:docs:1,high:docs:10}",
        );
        // Adding a CHECK scans existing records using numeric key ordering.
        q(&c,"DEFINE FIELD ref ON docs TYPE record<docs> CHECK ((ref) >= low AND ref < high AND ref BETWEEN low AND high)");
        q(&c, "CREATE UNIQUE INDEX docs_ref ON docs(ref)");
        q(&c, "BEGIN");
        for sql in [
            "UPDATE docs:saved {ref:docs:20}",
            "UPDATE docs SET ref=docs:20",
            "UPSERT docs:saved {ref:docs:20}",
            "INSERT INTO docs (id,ref,low,high) VALUES (docs:bad,docs:20,docs:1,docs:10)",
            "INSERT INTO docs {id:docs:bad,ref:docs:20,low:docs:1,high:docs:10}",
        ] {
            assert_eq!(
                c.execute(sql, &Parameters::new()).unwrap_err().code(),
                "FDB_VALIDATION",
                "{sql}"
            );
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        }
        assert_eq!(
            q(&c, "SELECT record::id(ref) AS key FROM docs").rows,
            vec![vec![Value::Integer(2)]]
        );
        q(&c, "UPDATE docs:saved {ref:docs:3}");
        q(&c, "ROLLBACK");
        let key = Value::Record(Record {
            table: "docs".into(),
            key: Key::Integer(2),
        });
        assert_eq!(c.lookup_index("docs", "docs_ref", &key).unwrap().len(), 1);
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        c.execute("UPDATE docs:saved {ref:docs:20}", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    q(&c, "UPDATE docs:saved {ref:docs:3}");
    let old = Value::Record(Record {
        table: "docs".into(),
        key: Key::Integer(2),
    });
    let new = Value::Record(Record {
        table: "docs".into(),
        key: Key::Integer(3),
    });
    assert!(c.lookup_index("docs", "docs_ref", &old).unwrap().is_empty());
    assert_eq!(c.lookup_index("docs", "docs_ref", &new).unwrap().len(), 1);
}

#[test]
fn deep_candidate_paths_preserve_quoted_segments_and_check_atomicity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deep-check.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(&c,"INSERT INTO docs {id:docs:saved,profile:{address:{details:{city:'Paris'},\"details.city\":{label:'quoted'}}}}");
        q(&c,"DEFINE FIELD profile.address.details.city ON docs TYPE string CHECK(length(profile.address.details.city)>0 AND profile.address.\"details.city\".label='quoted' AND length('a.b.c.d')=7)");
        q(
            &c,
            "CREATE INDEX cities ON docs(profile.address.details.city)",
        );
        q(&c, "BEGIN");
        assert_eq!(
            c.execute(
                "UPDATE docs SET profile.address.details.city=''",
                &Parameters::new()
            )
            .unwrap_err()
            .code(),
            "FDB_VALIDATION"
        );
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(
            c.lookup_index("docs", "cities", &Value::String("Paris".into()))
                .unwrap()
                .len(),
            1
        );
        q(&c, "ROLLBACK");
        for marker in [
            "__romedb_path(a,b,c,d)",
            "\"__romedb_path\"(a,b,c,d)",
            "'__romedb_path'(a,b,c,d)",
        ] {
            assert!(c
                .execute(
                    &format!("DEFINE FIELD forbidden ON docs TYPE string CHECK({marker})"),
                    &Parameters::new()
                )
                .is_err());
        }
        // An overlong candidate path cannot be interpreted as a helper call.
        let long = vec!["a"; 65].join(".");
        assert!(c
            .execute(
                &format!("DEFINE FIELD forbidden ON docs TYPE string CHECK({long})"),
                &Parameters::new()
            )
            .is_err());
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(c.execute("UPDATE docs:saved {profile:{address:{details:{city:''},\"details.city\":{label:'quoted'}}}}",&Parameters::new()).unwrap_err().code(),"FDB_VALIDATION");
    q(&c, "UPDATE docs SET profile.address.details.city='Osaka'");
    assert!(c
        .lookup_index("docs", "cities", &Value::String("Paris".into()))
        .unwrap()
        .is_empty());
    assert_eq!(
        c.lookup_index("docs", "cities", &Value::String("Osaka".into()))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn binary_check_arguments_use_candidate_payloads_and_persist_validation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("binary-check.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(
            &c,
            "INSERT INTO docs (id,valid,payload) VALUES (docs:saved,1,X'3132')",
        );
        q(&c,"DEFINE FIELD valid ON docs TYPE integer CHECK(length(payload)=2 AND length(coalesce(payload,X''))=2 AND length((payload COLLATE BINARY))=2 AND length(CASE WHEN valid=1 THEN payload ELSE X'' END)=2 AND CAST(payload AS INTEGER)=12 AND substr(payload,1,1)=X'31')");
        q(&c, "CREATE INDEX docs_payload ON docs(payload)");
        q(&c, "BEGIN");
        for sql in [
            "INSERT INTO docs (id,valid,payload) VALUES (docs:bad,1,X'31')",
            "UPDATE docs SET payload=X'313233'",
            "UPDATE docs:saved {payload: null}",
            "UPSERT docs:saved {payload: 'wrong'}",
        ] {
            assert_eq!(
                c.execute(sql, &Parameters::new()).unwrap_err().code(),
                "FDB_VALIDATION",
                "{sql}"
            );
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        }
        assert_eq!(
            c.lookup_index("docs", "docs_payload", &Value::Binary(vec![0x31, 0x32]))
                .unwrap()
                .len(),
            1
        );
        q(&c, "ROLLBACK");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        c.execute("UPDATE docs SET payload=X'33'", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    q(&c, "UPDATE docs SET payload=X'3132'");
    assert_eq!(
        q(
            &c,
            "SELECT length(payload),CAST(payload AS INTEGER) FROM docs"
        )
        .rows,
        vec![vec![Value::Integer(2), Value::Integer(12)]]
    );
}

#[test]
fn binary_check_operators_validate_payloads_atomically_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("binary-operators.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(
            &c,
            "INSERT INTO docs (id,valid,v2,v3,v4,payload) VALUES (docs:saved,1,1,1,1,X'3132')",
        );
        for (field, check) in [
            (
                "valid",
                "payload+1=13 AND payload-1=11 AND payload*2=24 AND payload/2=6 AND payload%5=2",
            ),
            (
                "v2",
                "(payload&3)=0 AND (payload|1)=13 AND (payload<<1)=24 AND (payload>>1)=6",
            ),
            ("v3", "payload||'!'='12!' AND -payload=-12 AND ~payload=-13"),
            (
                "v4",
                "(payload AND 1)=1 AND (payload OR 0)=1 AND (NOT payload)=0 AND length(+payload)=2",
            ),
        ] {
            q(
                &c,
                &format!("DEFINE FIELD {field} ON docs TYPE integer CHECK({check})"),
            );
        }
        q(&c, "CREATE INDEX docs_payload ON docs(payload)");
        q(&c, "BEGIN");
        for sql in [
            "INSERT INTO docs (id,valid,payload) VALUES (docs:bad,1,X'3133')",
            "UPDATE docs SET payload=X'30'",
            "UPDATE docs:saved {payload: null}",
            "UPSERT docs:saved {payload: 'wrong'}",
        ] {
            assert_eq!(
                c.execute(sql, &Parameters::new()).unwrap_err().code(),
                "FDB_VALIDATION",
                "{sql}"
            );
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        }
        assert_eq!(
            c.lookup_index("docs", "docs_payload", &Value::Binary(b"12".to_vec()))
                .unwrap()
                .len(),
            1
        );
        q(&c, "ROLLBACK");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        c.execute("UPDATE docs SET payload=X'3133'", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    q(&c, "UPDATE docs SET payload=X'3132'");
    assert_eq!(
        c.lookup_index("docs", "docs_payload", &Value::Binary(b"12".to_vec()))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn long_check_boolean_chains_prepare_and_validate() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(
        &c,
        "INSERT INTO docs (id,valid,any,payload) VALUES (docs:saved,1,1,X'3132')",
    );
    q(&c, "DEFINE FIELD valid ON docs TYPE integer CHECK(payload+1=13 AND payload-1=11 AND payload*2=24 AND payload/2=6 AND payload%5=2 AND (payload&3)=0 AND (payload|1)=13 AND (payload<<1)=24 AND (payload>>1)=6 AND payload||'!'='12!' AND -payload=-12 AND ~payload=-13 AND (payload AND 1)=1 AND (payload OR 0)=1 AND (NOT payload)=0 AND length(+payload)=2)");
    let mut terms = (20..32)
        .map(|value| format!("payload+0={value}"))
        .collect::<Vec<_>>();
    terms.extend(["NULL".into(), "payload+0=12".into()]);
    q(
        &c,
        &format!(
            "DEFINE FIELD any ON docs TYPE integer CHECK({})",
            terms.join(" OR ")
        ),
    );
    q(&c, "CREATE INDEX docs_payload ON docs(payload)");
    q(&c, "BEGIN");
    assert_eq!(
        c.execute("UPDATE docs SET payload=X'3133'", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(
        c.lookup_index("docs", "docs_payload", &Value::Binary(b"12".to_vec()))
            .unwrap()
            .len(),
        1
    );
    q(&c, "ROLLBACK");
    q(&c, "UPDATE docs SET payload=X'3132'");
}

#[test]
fn binary_check_truth_conditions_persist_and_reject_false_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("binary-truth.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(
            &c,
            "INSERT INTO docs (id,valid,searched,payload) VALUES (docs:saved,1,1,X'31')",
        );
        q(
            &c,
            "DEFINE FIELD valid ON docs TYPE integer NULLABLE CHECK(payload)",
        );
        q(&c, "DEFINE FIELD searched ON docs TYPE integer CHECK(CASE WHEN payload THEN payload ELSE X'30' END)");
        q(&c, "UPDATE docs SET valid=NULL");
        q(&c, "UPDATE docs SET valid=1");
        q(&c, "CREATE INDEX docs_payload ON docs(payload)");
        q(&c, "BEGIN");
        for value in ["X'30'", "X''", "X'6162'", "NULL"] {
            for assignment in ["payload", "valid=NULL,payload"] {
                let sql = format!("UPDATE docs SET {assignment}={value}");
                assert_eq!(
                    c.execute(&sql, &Parameters::new()).unwrap_err().code(),
                    "FDB_VALIDATION",
                    "{sql}"
                );
                assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
            }
        }
        assert_eq!(
            c.lookup_index("docs", "docs_payload", &Value::Binary(b"1".to_vec()))
                .unwrap()
                .len(),
            1
        );
        q(&c, "ROLLBACK");
        q(&c, "UPDATE docs SET payload=X'2D32'");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        c.execute("UPDATE docs SET payload=X'30'", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    assert_eq!(
        c.lookup_index("docs", "docs_payload", &Value::Binary(b"-2".to_vec()))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn simple_case_checks_match_binary_values_and_persist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("simple-case.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(
            &c,
            "INSERT INTO docs (id,valid,payload) VALUES (docs:saved,1,X'31')",
        );
        q(&c, "DEFINE FIELD valid ON docs TYPE integer CHECK(CASE payload WHEN X'31' THEN 1 ELSE 0 END)");
        q(&c, "CREATE INDEX docs_payload ON docs(payload)");
        q(&c, "BEGIN");
        for value in ["X'32'", "X''", "NULL"] {
            let sql = format!("UPDATE docs SET payload={value}");
            assert_eq!(
                c.execute(&sql, &Parameters::new()).unwrap_err().code(),
                "FDB_VALIDATION"
            );
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        }
        q(&c, "ROLLBACK");
        for (field, check) in [
            (
                "functions",
                "CASE substr(payload,1,1) WHEN X'31' THEN 1 ELSE 0 END",
            ),
            ("reversed", "CASE X'31' WHEN payload THEN 1 ELSE 0 END"),
            (
                "casts",
                "CASE CAST(payload AS INTEGER) WHEN 1 THEN 1 ELSE 0 END",
            ),
            (
                "cast_text",
                "CASE CAST(payload AS INTEGER) WHEN '1' THEN 0 ELSE 1 END",
            ),
            (
                "nested",
                "CASE (CASE WHEN valid THEN payload ELSE X'' END) WHEN X'31' THEN 1 ELSE 0 END",
            ),
        ] {
            q(&c, &format!("UPDATE docs SET {field}=1"));
            q(
                &c,
                &format!("DEFINE FIELD {field} ON docs TYPE integer CHECK({check})"),
            );
        }
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        c.execute("UPDATE docs SET payload=X'32'", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    q(&c, "UPDATE docs SET payload=X'31'");
    assert_eq!(
        c.lookup_index("docs", "docs_payload", &Value::Binary(b"1".to_vec()))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn binary_check_equality_and_membership_use_typed_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("binary-comparisons.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(
            &c,
            "INSERT INTO docs (id,valid,payload) VALUES (docs:saved,1,X'31')",
        );
        for (field, check) in [
            ("valid", "payload=X'31'"),
            ("reversed", "X'31' IS payload"),
            ("different", "payload!=X'32' AND payload IS NOT X'32'"),
            ("functions", "substr(payload,1,1)=payload"),
            ("members", "X'31' IN (payload,substr(payload,1,1))"),
            ("negative", "payload NOT IN (X'32',X'33')"),
            ("casts", "CAST(payload AS INTEGER)='1'"),
            ("collated", "CAST(X'61' AS TEXT) COLLATE NOCASE='A'"),
        ] {
            q(&c, &format!("UPDATE docs SET {field}=1"));
            q(
                &c,
                &format!("DEFINE FIELD {field} ON docs TYPE integer CHECK({check})"),
            );
        }
        q(&c, "UPDATE docs SET nullable_probe=1");
        assert_eq!(c.execute("DEFINE FIELD nullable_probe ON docs TYPE integer CHECK(payload NOT IN (X'32',NULL))", &Parameters::new()).unwrap_err().code(), "FDB_VALIDATION");
        q(&c, "CREATE TABLE guards");
        let bytes = b"FDB\x01{\"type\":\"Record\",\"value\":{\"table\":\"docs\",\"key\":{\"String\":\"saved\"}}}";
        c.execute(
            "INSERT INTO guards (valid,payload,reference) VALUES (1,$bytes,$record)",
            &Parameters::from([
                ("$bytes".into(), Value::Binary(bytes.to_vec())),
                (
                    "$record".into(),
                    Value::Record(Record {
                        table: "docs".into(),
                        key: Key::String("saved".into()),
                    }),
                ),
            ]),
        )
        .unwrap();
        q(&c, "DEFINE FIELD valid ON guards TYPE integer CHECK(payload!=reference AND payload NOT IN (reference))");
        q(&c, "CREATE INDEX docs_payload ON docs(payload)");
        q(&c, "BEGIN");
        for sql in [
            "UPDATE docs SET payload=X'32'",
            "INSERT INTO docs (valid,payload) VALUES (1,X'32')",
            "UPSERT docs:saved {payload:null}",
        ] {
            assert_eq!(
                c.execute(sql, &Parameters::new()).unwrap_err().code(),
                "FDB_VALIDATION"
            );
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        }
        q(&c, "ROLLBACK");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    q(&c, "UPDATE docs SET payload=X'31'");
    assert_eq!(
        c.execute("UPDATE docs SET payload=X'32'", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    assert_eq!(
        c.lookup_index("docs", "docs_payload", &Value::Binary(b"1".to_vec()))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn binary_check_literal_ranges_validate_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("binary-ranges.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(
            &c,
            "INSERT INTO docs (id,valid,payload) VALUES (docs:saved,1,X'02')",
        );
        q(&c, "DEFINE FIELD valid ON docs TYPE integer CHECK(payload >= (X'02') AND payload <= X'0A' AND payload BETWEEN X'02' AND X'0A' AND X'01' < payload AND payload NOT BETWEEN X'0B' AND X'FF')");
        q(&c, "UPDATE docs SET number=-2");
        q(
            &c,
            "DEFINE FIELD number ON docs TYPE integer CHECK(number BETWEEN -3 AND +2)",
        );
        assert_eq!(
            c.execute("UPDATE docs SET number=3", &Parameters::new())
                .unwrap_err()
                .code(),
            "FDB_VALIDATION"
        );
        q(&c, "CREATE INDEX docs_payload ON docs(payload)");
        q(&c, "BEGIN");
        for value in ["X''", "X'01'", "X'0B'", "NULL"] {
            assert_eq!(
                c.execute(
                    &format!("UPDATE docs SET payload={value}"),
                    &Parameters::new()
                )
                .unwrap_err()
                .code(),
                "FDB_VALIDATION"
            );
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        }
        q(&c, "ROLLBACK");
        q(&c, "UPDATE docs SET payload=X'0A'");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        c.execute("UPDATE docs SET payload=X'0B'", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    assert_eq!(
        c.lookup_index("docs", "docs_payload", &Value::Binary(vec![10]))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn binary_glob_checks_validate_payloads_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("binary-glob.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(
            &c,
            "INSERT INTO docs (id,valid,payload,pattern) VALUES (docs:saved,1,X'616263',X'612A')",
        );
        q(&c, "DEFINE FIELD valid ON docs TYPE integer CHECK(payload GLOB pattern AND payload NOT GLOB 'b*')");
        q(&c, "CREATE INDEX docs_payload ON docs(payload)");
        q(&c, "BEGIN");
        for assignment in ["payload=X'626364'", "payload=NULL", "pattern=X'622A'"] {
            assert_eq!(
                c.execute(&format!("UPDATE docs SET {assignment}"), &Parameters::new())
                    .unwrap_err()
                    .code(),
                "FDB_VALIDATION"
            );
            assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        }
        q(&c, "ROLLBACK");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        c.execute("UPDATE docs SET payload=X'626364'", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    assert_eq!(
        c.lookup_index("docs", "docs_payload", &Value::Binary(b"abc".to_vec()))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn json_arrow_checks_read_candidate_payloads_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("json-arrow.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE docs");
        q(
            &c,
            "INSERT INTO docs (id,valid,payload) VALUES (docs:saved,1,X'7B2278223A327D')",
        );
        q(
            &c,
            "DEFINE FIELD valid ON docs TYPE integer CHECK(payload->>'$.x'=2)",
        );
        q(&c, "CREATE INDEX docs_payload ON docs(payload)");
        q(&c, "BEGIN");
        assert_eq!(
            c.execute(
                "UPDATE docs SET payload=X'7B2278223A337D'",
                &Parameters::new()
            )
            .unwrap_err()
            .code(),
            "FDB_VALIDATION"
        );
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        q(&c, "ROLLBACK");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    q(&c, "UPDATE docs SET payload=X'7B2278223A327D'");
    assert_eq!(
        c.execute(
            "UPDATE docs SET payload=X'7B2278223A337D'",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_VALIDATION"
    );
    assert_eq!(
        c.lookup_index(
            "docs",
            "docs_payload",
            &Value::Binary(br#"{"x":2}"#.to_vec())
        )
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn direct_rust_check_depth_is_rejected_before_definition_changes() {
    let (_db, c) = setup();
    q(&c, "BEGIN");
    q(&c, "INSERT INTO users {id:users:prior,name:'Alice'}");
    let before = q(&c, "INFO FOR TABLE users").rows;
    for depth in [65, 10_000] {
        let field = romedb::Field {
            path: vec!["name".into()],
            kind: romedb::FieldType::String,
            required: true,
            nullable: false,
            check: Some(format!(
                "{}length(name)>0{}",
                "(".repeat(depth),
                ")".repeat(depth)
            )),
        };
        assert_eq!(
            c.define_field("users", field, true).unwrap_err().code(),
            "FDB_SYNTAX"
        );
        assert_eq!(q(&c, "INFO FOR TABLE users").rows, before);
        assert_eq!(
            c.lookup_index("users", "users_name", &Value::String("Alice".into()))
                .unwrap()
                .len(),
            1
        );
    }
    assert!(c
        .execute("INSERT INTO users {name:''}", &Parameters::new())
        .is_err());
    q(&c, "INSERT INTO users {name:'Bob'}");
    q(&c, "ROLLBACK");
    assert!(q(&c, "SELECT * FROM users").rows.is_empty());
}
