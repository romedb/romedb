use std::{
    io::Write,
    process::{Command, Stdio},
};

fn run(mode: &str, script: &str) -> (bool, Vec<serde_json::Value>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
        .args([mode, ":memory:"])
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
    let reports = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (output.status.success(), reports)
}

#[test]
fn limited_commands_report_boundaries_and_invalid_arguments() {
    for command in [".select-limit", ".profile-limit"] {
        let (ok, reports) = run("--script", &format!("{command} 1 9 SELECT 7 AS n;"));
        assert!(ok);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0]["rows"][0][0]["value"], 7);
        assert_eq!(
            reports[0].get("profile").is_some(),
            command == ".profile-limit"
        );
        for limits in ["0 9", "1 8"] {
            let (ok, reports) = run("--script", &format!("{command} {limits} SELECT 7 AS n;"));
            assert!(!ok);
            assert_eq!(reports[0]["error"]["code"], "FDB_LIMIT");
            assert!(reports[0].get("rows").is_none());
            assert!(reports[0].get("profile").is_none());
        }
    }
    for input in [
        ".select-limit",
        ".write-limit 1",
        ".profile-limit -1 9 SELECT 1",
        ".select-limit 1 99999999999999999999999999 SELECT 1",
        ".write-limit 1 9",
    ] {
        let (ok, reports) = run("--script", input);
        assert!(!ok, "{input}");
        assert_eq!(reports[0]["error"]["code"], "FDB_VALIDATION");
    }
    let (ok, reports) = run(
        "--script",
        ".select-limit\t1   2\nSELECT record::fetch(missing:key) AS n;",
    );
    assert!(ok);
    assert_eq!(reports[0]["rows"][0][0]["type"], "Null");
    let (ok, reports) = run(
        "--script",
        ".select-limit 1 9 SELECT 1; DELETE FROM missing;",
    );
    assert!(!ok);
    assert_eq!(reports.len(), 1);
    assert!(reports[0].get("error").is_some());
}

#[test]
fn limited_write_commands_preserve_pending_work_and_allow_retry() {
    let script = "CREATE TABLE docs;\nBEGIN;\nINSERT INTO docs {n:9};\n.write-limit 1 17 INSERT INTO docs(n) VALUES(1),(2) RETURNING n;\n.select-limit 1 9 SELECT n FROM docs;\n.write-limit 2 17 INSERT INTO docs(n) VALUES(1),(2) RETURNING n;\n.profile-limit 3 25 SELECT n FROM docs ORDER BY n;\n.write-limit 0 0 COMMIT;\nROLLBACK;\n.select-limit 0 1 SELECT n FROM docs;\n";
    for mode in ["--line", "--interactive"] {
        let (ok, reports) = run(mode, script);
        assert!(!ok);
        assert_eq!(reports.len(), 10, "{mode}: {reports:?}");
        assert_eq!(reports[3]["error"]["code"], "FDB_LIMIT");
        assert_eq!(
            reports[3]["transaction"],
            serde_json::json!({"before":"active","after":"active"})
        );
        assert_eq!(reports[4]["rows"][0][0]["value"], 9);
        assert_eq!(reports[5]["affected"], 2);
        assert_eq!(reports[6]["rows"].as_array().unwrap().len(), 3);
        assert!(reports[6]["profile"].is_object());
        assert_eq!(reports[7]["error"]["code"], "FDB_UNSUPPORTED");
        assert_eq!(reports[7]["transaction"]["after"], "active");
        assert_eq!(reports[9]["rows"], serde_json::json!([]));
    }
}

#[test]
fn interactive_limited_commands_accumulate_sql_and_clear_pending_input() {
    let (ok, reports) = run("--interactive", "CREATE TABLE docs;\n.write-limit 1 9\nINSERT INTO docs {\n n:7\n} RETURNING n;\n.select-limit 1 9 SELECT\n n FROM docs;\n.profile-limit 1 9\nSELECT n\nFROM docs;\n.profile\nSELECT\n n FROM docs;\n.write-limit 1 9 UPDATE docs {\n.clear\n.select-limit 1 9 SELECT n FROM docs;\n.quit\n");
    assert!(ok, "{reports:?}");
    assert_eq!(reports.len(), 6);
    for report in &reports[1..] {
        assert_eq!(report["rows"][0][0]["value"], 7);
    }
    assert!(reports[3]["profile"].is_object());
    assert!(reports[4]["profile"].is_object());
    let (ok, reports) = run(
        "--interactive",
        ".select-limit invalid 9\n.select-limit 1 9 SELECT\n 8 AS n;\n.quit\n",
    );
    assert!(!ok);
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0]["error"]["code"], "FDB_VALIDATION");
    assert_eq!(reports[1]["rows"][0][0]["value"], 8);
}

#[test]
fn timeout_commands_preserve_pending_work_and_multiline_completion() {
    let (ok, rows) = run("--line", "CREATE TABLE docs\nBEGIN\nINSERT INTO docs {n:1}\n.timeout 0 DELETE FROM docs\n.timeout -1 DELETE FROM docs\n.timeout 60000 SELECT n FROM docs\nROLLBACK\nSELECT * FROM docs\n");
    assert!(!ok);
    assert_eq!(rows.len(), 8);
    assert_eq!(rows[3]["error"]["code"], "FDB_CANCELLED");
    assert_eq!(rows[3]["transaction"]["after"], "active");
    assert_eq!(rows[4]["error"]["code"], "FDB_VALIDATION");
    assert_eq!(rows[5]["rows"].as_array().unwrap().len(), 1);
    assert_eq!(rows[7]["rows"].as_array().unwrap().len(), 0);
    let (ok, rows) = run("--interactive", ".timeout 60000\nSELECT\n1 AS n;\n.quit\n");
    assert!(ok);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["rows"][0][0]["value"], 1);
    let (ok, rows) = run("--script", ".timeout 60000 SELECT 1; SELECT 2;");
    assert!(!ok);
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["error"].is_object());
}

#[test]
fn active_cli_deadlines_discard_results_and_preserve_write_recovery() {
    let source = "input a,input b,input c,input d,input e,input f,input g,input h,input i,input j";
    let script = format!("CREATE TABLE input(n)\nINSERT INTO input VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9)\nCREATE TABLE docs\nCREATE UNIQUE INDEX docs_n ON docs(n)\nBEGIN\nINSERT INTO docs {{n:1}}\n.timeout 20 SELECT count(*) FROM {source}\n.timeout 20 INSERT INTO docs(n) SELECT count(*) FROM {source}\n.timeout 60000 SELECT n FROM docs\n.timeout 60000 INSERT INTO docs {{n:2}}\nSELECT n FROM docs ORDER BY n\nROLLBACK\nSELECT * FROM docs\n");
    let (ok, rows) = run("--line", &script);
    assert!(!ok);
    assert_eq!(rows.len(), 13);
    for report in &rows[6..8] {
        assert_eq!(report["error"]["code"], "FDB_CANCELLED");
        assert_eq!(report["transaction"]["before"], "active");
        assert_eq!(report["transaction"]["after"], "active");
        assert!(report.get("rows").is_none());
    }
    assert_eq!(rows[8]["rows"].as_array().unwrap().len(), 1);
    assert_eq!(rows[8]["rows"][0][0]["value"], 1);
    assert_eq!(rows[9]["affected"], 1);
    assert_eq!(rows[10]["rows"].as_array().unwrap().len(), 2);
    assert_eq!(rows[12]["rows"].as_array().unwrap().len(), 0);
}
