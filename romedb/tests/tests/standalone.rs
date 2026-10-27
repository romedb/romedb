use romedb::{Database, Key, Parameters, Record, Value};
#[test]
fn standalone_parameters_preserve_tags_and_alias_predicates() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for value in [
        Value::Boolean(true),
        Value::Record(Record {
            table: "docs".into(),
            key: Key::Integer(2),
        }),
        Value::Array(vec![Value::Integer(i64::MAX)]),
        Value::Object(Default::default()),
        Value::vector32(&[1., 2.]).unwrap(),
    ] {
        let params = Parameters::from([("$value".into(), value.clone())]);
        assert_eq!(
            c.execute("SELECT $value AS value", &params).unwrap().rows,
            vec![vec![value.clone()]]
        );
        assert_eq!(
            c.execute("SELECT DISTINCT $value AS value", &params)
                .unwrap()
                .rows,
            vec![vec![value]]
        );
        assert!(c
            .execute("SELECT $value AS value WHERE 0", &params)
            .unwrap()
            .rows
            .is_empty());
    }
    let params = Parameters::from([
        ("$flag".into(), Value::Boolean(true)),
        ("$n".into(), Value::Integer(3)),
    ]);
    assert_eq!(
        c.execute(
            "SELECT $flag AS flag, $n+2 AS n WHERE flag AND n=5",
            &params
        )
        .unwrap()
        .rows,
        vec![vec![Value::Boolean(true), Value::Integer(5)]]
    );
    assert_eq!(
        c.execute(
            "SELECT $flag AS flag, count(*) AS n GROUP BY flag HAVING flag",
            &Parameters::from([("$flag".into(), Value::Boolean(true))])
        )
        .unwrap()
        .rows,
        vec![vec![Value::Boolean(true), Value::Integer(1)]]
    );
    assert_eq!(
        c.execute(
            "SELECT $flag AS flag, 0 AS FLAG WHERE flag",
            &Parameters::from([("$flag".into(), Value::Boolean(true))])
        )
        .unwrap()
        .rows,
        vec![vec![Value::Boolean(true), Value::Integer(0)]]
    );
}
#[test]
fn native_scalar_queries_and_errors_remain_delegated() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let params = Parameters::from([("?1".into(), Value::Integer(2))]);
    assert_eq!(
        c.execute("SELECT ?1 AS x WHERE x=2", &params).unwrap().rows,
        vec![vec![Value::Integer(2)]]
    );
    let params = Parameters::from([("?1".into(), Value::Array(vec![Value::Null]))]);
    assert_eq!(
        c.execute("SELECT ?1", &params).unwrap().rows,
        vec![vec![Value::Array(vec![Value::Null])]]
    );
    assert!(c.execute("SELECT missing, ?1", &params).is_err());
}

#[test]
fn standalone_fetch_aliases_do_not_bypass_fetch_stage_rules() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    c.execute("CREATE TABLE docs", &Parameters::new()).unwrap();
    c.execute("INSERT INTO docs {id:docs:p1,value:1}", &Parameters::new())
        .unwrap();
    assert!(c
        .execute(
            "SELECT record::fetch(docs:p1) AS fetched WHERE fetched IS NOT NULL",
            &Parameters::new()
        )
        .is_err());
    assert!(c
        .execute(
            "SELECT record::fetch(docs:p1) AS fetched GROUP BY fetched",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(
        c.execute(
            "SELECT record::fetch(docs:p1) AS fetched",
            &Parameters::new()
        )
        .unwrap()
        .rows
        .len(),
        1
    );
}
