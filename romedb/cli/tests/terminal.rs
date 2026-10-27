#[cfg(unix)]
#[test]
fn terminal_editing_history_and_prompt_interruptions() {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/check-cli-terminal.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_romedb-cli"))
        .output()
        .expect("run terminal harness");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
