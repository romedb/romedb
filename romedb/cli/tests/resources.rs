use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn recursive_sql_returns_errors_instead_of_aborting_the_process() {
    let mut commands = vec![
        "CREATE TABLE docs".to_owned(),
        "CREATE TABLE native(v INTEGER)".to_owned(),
        "BEGIN".to_owned(),
        "INSERT INTO docs {v:1}".to_owned(),
    ];
    let expressions = [
        format!("{}1", "NOT ".repeat(2000)),
        format!("{}1", "+ ".repeat(2000)),
        format!("{}1", "- ".repeat(2000)),
        format!("{}1", "~ ".repeat(2000)),
        format!(
            "{}1{}",
            "CASE WHEN 1 THEN ".repeat(200),
            " ELSE 0 END".repeat(200)
        ),
        vec!["1"; 2000].join("+"),
    ];
    for expr in expressions {
        commands.push(format!("INSERT INTO native SELECT {expr}"));
        commands.push(format!("SELECT {expr} FROM docs"));
    }
    commands.extend(
        [
            "SELECT v FROM docs",
            "SELECT count(*) FROM native",
            "ROLLBACK",
            "SELECT count(*) FROM docs",
        ]
        .map(str::to_owned),
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
        .args(["--line", ":memory:"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all((commands.join("\n") + "\n").as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reports: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(reports.len(), commands.len());
    for report in &reports[4..16] {
        assert_eq!(report["error"]["code"], "FDB_ENGINE");
        assert!(report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("maximum depth 100"));
        assert_eq!(report["transaction"]["after"], "active");
    }
    assert_eq!(reports[16]["rows"][0][0]["value"], 1);
    assert_eq!(reports[17]["rows"][0][0]["value"], 0);
    assert_eq!(reports[19]["rows"][0][0]["value"], 0);
}
