use std::{
    path::Path,
    process::{Command, Stdio},
};
fn audit(path: &Path, extra: &[&str]) -> (bool, serde_json::Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
        .args(["--check-collection", "docs"])
        .args(extra)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        output.status.success(),
        serde_json::from_slice(&output.stdout).expect("JSON audit report"),
    )
}
fn directory() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "romedb-cli-audit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    path
}
#[test]
fn cli_audit_reports_counts_and_limits_without_changing_documents() {
    let root = directory();
    let path = root.join("audit.db");
    {
        let db = romedb::Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "CREATE TABLE docs",
            "CREATE INDEX docs_n ON docs(n)",
            "INSERT INTO docs {n:1}",
            "INSERT INTO docs {n:2}",
        ] {
            c.execute(sql, &romedb::Parameters::new()).unwrap();
        }
    }
    let (ok, report) = audit(&path, &[]);
    assert!(ok);
    assert_eq!(report["documents"], 2);
    assert_eq!(report["indexes"], 1);
    assert_eq!(report["index_entries"], 2);
    assert_eq!(report["transaction"]["after"], "autocommit");
    let bytes = report["encoded_bytes"].as_u64().unwrap().to_string();
    assert!(
        audit(
            &path,
            &["--max-documents", "2", "--max-encoded-bytes", &bytes]
        )
        .0
    );
    for extra in [["--max-documents", "1"], ["--max-encoded-bytes", "1"]] {
        let (ok, error) = audit(&path, &extra);
        assert!(!ok);
        assert_eq!(error["error"]["code"], "FDB_LIMIT");
        assert!(error.get("documents").is_none());
    }
    assert_eq!(audit(&path, &[]).1["documents"], 2);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn cli_audit_rejects_missing_files_and_conflicting_modes_before_creation() {
    let root = directory();
    let path = root.join("missing.db");
    let (ok, error) = audit(&path, &[]);
    assert!(!ok);
    assert_eq!(error["error"]["code"], "FDB_NOT_FOUND");
    assert!(!path.exists());
    for extra in [
        vec!["--script"],
        vec!["--line"],
        vec!["--interactive"],
        vec!["--max-input-bytes", "12"],
        vec!["--max-documents", "-1"],
        vec!["--max-documents", "18446744073709551616"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
            .args(["--check-collection", "docs"])
            .args(extra)
            .arg(&path)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!path.exists());
    }
    let output = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
        .args(["--max-documents", "1"])
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!path.exists());
    std::fs::remove_dir_all(root).unwrap();
}
