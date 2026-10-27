use std::{
    io::Write,
    process::{Command, Stdio},
};
#[test]
fn cli_attaches_transaction_state_to_success_and_error_lines() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
        .arg("--line")
        .arg(":memory:")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"CREATE TABLE posts\nBEGIN\nINSERT INTO posts {n:1}\nUPDATE posts SET n=2 RETURNING array::append(1,2) AS bad\nSELECT n FROM posts\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let rows = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[1]["transaction"]["before"], "autocommit");
    assert_eq!(rows[1]["transaction"]["after"], "active");
    assert!(rows[3]["error"]["code"].is_string());
    assert_eq!(rows[3]["transaction"]["before"], "active");
    assert_eq!(rows[3]["transaction"]["after"], "active");
    assert_eq!(rows[4]["rows"][0][0]["value"], 1);
    assert_eq!(rows[4]["rows"].as_array().unwrap().len(), 1);
}

#[test]
fn cli_reports_iterator_rollback_and_recovers_after_reopen() {
    let root = std::env::temp_dir().join(format!(
        "romedb-cli-iterator-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("database.db");
    let run = |script: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
            .arg(&file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let rows = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        (output.status.success(), rows)
    };
    let insert="INSERT INTO output(n) SELECT x.value FROM docs d CROSS JOIN json_each(d.j) x ORDER BY d.n RETURNING n;";
    let script=format!("CREATE TABLE docs; INSERT INTO docs(n,j) VALUES(1,'[1]'),(2,'invalid'); CREATE TABLE output; CREATE UNIQUE INDEX output_n ON output(n); INSERT INTO output(n) VALUES(-1); BEGIN; INSERT INTO output(n) VALUES(0); {insert} SELECT 999;");
    let (ok, rows) = run(&script);
    assert!(!ok);
    assert_eq!(rows.len(), 8);
    assert_eq!(rows[7]["error"]["code"], "FDB_ENGINE");
    assert_eq!(
        rows[7]["transaction"],
        serde_json::json!({"before":"active","after":"autocommit"})
    );
    assert_eq!(rows[7]["offset"], script.find(insert).unwrap());
    let (ok,rows)=run(&format!("SELECT n FROM output; UPDATE docs SET j='[2]' WHERE n=2; {insert} SELECT n FROM output ORDER BY n;"));
    assert!(ok);
    assert_eq!(rows[0]["rows"][0][0]["value"], -1);
    assert_eq!(rows[0]["rows"].as_array().unwrap().len(), 1);
    let values = rows[3]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[0]["value"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values, vec![-1, 1, 2]);
    {
        let db = romedb::Database::open(file.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        let audit = c
            .check_collection_integrity("output", romedb::IntegrityLimits::default())
            .unwrap();
        assert_eq!((audit.documents, audit.index_entries), (3, 3));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cli_commits_ordered_tuple_lookups_and_reopens() {
    let root = std::env::temp_dir().join(format!(
        "romedb-cli-tuple-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("database.db");
    let run = |script: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
            .arg(&file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>()
    };
    let reports=run("CREATE TABLE docs; INSERT INTO docs(n) VALUES(1); CREATE TABLE lookup; INSERT INTO lookup(n,a,b) VALUES(1,2,3),(1,4,5); BEGIN; UPDATE docs SET (a,b)=(SELECT x.a,x.b FROM lookup x WHERE x.n=docs.n ORDER BY x.a DESC LIMIT 1) RETURNING a,b; COMMIT;");
    assert_eq!(reports.len(), 7);
    assert_eq!(reports[5]["rows"][0][0]["value"], 4);
    assert_eq!(reports[5]["rows"][0][1]["value"], 5);
    assert_eq!(reports[5]["transaction"]["after"], "active");
    assert_eq!(reports[6]["transaction"]["after"], "autocommit");
    let reopened = run("SELECT a,b FROM docs; UPDATE docs SET (a,b)=(SELECT b,a) RETURNING a,b;");
    assert_eq!(reopened[0]["rows"], reports[5]["rows"]);
    assert_eq!(reopened[1]["rows"][0][0]["value"], 5);
    assert_eq!(reopened[1]["rows"][0][1]["value"], 4);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn inventory_script_failure_discards_uncommitted_events_on_exit() {
    for (replacement, code) in [
        ("('P2',-30)", "FDB_VALIDATION"),
        ("('UNKNOWN',-3)", "FDB_CONSTRAINT"),
    ] {
        let root = std::env::temp_dir().join(format!(
            "romedb-inventory-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("inventory.db");
        let run = |script: &str| {
            let mut child = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
                .arg(&file)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(script.as_bytes())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            let rows = String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            (output.status.success(), rows)
        };
        let script = include_str!("../../examples/inventory-reconciliation.romeql");
        let (ok, rows) = run(&script.replace("('P2',-3)", replacement));
        assert!(!ok);
        let failure = rows.last().unwrap();
        assert_eq!(failure["error"]["code"], code);
        assert_eq!(failure["transaction"]["after"], "active");
        {
            let db = romedb::Database::open(file.to_str().unwrap()).unwrap();
            let c = db.connect().unwrap();
            let p = romedb::Parameters::new();
            assert_eq!(
                c.execute("SELECT quantity FROM inventory ORDER BY sku", &p)
                    .unwrap()
                    .rows,
                vec![
                    vec![romedb::Value::Integer(10)],
                    vec![romedb::Value::Integer(5)]
                ]
            );
            for table in ["inventory_events", "adjustments"] {
                assert_eq!(
                    c.execute(&format!("SELECT count(*) FROM {table}"), &p)
                        .unwrap()
                        .rows,
                    vec![vec![romedb::Value::Integer(0)]]
                );
            }
            assert_eq!(
                c.check_collection_integrity("inventory", Default::default())
                    .unwrap()
                    .index_entries,
                2
            );
        }
        let (_, transaction) = script.split_once("BEGIN;").unwrap();
        assert!(run(&format!("BEGIN;{transaction}")).0);
        {
            let db = romedb::Database::open(file.to_str().unwrap()).unwrap();
            let c = db.connect().unwrap();
            let p = romedb::Parameters::new();
            assert_eq!(
                c.execute("SELECT quantity FROM inventory ORDER BY sku", &p)
                    .unwrap()
                    .rows,
                vec![
                    vec![romedb::Value::Integer(12)],
                    vec![romedb::Value::Integer(2)]
                ]
            );
            assert_eq!(
                c.execute("SELECT count(*) FROM inventory_events", &p)
                    .unwrap()
                    .rows,
                vec![vec![romedb::Value::Integer(2)]]
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
