#[cfg(unix)]
#[test]
fn cloud_http_management_and_retry_contract() {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/check-cloud-cli.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_romedb-cli"))
        .output()
        .expect("run generic cloud HTTP harness");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn cloud_import_journal_and_resume_contract() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../scripts/check-cloud-import-cli.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_romedb-cli"))
        .output()
        .expect("run import client HTTP harness");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
