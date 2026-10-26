use romedb::{Database, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"))
}
fn define(c: &romedb::Connection, name: &str, params: &str, returns: &str, body: &str) {
    q(c,&format!("CREATE OR REPLACE FUNCTION app::{name}({params}) RETURNS {returns} LANGUAGE JAVASCRIPT AS '{}'",body.replace('\'',"''")));
}
#[test]
fn patched_runtime_preserves_rope_json_indentation() {
    // GHSA-3jf7-4qfx-xc2h: QuickJS-NG 0.15.1 copied pointer bytes instead of
    // the first ten characters when JSON.stringify's indentation was a rope.
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    define(
        &c,
        "rope_json",
        "",
        "string",
        "const a='A'.repeat(10000); const b='B'.repeat(10000); return JSON.stringify({value:1},null,a+b);",
    );
    assert_eq!(
        q(&c, "SELECT app::rope_json()").rows,
        vec![vec![Value::String("{\nAAAAAAAAAA\"value\": 1\n}".into())]]
    );
    let info = q(&c, "INFO FOR FUNCTION app::rope_json");
    let Value::Object(info) = &info.rows[0][0] else {
        panic!("function info must be an object");
    };
    assert_eq!(
        info.get("runtime"),
        Some(&Value::String("quickjs-ng-0.16.2-rquickjs-0.13.0".into()))
    );
}
#[test]
fn typed_calls_select_writes_replacement_and_rollback() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    define(
        &c,
        "normalize",
        "value string",
        "string",
        "return value.trim().toLowerCase();",
    );
    assert_eq!(
        q(&c, "SELECT app::normalize(' ALICE ')").rows,
        vec![vec![Value::String("alice".into())]]
    );
    assert!(!q(&c, "SELECT app::normalize(' x ')").columns[0].contains("__romedb"));
    q(
        &c,
        "INSERT INTO users {id:users:a,name:app::normalize(' ALICE ')}",
    );
    q(
        &c,
        "INSERT INTO users(name) VALUES(app::normalize(' BOB '))",
    );
    assert_eq!(
        q(&c, "SELECT app::normalize(name) AS n FROM users ORDER BY n").rows,
        vec![
            vec![Value::String("alice".into())],
            vec![Value::String("bob".into())]
        ]
    );
    q(
        &c,
        "UPDATE users SET name=app::normalize(' CHARLIE ') WHERE id=users:a",
    );
    q(&c, "BEGIN");
    define(
        &c,
        "normalize",
        "value string",
        "string",
        "return 'replaced';",
    );
    assert_eq!(
        q(&c, "SELECT app::normalize('x')").rows[0][0],
        Value::String("replaced".into())
    );
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT app::normalize('x')").rows[0][0],
        Value::String("x".into())
    );
    define(&c, "identity", "v any", "any", "return v;");
    let value = Value::Object(
        [
            ("large".into(), Value::Integer(i64::MAX)),
            (
                "nested".into(),
                Value::Array(vec![Value::Boolean(true), Value::Null, Value::Number(1.5)]),
            ),
        ]
        .into(),
    );
    let params = Parameters::from([("$v".into(), value.clone())]);
    assert_eq!(
        c.execute("SELECT app::identity($v)", &params).unwrap().rows[0][0],
        value
    );
    let minus_zero = Parameters::from([("$v".into(), Value::Number(-0.0))]);
    let Value::Number(z) = c
        .execute("SELECT app::identity($v)", &minus_zero)
        .unwrap()
        .rows[0][0]
    else {
        panic!("number")
    };
    assert!(z.is_sign_negative());
    define(&c,"tamper","","object","Object.prototype.toJSON=()=>null;Array.prototype[Symbol.iterator]=()=>{throw Error('iterator')};JSON.stringify=()=>null;return {n:1n};");
    assert_eq!(
        q(&c, "SELECT app::tamper()").rows[0][0],
        Value::Object([("n".into(), Value::Integer(1))].into())
    );
    define(&c, "nullable", "v string?", "string?", "return v;");
    assert_eq!(q(&c, "SELECT app::nullable(NULL)").rows[0][0], Value::Null);
    define(&c, "increment", "v integer", "integer", "return v+1n;");
    assert_eq!(
        q(&c, "SELECT app::increment(9223372036854775806)").rows[0][0],
        Value::Integer(i64::MAX)
    );
    q(&c, "DROP FUNCTION app::normalize");
    assert_eq!(
        c.execute("SELECT app::normalize('a')", &Parameters::new())
            .unwrap_err()
            .code(),
        "FDB_NOT_FOUND"
    );
}
#[test]
fn sandbox_limits_and_atomic_failure() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    define(&c,"environment","","string","return [typeof process,typeof require,typeof fetch,typeof std,typeof os,typeof Date,typeof Math.random,typeof WeakRef,typeof SharedArrayBuffer,typeof performance].join(',');");
    assert_eq!(
        q(&c, "SELECT app::environment()").rows[0][0],
        Value::String(["undefined"; 10].join(","))
    );
    define(
        &c,
        "counter",
        "",
        "integer",
        "globalThis.n=(globalThis.n||0n)+1n; return globalThis.n;",
    );
    assert_eq!(
        q(&c, "SELECT app::counter(),app::counter()").rows[0],
        vec![Value::Integer(1); 2]
    );
    for (body, code) in [
        ("while(true){}", "FDB_LIMIT"),
        (
            "let a=[];while(true)a.push(new Array(100000).fill(1));",
            "FDB_LIMIT",
        ),
        ("function f(){return 1+f()} return f();", "FDB_LIMIT"),
        ("return 'a'.repeat(100000);", "FDB_LIMIT"),
        ("return undefined;", "FDB_VALIDATION"),
        ("return NaN;", "FDB_VALIDATION"),
        ("return 9223372036854775808n;", "FDB_VALIDATION"),
        ("let v={};v.x=v;return v;", "FDB_VALIDATION"),
        ("return Promise.resolve(1);", "FDB_VALIDATION"),
        ("return {get x(){while(true){}}};", "FDB_VALIDATION"),
        ("return '\\ud800';", "FDB_VALIDATION"),
    ] {
        define(&c, "bad", "", "any", body);
        let err = c
            .execute("SELECT app::bad()", &Parameters::new())
            .unwrap_err();
        assert_eq!(err.code(), code, "{body}: {err:?}");
        assert_eq!(
            c.transaction_state(),
            romedb::TransactionState::Autocommit,
            "{body}: {err:?}"
        );
        assert_eq!(q(&c, "SELECT 1").rows[0][0], Value::Integer(1));
    }
    q(&c, "CREATE TABLE docs");
    q(&c, "CREATE UNIQUE INDEX docs_n ON docs(n)");
    q(&c, "INSERT INTO docs(n) VALUES(1),(2),(3)");
    define(
        &c,
        "late",
        "n integer",
        "integer",
        "if(n===3n)throw Error('late');return n+10n;",
    );
    q(&c, "BEGIN");
    q(&c, "INSERT INTO docs(n) VALUES(4)");
    assert!(c
        .execute("UPDATE docs SET n=app::late(n)", &Parameters::new())
        .is_err());
    assert_eq!(
        q(&c, "SELECT n FROM docs ORDER BY n").rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(3)],
            vec![Value::Integer(4)]
        ]
    );
    assert_eq!(
        c.check_collection_integrity("docs", Default::default())
            .unwrap()
            .documents,
        4
    );
    q(&c, "ROLLBACK");
}
#[test]
fn definitions_persist_and_are_protected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("functions.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        define(&c, "hello", "v string", "string", "return 'hello '+v;");
        for sql in ["DELETE FROM __romedb_functions", "SELECT __romedb_user_function('{}')", "CREATE FUNCTION string::custom() RETURNS any LANGUAGE JAVASCRIPT AS 'return 1'", "CREATE OR REPLACE FUNCTION app::hello(v string) RETURNS string LANGUAGE JAVASCRIPT AS '} ); throw Error(1); ('"] {
            assert!(c.execute(sql,&Parameters::new()).is_err(),"{sql}");
        }
        assert_eq!(
            q(&c, "SELECT app::hello('x')").rows[0][0],
            Value::String("hello x".into())
        );
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        q(&c, "SELECT app::hello('world')").rows[0][0],
        Value::String("hello world".into())
    );
    let Value::Object(info) = q(&c, "INFO FOR FUNCTION app::hello").rows[0][0].clone() else {
        panic!()
    };
    assert!(matches!(&info["digest"],Value::String(s) if s.len()==64));
}
#[test]
fn function_versions_follow_reader_snapshots_and_cancelled_writes_preserve_prior_work() {
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshots.db");
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let writer = db.connect().unwrap();
    let reader = db.connect().unwrap();
    define(&writer, "version", "", "string", "return 'old';");
    q(&reader, "BEGIN");
    assert_eq!(
        q(&reader, "SELECT app::version()").rows[0][0],
        Value::String("old".into())
    );
    define(&writer, "version", "", "string", "return 'new';");
    assert_eq!(
        q(&reader, "SELECT app::version()").rows[0][0],
        Value::String("old".into())
    );
    q(&reader, "COMMIT");
    assert_eq!(
        q(&reader, "SELECT app::version()").rows[0][0],
        Value::String("new".into())
    );
    define(&writer, "runaway", "", "integer", "while(true){}");
    q(&writer, "CREATE TABLE docs");
    q(&writer, "CREATE UNIQUE INDEX docs_n ON docs(n)");
    for sql in [
        "SELECT app::runaway()",
        "INSERT INTO docs {n:app::runaway()}",
        "UPDATE docs SET n=app::runaway()",
    ] {
        q(&writer, "BEGIN");
        q(&writer, "INSERT INTO docs(n) VALUES(1)");
        let token =
            romedb::CancellationToken::with_deadline(Instant::now() + Duration::from_millis(5));
        let error = writer
            .execute_cancellable(sql, &Parameters::new(), &token)
            .unwrap_err();
        assert_eq!(error.code(), "FDB_CANCELLED", "{sql}: {error:?}");
        assert_eq!(writer.transaction_state(), romedb::TransactionState::Active);
        assert_eq!(
            q(&writer, "SELECT n FROM docs").rows,
            vec![vec![Value::Integer(1)]]
        );
        writer
            .check_collection_integrity("docs", Default::default())
            .unwrap();
        q(&writer, "ROLLBACK");
    }
}
