use std::{
    io::Write,
    process::{Command, Stdio},
};
fn run(args: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}
#[test]
fn cli_exports_and_imports_persistent_collections() {
    let dir = std::env::temp_dir().join(format!(
        "romedb-transfer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let a = dir.join("a.db");
    let b = dir.join("b.db");
    let a = a.to_str().unwrap();
    let b = b.to_str().unwrap();
    assert!(run(
        &[a],
        b"CREATE TABLE docs; INSERT INTO docs {id:docs:p1,value:9223372036854775807};"
    )
    .status
    .success());
    assert!(run(&[b], b"CREATE TABLE docs;").status.success());
    let export = run(&["--export", "docs", "--ndjson", a], b"");
    assert!(export.status.success(), "{export:?}");
    let import = run(&["--import", "docs", "--ndjson", b], &export.stdout);
    assert!(import.status.success(), "{import:?}");
    assert_eq!(
        run(&["--export", "docs", "--ndjson", b], b"").stdout,
        export.stdout
    );
    assert!(!run(&["--import", "docs", "--ndjson", b], &export.stdout)
        .status
        .success());
    assert_eq!(
        run(&["--export", "docs", "--ndjson", b], b"").stdout,
        export.stdout
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn transfer_output_failures_preserve_committed_data_in_both_formats() {
    fn fail_output(args: &[&str], input: &[u8]) -> std::process::Output {
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(full))
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    }
    let dir = std::env::temp_dir().join(format!(
        "romedb-transfer-output-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let source = dir.join("source.db");
    let source = source.to_str().unwrap();
    assert!(run(
        &[source],
        b"CREATE TABLE docs; INSERT INTO docs {id:docs:first,n:1};"
    )
    .status
    .success());
    for ndjson in [false, true] {
        let target = dir.join(if ndjson { "ndjson.db" } else { "json.db" });
        let target = target.to_str().unwrap();
        assert!(run(&[target], b"CREATE TABLE docs;").status.success());
        let mut export_args = vec!["--export", "docs", source];
        let mut import_args = vec!["--import", "docs", target];
        let mut target_args = vec!["--export", "docs", target];
        if ndjson {
            export_args.push("--ndjson");
            import_args.push("--ndjson");
            target_args.push("--ndjson");
        }
        let exported = run(&export_args, b"");
        assert!(exported.status.success(), "{exported:?}");
        for failed in [
            fail_output(&export_args, b""),
            fail_output(&import_args, &exported.stdout),
        ] {
            assert!(!failed.status.success());
            let message = String::from_utf8_lossy(&failed.stderr);
            assert!(!message.is_empty());
            assert!(!message.contains("panicked"), "{message}");
        }
        let actual = run(&target_args, b"");
        assert!(actual.status.success(), "{actual:?}");
        assert_eq!(actual.stdout, exported.stdout);
        assert!(!run(&import_args, &exported.stdout).status.success());
        assert_eq!(run(&target_args, b"").stdout, exported.stdout);
        assert_eq!(run(&export_args, b"").stdout, exported.stdout);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn maximum_depth_documents_survive_cli_transfer_and_query_processes() {
    let dir = std::env::temp_dir().join(format!(
        "romedb-cli-deep-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let source = dir.join("source.db");
    let mut value = romedb::Value::String("quoted\"\\\nไทย".into());
    for depth in 0..63 {
        value = if depth % 2 == 0 {
            romedb::Value::Array(vec![value])
        } else {
            romedb::Value::Object([("nested".into(), value)].into())
        };
    }
    {
        let db = romedb::Database::open(source.to_str().unwrap()).unwrap();
        let conn = db.connect().unwrap();
        conn.execute("CREATE TABLE docs", &romedb::Parameters::new())
            .unwrap();
        conn.insert("docs", [("payload".into(), value.clone())].into())
            .unwrap();
    }
    for ndjson in [false, true] {
        let target = dir.join(if ndjson { "lines.db" } else { "json.db" });
        let target = target.to_str().unwrap();
        assert!(run(&[target], b"CREATE TABLE docs;").status.success());
        let mut source_args = vec!["--export", "docs", source.to_str().unwrap()];
        let mut import_args = vec!["--import", "docs", target];
        let mut target_args = vec!["--export", "docs", target];
        if ndjson {
            source_args.push("--ndjson");
            import_args.push("--ndjson");
            target_args.push("--ndjson");
        }
        let exported = run(&source_args, b"");
        assert!(exported.status.success(), "{exported:?}");
        let imported = run(&import_args, &exported.stdout);
        assert!(imported.status.success(), "{imported:?}");
        let reopened = run(&target_args, b"");
        assert!(reopened.status.success(), "{reopened:?}");
        assert_eq!(reopened.stdout, exported.stdout);
        let queried = run(&[target], b"SELECT payload FROM docs;");
        assert!(queried.status.success(), "{queried:?}");
        // The tagged result exceeds serde's default text recursion bound.
        let report: serde_json::Value =
            romedb::decode_wire_json(std::str::from_utf8(&queried.stdout).unwrap()).unwrap();
        assert_eq!(
            report["rows"],
            serde_json::to_value(vec![vec![value.clone()]]).unwrap()
        );
        assert_eq!(report["transaction"]["after"], "autocommit");
        let duplicate = run(&import_args, &exported.stdout);
        assert!(!duplicate.status.success());
        assert_eq!(run(&target_args, b"").stdout, exported.stdout);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn transfer_operation_failures_report_codes_without_polluting_data_output() {
    let dir = std::env::temp_dir().join(format!(
        "romedb-cli-transfer-errors-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let file = dir.join("database.db");
    let file = file.to_str().unwrap();
    assert!(run(
        &[file],
        b"CREATE TABLE docs; INSERT INTO docs {id:docs:saved,n:1};"
    )
    .status
    .success());
    for ndjson in [false, true] {
        let mut export = vec!["--export", "docs", file];
        let mut import = vec!["--import", "docs", file];
        let mut missing = vec!["--export", "absent", file];
        if ndjson {
            export.push("--ndjson");
            import.push("--ndjson");
            missing.push("--ndjson");
        }
        let original = run(&export, b"");
        assert!(original.status.success());
        for (output, code) in [
            (run(&import, &original.stdout), "FDB_CONSTRAINT"),
            (run(&import, b"invalid input"), "FDB_VALIDATION"),
            (run(&import, &[0xff]), "FDB_VALIDATION"),
            (run(&missing, b""), "FDB_NOT_FOUND"),
        ] {
            assert!(!output.status.success());
            assert!(
                output.stdout.is_empty(),
                "transfer errors must not become data"
            );
            let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(diagnostic["error"]["code"], code);
            assert!(diagnostic["error"]["message"].as_str().unwrap().len() > 1);
            assert_eq!(
                diagnostic["transaction"],
                serde_json::json!({"before":"autocommit","after":"autocommit"})
            );
            assert_eq!(run(&export, b"").stdout, original.stdout);
        }
        assert!(run(&[file], b"DELETE FROM docs;").status.success());
        let success = run(&import, &original.stdout);
        assert!(success.status.success(), "{success:?}");
        assert!(success.stderr.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&success.stdout).unwrap();
        assert_eq!(report["imported"], 1);
        assert_eq!(
            report["transaction"],
            serde_json::json!({"before":"autocommit","after":"autocommit"})
        );
        assert_eq!(run(&export, b"").stdout, original.stdout);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn actual_import_byte_limit_accepts_exact_input_and_rejects_split_utf8_overflow() {
    let root = std::env::temp_dir().join(format!(
        "romedb-cli-import-cap-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let database = root.join("database.db");
    let database = database.to_str().unwrap();
    let input = root.join("input.json");
    assert!(run(
        &[database],
        b"CREATE TABLE docs; INSERT INTO docs {id:docs:saved,n:1};"
    )
    .status
    .success());
    let limit = 64 * 1024 * 1024;
    for ndjson in [false, true] {
        let prefix: &[u8] = if ndjson {
            b"{\"format\":\"romedb.documents\",\"version\":1}"
        } else {
            b"{\"header\":{\"format\":\"romedb.documents\",\"version\":1},\"documents\":[]}"
        };
        let mut args = vec!["--import", "docs", database];
        if ndjson {
            args.push("--ndjson");
        }
        for overflow in [true, false] {
            {
                let mut writer = std::io::BufWriter::new(std::fs::File::create(&input).unwrap());
                writer.write_all(prefix).unwrap();
                let mut remaining = limit - 1 - prefix.len();
                let spaces = [b' '; 65536];
                while remaining > 0 {
                    let count = remaining.min(spaces.len());
                    writer.write_all(&spaces[..count]).unwrap();
                    remaining -= count;
                }
                writer
                    .write_all(if overflow {
                        "ไทย".as_bytes()
                    } else {
                        b"\n"
                    })
                    .unwrap();
                writer.flush().unwrap();
            }
            let output = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
                .args(&args)
                .stdin(Stdio::from(std::fs::File::open(&input).unwrap()))
                .output()
                .unwrap();
            if overflow {
                assert!(!output.status.success());
                assert!(output.stdout.is_empty());
                let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
                assert_eq!(error["error"]["code"], "FDB_LIMIT");
                assert_eq!(
                    error["transaction"],
                    serde_json::json!({"before":"autocommit","after":"autocommit"})
                );
            } else {
                assert_eq!(std::fs::metadata(&input).unwrap().len(), limit as u64);
                assert!(output.status.success(), "{output:?}");
                assert!(output.stderr.is_empty());
                let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(report["imported"], 0);
            }
            let data = run(&[database], b"SELECT n FROM docs;");
            assert!(data.status.success());
            let report: serde_json::Value = serde_json::from_slice(&data.stdout).unwrap();
            assert_eq!(
                report["rows"],
                serde_json::json!([[{"type":"Integer","value":1}]])
            );
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}
