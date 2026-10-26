use std::{
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn line_mode_buffer_policy_preserves_transaction_and_allows_smaller_retry() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
        .args(["--line", "--write-buffer-limits", "1", "1000", ":memory:"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"CREATE TABLE docs\nINSERT INTO docs {id:docs:a,n:1}\nBEGIN\nINSERT INTO docs {id:docs:b,n:2}\nUPDATE docs SET n=n+10\nSELECT n FROM docs ORDER BY n\nUPDATE docs SET n=3 WHERE n=2\nROLLBACK\nSELECT n FROM docs\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let rows: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(rows.len(), 9);
    assert_eq!(rows[4]["error"]["code"], "FDB_LIMIT");
    assert_eq!(rows[4]["transaction"]["after"], "active");
    assert_eq!(rows[5]["rows"].as_array().unwrap().len(), 2);
    assert_eq!(rows[6]["affected"], 1);
    assert_eq!(rows[8]["rows"].as_array().unwrap().len(), 1);
}

#[test]
fn invalid_buffer_policy_fails_before_opening_storage() {
    let dir = std::env::temp_dir().join(format!("romedb-buffer-options-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("absent.db");
    for args in [
        vec!["--write-buffer-limits", "-1", "100"],
        vec!["--write-buffer-limits", "1"],
        vec!["--write-buffer-limits", "1", "100", "--export", "docs"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
            .args(args)
            .arg(&path)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!path.exists());
    }
    std::fs::remove_dir_all(dir).unwrap();
}
