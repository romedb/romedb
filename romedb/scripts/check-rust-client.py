#!/usr/bin/env python3
"""Build a private path-dependent consumer outside the Turso workspace (offline)."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
from shipping_policy import PROFILE, build_environment, check_profile, profile_toml

ROOT = Path(__file__).resolve().parents[2]


def registry_packages(lock):
    return {
        (p["name"], p["version"], p["source"], p.get("checksum"))
        for p in tomllib.loads(lock)["package"]
        if "source" in p
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--shipping-profile", action="store_true", help="Qualify the documented production profile in the standalone consumer")
    args = parser.parse_args()
    baseline = (ROOT / "Cargo.lock").read_text()
    compiler = subprocess.check_output(["rustc", "-vV"], text=True)
    host = next(line.removeprefix("host: ") for line in compiler.splitlines() if line.startswith("host: "))
    environment = build_environment() if args.shipping_profile else os.environ.copy()
    if args.shipping_profile:
        check_profile((ROOT / "Cargo.toml").read_text())
    # Do not make a dependent application rely on the checkout's injected flags.
    for key in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS"):
        environment.pop(key, None)
    environment["CARGO_TARGET_DIR"] = str(ROOT / "target" / "romedb-rust-consumer")
    with tempfile.TemporaryDirectory(prefix="romedb-rust-client-") as directory:
        consumer = Path(directory)
        (consumer / "src").mkdir()
        (consumer / "Cargo.toml").write_text(
            '[package]\nname = "romedb-consumer-smoke"\nversion = "0.0.0"\n'
            'edition = "2021"\npublish = false\n\n[workspace]\n\n'
            '[dependencies]\nromedb = { path = '
            + json.dumps(str(ROOT / "romedb" / "frontend"))
            + ' }\n'
            + (profile_toml() if args.shipping_profile else '')
        )
        shutil.copyfile(ROOT / "Cargo.lock", consumer / "Cargo.lock")
        (consumer / "src" / "main.rs").write_text(r'''
use romedb::{Database, Parameters, Record, Key, Value, IntegrityLimits, IntegrityReport, ProfiledQuery, ResultLimits, CancellationToken};
fn iterator_consumer(file: &str) -> Result<(), Box<dyn std::error::Error>> {
    let empty=Parameters::new();
    let params=Parameters::from([("$delta".into(),Value::Integer(1))]);
    let query="SELECT d.n AS n,y.value AS v FROM iterator_docs d CROSS JOIN json_each(d.payload.inner.j) x CROSS JOIN json_each((WITH a AS (SELECT x.value+$delta AS n) SELECT json_array(n) FROM a)) y ORDER BY d.n,x.key";
    let expected=vec![vec![Value::Integer(1),Value::Integer(2)],vec![Value::Integer(1),Value::Integer(3)],vec![Value::Integer(2),Value::Integer(4)]];
    {
        let db=Database::open(file)?;
        let c=db.connect()?;
        c.execute("CREATE TABLE iterator_docs",&empty)?;
        for (n,json) in [(1,"[1,2]"),(2,"[3]")] {
            let payload=Value::Object(std::collections::BTreeMap::from([("inner".into(),Value::Object(std::collections::BTreeMap::from([("j".into(),Value::String(json.into()))])))]));
            c.execute("INSERT INTO iterator_docs(n,payload) VALUES($n,$p)",&Parameters::from([("$n".into(),Value::Integer(n)),("$p".into(),payload)]))?;
        }
        let limits=ResultLimits {max_rows:3,max_payload_bytes:50};
        assert_eq!(c.select_with_limits(query,&params,limits)?.rows,expected);
        assert_eq!(c.profile_select_with_limits(query,&params,limits)?.result.rows,expected);
        assert_eq!(c.select_with_limits(query,&params,ResultLimits {max_payload_bytes:49,..limits}).unwrap_err().code(),"FDB_LIMIT");
        assert_eq!(c.execute(query,&empty).unwrap_err().code(),"FDB_PARAMETER");
        for sql in ["CREATE TABLE iterator_output","DEFINE FIELD n ON iterator_output TYPE integer CHECK(n<4)","CREATE UNIQUE INDEX iterator_n ON iterator_output(n)","BEGIN","INSERT INTO iterator_output(n) VALUES(0)"] {c.execute(sql,&empty)?;}
        let insert=format!("INSERT INTO iterator_output(n) SELECT v FROM ({query}) RETURNING n");
        assert_eq!(c.execute(&insert,&params).unwrap_err().code(),"FDB_VALIDATION");
        assert_eq!(c.transaction_state(),romedb::TransactionState::Active);
        assert_eq!(c.execute("SELECT n FROM iterator_output",&empty)?.rows,vec![vec![Value::Integer(0)]]);
        assert!(c.lookup_index("iterator_output","iterator_n",&Value::Integer(2))?.is_empty());
        let retry=format!("INSERT INTO iterator_output(n) SELECT v FROM ({query}) WHERE v<4 RETURNING n");
        assert_eq!(c.execute(&retry,&params)?.rows,vec![vec![Value::Integer(2)],vec![Value::Integer(3)]]);
        c.execute("ROLLBACK",&empty)?;
    }
    let db=Database::open(file)?;
    let c=db.connect()?;
    assert_eq!(c.execute(query,&params)?.rows,expected);
    for (projection,totals) in [
        ("(WITH a AS (SELECT d.n+x.value+$delta AS v FROM main.json_each(d.payload.inner.j) x) SELECT sum(v) FROM a)",[7,6]),
        ("(WITH a AS (SELECT x.value AS v FROM temp.json_each(d.payload.inner.j) x UNION ALL SELECT d.n+$delta) SELECT sum(v) FROM a)",[5,6]),
    ] {
        let sql=format!("SELECT d.n AS n,{projection} AS total FROM iterator_docs d ORDER BY d.n");
        let rows=vec![vec![Value::Integer(1),Value::Integer(totals[0])],vec![Value::Integer(2),Value::Integer(totals[1])]];
        let limits=ResultLimits {max_rows:2,max_payload_bytes:38};
        assert_eq!(c.select_with_limits(&sql,&params,limits)?.rows,rows);
        assert_eq!(c.profile_select_with_limits(&sql,&params,limits)?.result.rows,rows);
        assert_eq!(c.execute(&sql,&empty).unwrap_err().code(),"FDB_PARAMETER");
        assert_eq!(c.select_with_limits(&sql,&params,ResultLimits {max_rows:1,..limits}).unwrap_err().code(),"FDB_LIMIT");
        assert_eq!(c.execute(&sql,&params)?.rows,rows);
    }
    assert_eq!(c.check_collection_integrity("iterator_output",IntegrityLimits::default())?.index_entries,0);
    Ok(())
}
fn tuple_consumer(file: &str) -> Result<(), Box<dyn std::error::Error>> {
    let empty=Parameters::new();
    let record=Value::Record(Record {table:"tuples".into(),key:Key::Integer(i64::MAX)});
    let payload=Value::Array(vec![Value::Boolean(true),Value::Binary(vec![0,255]),Value::Integer(i64::MIN)]);
    {
        let db=Database::open(file)?;
        let c=db.connect()?;
        c.execute("CREATE TABLE tuples",&empty)?;
        c.execute("INSERT INTO tuples(a,b) VALUES($a,$b)",&Parameters::from([("$a".into(),record.clone()),("$b".into(),payload.clone())]))?;
        c.execute("BEGIN",&empty)?;
        assert_eq!(c.execute("UPDATE tuples SET (a,b)=(VALUES(b,a)) RETURNING a,b",&empty)?.rows,vec![vec![payload.clone(),record.clone()]]);
        assert_eq!(c.execute("UPDATE tuples SET (a,b)=(VALUES(1,2),(3,4))",&empty).unwrap_err().code(),"FDB_UNSUPPORTED");
        assert_eq!(c.transaction_state(),romedb::TransactionState::Active);
        c.execute("ROLLBACK",&empty)?;
        assert_eq!(c.execute("SELECT a,b FROM tuples",&empty)?.rows,vec![vec![record.clone(),payload.clone()]]);
        c.execute("CREATE TABLE tuple_source",&empty)?;
        c.execute("INSERT INTO tuple_source(a,b) SELECT a,b FROM tuples",&empty)?;
        c.execute("INSERT INTO tuple_source(a,b) SELECT a,b FROM tuples",&empty)?;
        c.execute("CREATE TABLE tuple_keys(n INTEGER)",&empty)?;
        c.execute("BEGIN",&empty)?;
        assert_eq!(c.execute("UPDATE tuples SET (a,b)=(SELECT b,a UNION ALL SELECT b,a LIMIT 1) RETURNING a,b",&empty)?.rows,vec![vec![payload.clone(),record.clone()]]);
        c.execute("UPDATE tuples SET (a,b)=(NULL,NULL)",&empty)?;
        let joined="UPDATE tuples SET (a,b)=(s.b,s.a) FROM (SELECT a,b FROM (SELECT a,b FROM tuple_source LIMIT 1) LIMIT $count OFFSET $skip) s LEFT JOIN tuple_keys k ON k.n=1 WHERE k.n IS NULL RETURNING a,b";
        assert_eq!(c.execute(joined,&Parameters::from([("$count".into(),Value::Integer(1))])).unwrap_err().code(),"FDB_CONSTRAINT");
        assert_eq!(c.execute(joined,&Parameters::from([("$count".into(),Value::Integer(1)),("$skip".into(),Value::Integer(1))]))?.affected,0);
        let result=c.execute(joined,&Parameters::from([("$count".into(),Value::Integer(1)),("$skip".into(),Value::Integer(0))]))?;
        assert_eq!(result.rows,vec![vec![payload.clone(),record.clone()]]);
        assert_eq!(result.affected,1);
        let scoped="WITH d AS (SELECT a,b FROM (SELECT a,b FROM tuple_source LIMIT 1) LIMIT $count OFFSET $skip) UPDATE tuples AS d SET (a,b)=(s.b,s.a) FROM tuple_keys k RIGHT JOIN d s ON k.n=1 LEFT JOIN tuple_keys extra ON extra.n=1 WHERE k.n IS NULL AND extra.n IS NULL RETURNING a,b";
        let result=c.execute(scoped,&Parameters::from([("$count".into(),Value::Integer(1)),("$skip".into(),Value::Integer(0))]))?;
        assert_eq!(result.rows,vec![vec![payload.clone(),record.clone()]]);
        assert_eq!(result.affected,1);
        assert_eq!(c.transaction_state(),romedb::TransactionState::Active);
        c.execute("UPDATE tuples SET j='[7]'",&empty)?;
        let result=c.execute("UPDATE tuples AS d SET result=array::new(x.value,$record) FROM json_each(d.j) x RETURNING result",&Parameters::from([("$record".into(),record.clone())]))?;
        assert_eq!(result.affected,1);
        assert_eq!(result.rows,vec![vec![Value::Array(vec![Value::Integer(7),record.clone()])]]);
        c.execute("COMMIT",&empty)?;
    }
    let db=Database::open(file)?;
    let c=db.connect()?;
    assert_eq!(c.execute("SELECT result FROM tuples",&empty)?.rows,vec![vec![Value::Array(vec![Value::Integer(7),record.clone()])]]);
    assert_eq!(c.execute("SELECT a,b FROM tuples",&empty)?.rows,vec![vec![payload,record]]);
    assert_eq!(c.check_collection_integrity("tuples",IntegrityLimits::default())?.documents,1);
    Ok(())
}
fn conflict_consumer(file: &str) -> Result<(), Box<dyn std::error::Error>> {
    let empty=Parameters::new();
    let payload=Value::Array(vec![Value::Record(Record {table:"items".into(),key:Key::Integer(i64::MAX)}),Value::Binary(vec![0,255])]);
    for policy in ["ABORT","ROLLBACK","IGNORE","FAIL","REPLACE"] {
        let path=format!("{file}.{policy}");
        let retained=matches!(policy,"IGNORE"|"FAIL"|"REPLACE");
        let pending=policy!="ROLLBACK";
        {
            let db=Database::open(&path)?;
            let c=db.connect()?;
            for sql in ["CREATE TABLE items","CREATE INDEX items_v ON items(v)","CREATE UNIQUE INDEX items_n ON items(n)","INSERT INTO items(n,v) VALUES(1,0),(2,0)","BEGIN","INSERT INTO items(n,v) VALUES(3,0)"] { c.execute(sql,&empty)?; }
            let result=c.execute(&format!("UPDATE OR {policy} items SET n=10,v=7,payload=$value WHERE n<3 RETURNING n,payload"),&Parameters::from([("$value".into(),payload.clone())]));
            if matches!(policy,"IGNORE"|"REPLACE") {
                let result=result?;
                let count=if policy=="REPLACE" {2} else {1};
                assert_eq!(result.affected,count);
                assert_eq!(result.rows,vec![vec![Value::Integer(10),payload.clone()];count as usize]);
            } else { assert_eq!(result.unwrap_err().code(),"FDB_CONSTRAINT"); }
            assert_eq!(c.transaction_state(),if pending {romedb::TransactionState::Active} else {romedb::TransactionState::Autocommit});
            if pending { c.execute("COMMIT",&empty)?; }
        }
        let db=Database::open(&path)?;
        let c=db.connect()?;
        let mut expected=Vec::new();
        if !retained { expected.push(vec![Value::Integer(1),Value::Integer(0),Value::Null]); }
        if policy!="REPLACE" { expected.push(vec![Value::Integer(2),Value::Integer(0),Value::Null]); }
        if pending { expected.push(vec![Value::Integer(3),Value::Integer(0),Value::Null]); }
        if retained { expected.push(vec![Value::Integer(10),Value::Integer(7),payload.clone()]); }
        assert_eq!(c.execute("SELECT n,v,payload FROM items ORDER BY n",&empty)?.rows,expected);
        assert_eq!(c.check_collection_integrity("items",IntegrityLimits::default())?.index_entries,expected.len() as u64 * 2);
        assert_eq!(c.lookup_index("items","items_n",&Value::Integer(10))?.len(),usize::from(retained));
        assert_eq!(c.lookup_index("items","items_v",&Value::Integer(7))?.len(),usize::from(retained));
    }
    Ok(())
}
fn insert_policy_consumer(file: &str) -> Result<(), Box<dyn std::error::Error>> {
    let p=Parameters::new();
    let payload=Value::Array(vec![Value::Record(Record {table:"docs".into(),key:Key::Integer(i64::MAX)}),Value::Binary(vec![0,255])]);
    for policy in ["ABORT","ROLLBACK","FAIL","IGNORE","REPLACE"] {
        let path=format!("{file}.{policy}");
        let pending=policy!="ROLLBACK";
        let prefix=matches!(policy,"FAIL"|"IGNORE"|"REPLACE");
        let continued=matches!(policy,"IGNORE"|"REPLACE");
        {
            let db=Database::open(&path)?;
            let c=db.connect()?;
            for sql in ["CREATE TABLE docs","CREATE UNIQUE INDEX docs_n ON docs(n)","INSERT INTO docs(id,n) VALUES(docs:a,1)","BEGIN","INSERT INTO docs(id,n) VALUES(docs:pending,9)"] {c.execute(sql,&p)?;}
            let result=c.execute(&format!("INSERT OR {policy} INTO docs(id,n,payload) VALUES(docs:b,2,$value),(docs:a,3,$value),(docs:c,4,$value) RETURNING n,payload"),&Parameters::from([("$value".into(),payload.clone())]));
            if continued {
                let result=result?;
                let keys=if policy=="REPLACE" {vec![2,3,4]} else {vec![2,4]};
                assert_eq!(result.affected,keys.len() as i64);
                assert_eq!(result.rows,keys.into_iter().map(|n|vec![Value::Integer(n),payload.clone()]).collect::<Vec<_>>());
            } else {assert_eq!(result.unwrap_err().code(),"FDB_CONSTRAINT");}
            assert_eq!(c.transaction_state(),if pending {romedb::TransactionState::Active} else {romedb::TransactionState::Autocommit});
            if pending {c.execute("COMMIT",&p)?;}
        }
        let db=Database::open(&path)?;
        let c=db.connect()?;
        let mut expected=Vec::new();
        if policy!="REPLACE" {expected.push(vec![Value::Integer(1),Value::Null]);}
        if prefix {expected.push(vec![Value::Integer(2),payload.clone()]);}
        if policy=="REPLACE" {expected.push(vec![Value::Integer(3),payload.clone()]);}
        if continued {expected.push(vec![Value::Integer(4),payload.clone()]);}
        if pending {expected.push(vec![Value::Integer(9),Value::Null]);}
        assert_eq!(c.execute("SELECT n,payload FROM docs ORDER BY n",&p)?.rows,expected);
        assert_eq!(c.check_collection_integrity("docs",IntegrityLimits::default())?.index_entries,expected.len() as u64);
        assert_eq!(c.lookup_index("docs","docs_n",&Value::Integer(1))?.len(),usize::from(policy!="REPLACE"));
    }
    Ok(())
}
fn v2_consumer(file: &str) -> Result<(), Box<dyn std::error::Error>> {
    let empty = Parameters::new();
    let expected_id = Value::Record(Record { table: "posts".into(), key: Key::String("one".into()) });
    {
        let db = Database::open(file)?;
        let c = db.connect()?;
        c.execute("INSERT INTO users {id:users:alice,name:'Alice'}", &empty)?;
        c.execute("INSERT INTO posts {id:posts:one,title:'Fast database',author:users:alice,p:geo::point(100,13),v:$v,n:$n}",
            &Parameters::from([("$v".into(), Value::vector32(&[1.0,0.0])?), ("$n".into(), Value::Integer(i64::MAX))]))?;
        c.create_index("posts", "post_author", vec!["author".into()], false)?;
        c.define_relation("authored", "users", "posts", vec!["author".into()])?;
        c.create_spatial_index("posts", "near_posts", vec!["p".into()], false)?;
        c.create_fulltext_index("posts", "text_posts", vec![vec!["title".into()]], false)?;
        c.create_vector_index("posts", "vector_posts", vec!["v".into()], 2, "cosine", false)?;
        c.create_function("app::upper", vec![("v".into(), "string".into())], "string", "return v.toUpperCase();", false)?;
        assert_eq!(c.search_vectors("vector_posts", &Value::vector32(&[1.0,0.0])?, 1)?.rows[0][0], expected_id);
        c.execute("BEGIN", &empty)?;
        c.execute("UPDATE posts SET title='changed',p=geo::point(0,0),v=vector32('[0,1]')", &empty)?;
        assert!(c.execute("SELECT id FROM search::text('text_posts','database',20)", &empty)?.rows.is_empty());
        c.execute("ROLLBACK", &empty)?;
    }
    let db = Database::open(file)?;
    let c = db.connect()?;
    for query in [
        "SELECT id FROM search::text('text_posts','database',20)",
        "SELECT id FROM search::vector('vector_posts',vector32('[1,0]'),1)",
        "SELECT id FROM search::near('near_posts',geo::point(100,13),1)",
    ] {
        assert_eq!(c.execute(query, &empty)?.rows, vec![vec![expected_id.clone()]]);
    }
    let row = c.execute("SELECT posts:one {title,author.* AS writer}", &empty)?.exactly_one()?;
    assert_eq!(row[0], Value::String("Fast database".into()));
    let Value::Object(ref author) = row[1] else { panic!("missing forward projection") };
    assert_eq!(author["name"], Value::String("Alice".into()));
    let row = c.execute("SELECT relation::fetch(users:alice,'authored',20)", &empty)?.exactly_one()?;
    let Value::Array(ref posts) = row[0] else { panic!("missing inverse relation") };
    let Value::Object(ref post) = posts[0] else { panic!("missing inverse document") };
    assert_eq!(post["id"], expected_id);
    assert_eq!(post["n"], Value::Integer(i64::MAX));
    assert_eq!(c.execute("SELECT geo::cell(p,7) FROM posts", &empty)?.rows, vec![vec![Value::String("87658b314ffffff".into())]]);
    assert_eq!(c.execute("SELECT app::upper('hello')", &empty)?.rows, vec![vec![Value::String("HELLO".into())]]);
    c.execute("BEGIN", &empty)?;
    c.drop_relation("authored", false)?;
    c.drop_function("app::upper", false)?;
    c.execute("DROP INDEX text_posts", &empty)?;
    assert_eq!(c.execute("PRAGMA integrity_check", &empty)?.rows, vec![vec![Value::String("ok".into())]]);
    c.execute("ROLLBACK", &empty)?;
    assert_eq!(c.execute("SELECT app::upper('restored')", &empty)?.rows, vec![vec![Value::String("RESTORED".into())]]);
    assert_eq!(c.execute("SELECT id FROM search::text('text_posts','database',20)", &empty)?.rows, vec![vec![expected_id]]);
    assert_eq!(c.execute("PRAGMA integrity_check", &empty)?.rows, vec![vec![Value::String("ok".into())]]);
    assert_eq!(c.check_collection_integrity("posts", IntegrityLimits::default())?.documents, 1);
    println!("Standalone V2 API smoke passed: direct index/relation/function APIs, RomeQL search/projections/H3, rollback, integrity and reopen");
    Ok(())
}
fn v22_consumer(file: &str) -> Result<(), Box<dyn std::error::Error>> {
    let p=Parameters::new();
    let db=Database::open(file)?;
    let c=db.connect()?;
    for sql in [
        "CREATE TABLE docs",
        "DEFINE FIELD stamp ON docs TYPE integer DEFAULT 7 READONLY",
        "DEFINE FIELD total ON docs TYPE integer VALUE (n*2)",
        "DEFINE FIELD tags ON docs TYPE array<integer>",
        "INSERT INTO docs {id:docs:a,n:1,uid:'one',tags:[1],body:'Alpha beta',v:vector32('[1,0]')}",
        "CREATE UNIQUE INDEX pair ON docs(n,uid)",
        "CREATE SEARCH INDEX tags ON docs(tags) USING ARRAY",
        "CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT WITH(tokenizer='simple')",
        "CREATE SEARCH INDEX vectors ON docs(v) USING VECTOR WITH(dimensions=2,metric='l2',quantization='f16')",
        "UPDATE docs:a MERGE {body:'Alpha beta'}",
        "INSERT INTO docs(n,uid,tags) VALUES(1,'one',array::new(2)) ON CONFLICT(n,uid) DO UPDATE SET n=2,tags=excluded.tags",
        "UPDATE docs:a PATCH [{op:'add',path:'/tags/-',value:3}]",
    ] {c.execute(sql,&p)?;}
    assert_eq!(c.execute("SELECT n,total,stamp,tags FROM docs",&p)?.rows,vec![vec![Value::Integer(2),Value::Integer(4),Value::Integer(7),Value::Array(vec![Value::Integer(2),Value::Integer(3)])]]);
    let id=Value::Record(Record {table:"docs".into(),key:Key::String("a".into())});
    let ids=Parameters::from([("$ids".into(),Value::Array(vec![id.clone()]))]);
    for sql in ["SELECT id FROM search::text('words','Alpha',1,$ids)","SELECT id FROM search::vector('vectors',vector32('[1,0]'),1,$ids)"] {
        assert_eq!(c.execute(sql,&ids)?.rows,vec![vec![id.clone()]]);
    }
    assert_eq!(c.execute("SELECT id FROM docs WHERE array::contains(tags,3)",&p)?.rows,vec![vec![id.clone()]]);
    assert_eq!(c.execute("SELECT DISTINCT tags FROM docs UNION SELECT tags FROM docs",&p)?.rows,vec![vec![Value::Array(vec![Value::Integer(2),Value::Integer(3)])]]);
    assert_eq!(c.execute("SELECT n FROM docs WHERE (n,tags) IN(SELECT n,tags FROM docs)",&p)?.rows,vec![vec![Value::Integer(2)]]);
    assert_eq!(c.execute("WITH RECURSIVE r(n) AS (SELECT n FROM docs UNION ALL SELECT n+1 FROM r WHERE n<4) SELECT n FROM r",&p)?.rows,vec![vec![Value::Integer(2)],vec![Value::Integer(3)],vec![Value::Integer(4)]]);
    assert_eq!(c.execute("UPDATE docs SET stamp=8",&p).unwrap_err().code(),"FDB_VALIDATION");
    for index in ["pair","tags","words","vectors"] {c.execute(&format!("REINDEX {index}"),&p)?;}
    c.execute("BEGIN",&p)?;
    c.execute("UPDATE docs:a MERGE {n:3}",&p)?;
    assert_eq!(c.execute("UPDATE docs SET n=8 TIMEOUT 0ms",&p).unwrap_err().code(),"FDB_CANCELLED");
    c.execute("ROLLBACK",&p)?;
    assert_eq!(c.lookup_compound_index("docs","pair",&[Value::Integer(2),Value::String("one".into())])?.len(),1);
    c.check_collection_integrity("docs",Default::default())?;
    drop(c);drop(db);
    let c=Database::open(file)?.connect()?;
    assert_eq!(c.execute("SELECT n,total FROM docs",&p)?.rows,vec![vec![Value::Integer(2),Value::Integer(4)]]);
    c.check_collection_integrity("docs",Default::default())?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file = std::env::args().nth(1).expect("database path");
    v2_consumer(&format!("{file}.v2"))?;
    v22_consumer(&format!("{file}.v22"))?;
    iterator_consumer(&format!("{file}.iterators"))?;
    tuple_consumer(&format!("{file}.tuples"))?;
    conflict_consumer(&format!("{file}.conflicts"))?;
    insert_policy_consumer(&format!("{file}.insert-policies"))?;
    let id = Record { table: "docs".into(), key: Key::String("saved".into()) };
    let portable = Value::Array(vec![
        Value::Integer(i64::MAX), Value::Number(-0.0),
        Value::Binary(vec![0, 127, 255]), Value::Record(id.clone()),
        Value::String("quoted\" key\nไทย".into()),
    ]);
    let encoded = portable.clone().into_portable_json()?.parse()?;
    let mut written = Vec::new();
    portable.clone().write_portable_json(&mut written)?;
    assert_eq!(written, portable.clone().into_portable_json()?.as_bytes());
    assert_eq!(portable, Value::from_portable_value(encoded)?);
    assert_eq!(Value::Number(f64::NAN).into_portable_json().unwrap_err().code(), "FDB_VALIDATION");
    {
        let db = Database::open(&file)?;
        let c = db.connect()?;
        c.execute("CREATE TABLE docs", &Parameters::new())?;
        c.execute("DEFINE FIELD value ON docs TYPE integer REQUIRED CHECK(value>0)", &Parameters::new())?;
        c.execute("CREATE UNIQUE INDEX docs_value ON docs(value)", &Parameters::new())?;
        c.execute("INSERT INTO docs (id,value) VALUES ($id,$value)", &Parameters::from([
            ("$id".into(),Value::Record(id.clone())), ("$value".into(),Value::Integer(i64::MAX)),
        ]))?;
        c.execute("BEGIN", &Parameters::new())?;
        c.execute("UPDATE docs SET value=7", &Parameters::new())?;
        let report=c.execute_report("UPDATE docs SET value=-1", &Parameters::new());
        assert_eq!(report.result.unwrap_err().code(), "FDB_VALIDATION");
        assert_eq!(report.transaction_after,romedb::TransactionState::Active);
        let limit = c.check_collection_integrity("docs", IntegrityLimits { max_documents: 0, ..IntegrityLimits::default() }).unwrap_err();
        assert_eq!(limit.code(), "FDB_LIMIT");
        assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
        c.execute("ROLLBACK", &Parameters::new())?;
        assert_eq!(c.lookup_index("docs","docs_value",&Value::Integer(i64::MAX))?.len(),1);
        assert!(c.lookup_index("docs","docs_value",&Value::Integer(7))?.is_empty());
        assert_eq!(c.execute("SELECT string::slugify('Hello Rust') AS slug",&Parameters::new())?.exactly_one()?,vec![Value::String("hello-rust".into())]);
        for vector in [Value::vector32_sparse_entries(3, &[(0,1.0),(2,-1.0)])?, Value::vector32(&[1.0,0.0,-1.0])?, Value::vector64(&[0.1,0.2,0.3])?, Value::vector32_sparse(&[1.0,0.0,-1.0])?, Value::vector8(&[1.0,0.0,-1.0])?, Value::vector1bit(&[1.0,0.0,-1.0])?] {
            assert_eq!(vector.vector_dimensions()?,3);
            assert_eq!(c.execute("SELECT $v AS embedding",&Parameters::from([("$v".into(),vector.clone())]))?.exactly_one()?,vec![vector]);
        }
    }
    let db=Database::open(&file)?;
    let c=db.connect()?;
    assert_eq!(c.get(&id)?.expect("persisted document")["value"],Value::Integer(i64::MAX));
    assert_eq!(c.lookup_index("docs","docs_value",&Value::Integer(i64::MAX))?.len(),1);
    let audit: IntegrityReport = c.check_collection_integrity("docs", IntegrityLimits::default())?;
    assert_eq!((audit.documents,audit.indexes,audit.index_entries),(1,1,1));
    assert!(audit.encoded_bytes>0);
    assert_eq!(c.check_collection_integrity("docs", IntegrityLimits { max_documents: 1, max_encoded_bytes: audit.encoded_bytes })?.encoded_bytes,audit.encoded_bytes);
    let params=Parameters::from([("$value".into(),Value::Integer(i64::MAX))]);
    let profile: ProfiledQuery=c.profile_select("SELECT id,value FROM docs WHERE value=$value", &params)?;
    assert_eq!(profile.result.exactly_one()?,vec![Value::Record(id),Value::Integer(i64::MAX)]);
    assert!(profile.metrics.vm_steps>0);
    // A unique lookup seeks directly and need not iterate an index.
    assert!(profile.metrics.btree_seeks>0);
    assert_eq!(profile.metrics.rows_written,0);
    assert_eq!(c.profile_select("SELECT id,value FROM docs WHERE value=$value", &params)?.metrics, profile.metrics);
    assert_eq!(c.profile_select("DELETE FROM docs", &Parameters::new()).unwrap_err().code(),"FDB_UNSUPPORTED");
    assert_eq!(c.check_collection_integrity("docs", IntegrityLimits::default())?.documents,1);
    let empty = Parameters::new();
    let one = ResultLimits { max_rows: 1, max_payload_bytes: 9 };
    assert_eq!(c.select_with_limits("SELECT value AS v FROM docs", &empty, one)?.rows, vec![vec![Value::Integer(i64::MAX)]]);
    assert_eq!(c.profile_select_with_limits("SELECT value AS v FROM docs", &empty, one)?.result.rows, vec![vec![Value::Integer(i64::MAX)]]);
    assert_eq!(c.select_with_limits("SELECT value AS v FROM docs", &empty, ResultLimits { max_payload_bytes:8, ..one }).unwrap_err().code(), "FDB_LIMIT");
    let fetch = ResultLimits { max_rows:1, max_payload_bytes:25 };
    assert_eq!(c.select_with_limits("SELECT record::fetch(docs:saved) AS v", &empty, fetch)?.rows.len(), 1);
    assert_eq!(c.select_with_limits("SELECT record::fetch(docs:saved) AS v", &empty, ResultLimits { max_payload_bytes:24, ..fetch }).unwrap_err().code(), "FDB_LIMIT");
    c.execute("BEGIN", &empty)?;
    c.execute("UPDATE docs SET value=7", &empty)?;
    let write = "UPDATE docs SET value=8 RETURNING value AS v";
    assert_eq!(c.write_with_result_limits(write, &empty, ResultLimits { max_rows:0, ..one }).unwrap_err().code(), "FDB_LIMIT");
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(c.select_with_limits("SELECT value AS v FROM docs", &empty, one)?.rows, vec![vec![Value::Integer(7)]]);
    assert_eq!(c.write_with_result_limits(write, &empty, one)?.rows, vec![vec![Value::Integer(8)]]);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(c.write_with_result_limits_cancellable("DELETE FROM docs", &empty, one, &cancelled).unwrap_err().code(), "FDB_CANCELLED");
    let expired = CancellationToken::with_deadline(std::time::Instant::now());
    assert_eq!(c.execute_cancellable("DELETE FROM docs", &empty, &expired).unwrap_err().code(), "FDB_CANCELLED");
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(c.execute("SELECT value FROM docs", &empty)?.rows, vec![vec![Value::Integer(8)]]);
    let fresh = CancellationToken::with_deadline(std::time::Instant::now() + std::time::Duration::from_secs(60));
    assert_eq!(c.write_with_result_limits_cancellable("UPDATE docs SET value=9 RETURNING value AS v", &empty, one, &fresh)?.rows, vec![vec![Value::Integer(9)]]);
    assert_eq!(c.select_with_limits_cancellable("SELECT value AS v FROM docs", &empty, one, &fresh)?.rows, vec![vec![Value::Integer(9)]]);
    assert_eq!(c.profile_select_with_limits_cancellable("SELECT value AS v FROM docs", &empty, one, &fresh)?.result.rows, vec![vec![Value::Integer(9)]]);
    c.execute("ROLLBACK", &empty)?;
    assert_eq!(c.lookup_index("docs", "docs_value", &Value::Integer(i64::MAX))?.len(), 1);
    assert_eq!(c.check_collection_integrity("docs", IntegrityLimits::default())?.documents, 1);
    c.execute("BEGIN", &empty)?;
    c.execute("INSERT INTO docs {id:docs:pending,value:2}", &empty)?;
    let c = c.with_write_buffer_limits(ResultLimits { max_rows:1, max_payload_bytes:1000 });
    assert_eq!(c.execute("DELETE FROM docs RETURNING value", &empty).unwrap_err().code(), "FDB_LIMIT");
    assert_eq!(c.transaction_state(), romedb::TransactionState::Active);
    assert_eq!(c.execute("SELECT value FROM docs ORDER BY value", &empty)?.rows,
        vec![vec![Value::Integer(2)],vec![Value::Integer(i64::MAX)]]);
    let c = c.with_write_buffer_limits(ResultLimits { max_rows:2, max_payload_bytes:1000 });
    assert_eq!(c.execute("DELETE FROM docs RETURNING value", &empty)?.rows.len(),2);
    assert_eq!(c.check_collection_integrity("docs", IntegrityLimits::default())?.documents,0);
    c.execute("ROLLBACK", &empty)?;
    assert_eq!(c.lookup_index("docs", "docs_value", &Value::Integer(i64::MAX))?.len(),1);
    assert_eq!(c.check_collection_integrity("docs", IntegrityLimits::default())?.documents,1);
    println!("Standalone Rust client smoke passed: typed values, portable JSON, validation, indexes, rollback, QuickJS, vectors, profiles, audits, result/write buffer limits, cancellation/deadlines, iterator CTEs, UPDATE/INSERT conflict policies and reopen");
    Ok(())
}
''')
        # Let Cargo add the consumer and prune unused workspace packages while
        # retaining the seeded versions. Reject any registry/git dependency drift.
        subprocess.run(
            ["cargo", "metadata", "--offline", "--format-version", "1", "--filter-platform", host],
            cwd=consumer, env=environment, stdout=subprocess.DEVNULL, check=True,
        )
        resolved = registry_packages((consumer / "Cargo.lock").read_text())
        drift = resolved - registry_packages(baseline)
        if drift:
            raise RuntimeError(f"Consumer resolved packages outside the pinned lockfile: {sorted(drift)}")
        subprocess.run(
            ["cargo", "run", "--offline", "--locked", *(["--profile", PROFILE] if args.shipping_profile else []), "--", str(consumer / "database.db")],
            cwd=consumer, env=environment, check=True,
        )
        print(f"Verified {len(resolved)} registry/git package identities against the workspace lockfile")


if __name__ == "__main__":
    main()
