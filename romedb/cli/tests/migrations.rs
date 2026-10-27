use std::process::Command;
#[test]
fn cli_migrations_persist_and_detect_edited_files() {
    let root = std::env::temp_dir().join(format!(
        "romedb-migrations-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let dir = root.join("migrations");
    std::fs::create_dir_all(&dir).unwrap();
    let db = root.join("test.db");
    let first = dir.join("001_create.sql");
    std::fs::write(&first, "CREATE TABLE docs;").unwrap();
    std::fs::write(
        dir.join("002_insert.sql"),
        "INSERT INTO docs {id:docs:p1,value:1};",
    )
    .unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
            .arg("--migrate")
            .arg(&dir)
            .arg(&db)
            .output()
            .unwrap()
    };
    let output = run();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["applied"], serde_json::json!([1, 2]));
    let output = run();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["already_applied"], 2);
    std::fs::write(first, "CREATE TABLE docs; -- edited").unwrap();
    assert!(!run().status.success());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cli_rejects_non_file_migration_sources_and_allows_retry() {
    let root = std::env::temp_dir().join(format!(
        "romedb-migration-source-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let dir = root.join("migrations");
    std::fs::create_dir_all(&dir).unwrap();
    let invalid = dir.join("002_directory.sql");
    std::fs::create_dir(&invalid).unwrap();
    std::fs::write(dir.join("001_create.sql"), "CREATE TABLE docs;").unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
            .arg("--migrate")
            .arg(&dir)
            .arg(root.join("test.db"))
            .output()
            .unwrap()
    };
    let output = run();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("migration source must be a regular file"),
        "{error}"
    );
    assert!(error.contains("002_directory.sql"), "{error}");
    std::fs::remove_dir(invalid).unwrap();
    for (name, contents, diagnostic) in [
        ("002_invalid_utf8.sql", vec![0xff], "cannot read migration"),
        (
            "bad_version.sql",
            b"SELECT 1;".to_vec(),
            "invalid migration version",
        ),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        let output = run();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(diagnostic), "{error}");
        assert!(error.contains(name), "{error}");
        std::fs::remove_file(path).unwrap();
    }
    let output = run();
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["already_applied"], 0);
    assert_eq!(report["applied"], serde_json::json!([1]));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn migration_output_failure_reports_error_after_committed_apply() {
    let root = std::env::temp_dir().join(format!(
        "romedb-migration-output-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let dir = root.join("migrations");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("001_create.sql"), "CREATE TABLE docs;").unwrap();
    let db = root.join("test.db");
    let output = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
        .arg("--migrate")
        .arg(&dir)
        .arg(&db)
        .stdout(std::process::Stdio::from(
            std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        ))
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!error.contains("panicked"), "{error}");
    assert!(!error.is_empty());
    let retry = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
        .arg("--migrate")
        .arg(&dir)
        .arg(&db)
        .output()
        .unwrap();
    assert!(retry.status.success(), "{retry:?}");
    let report: serde_json::Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(report["already_applied"], 1);
    assert_eq!(report["applied"], serde_json::json!([]));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn migration_buffer_failure_persists_rollback_and_accepts_larger_policy_on_retry() {
    let root = std::env::temp_dir().join(format!("romedb-migration-buffer-{}", std::process::id()));
    let dir = root.join("migrations");
    std::fs::create_dir_all(&dir).unwrap();
    let file = root.join("test.db");
    std::fs::write(dir.join("001_initial.sql"), "CREATE TABLE docs; CREATE UNIQUE INDEX docs_n ON docs(n); INSERT INTO docs {id:docs:a,n:1};").unwrap();
    let run = |rows: &str| {
        Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
            .args(["--write-buffer-limits", rows, "1000", "--migrate"])
            .arg(&dir)
            .arg(&file)
            .output()
            .unwrap()
    };
    assert!(run("1").status.success());
    std::fs::write(
        dir.join("002_pending.sql"),
        "CREATE TABLE audit(n); INSERT INTO audit VALUES(9); INSERT INTO docs {id:docs:b,n:2};",
    )
    .unwrap();
    std::fs::write(dir.join("003_rewrite.sql"), "UPDATE docs SET n=n+10;").unwrap();
    let failed = run("1");
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    let diagnostic: serde_json::Value = serde_json::from_slice(&failed.stderr).unwrap();
    assert_eq!(diagnostic["error"]["code"], "FDB_MIGRATION");
    assert_eq!(diagnostic["error"]["migration"]["version"], 3);
    assert_eq!(diagnostic["error"]["migration"]["offset"], 0);
    assert_eq!(
        diagnostic["error"]["migration"]["cause"]["code"],
        "FDB_LIMIT"
    );
    assert!(diagnostic["error"]["message"]
        .as_str()
        .unwrap()
        .contains("resource limit"));
    assert_eq!(
        diagnostic["transaction"],
        serde_json::json!({"before":"autocommit","after":"autocommit"})
    );
    {
        let db = romedb::Database::open(file.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        let p = romedb::Parameters::new();
        assert!(c.execute("SELECT * FROM audit", &p).is_err());
        assert_eq!(
            c.execute("SELECT n FROM docs", &p).unwrap().rows,
            vec![vec![romedb::Value::Integer(1)]]
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .documents,
            1
        );
    }
    let retried = run("2");
    assert!(retried.status.success(), "{retried:?}");
    let report: serde_json::Value = serde_json::from_slice(&retried.stdout).unwrap();
    assert_eq!(report["already_applied"], 1);
    assert_eq!(report["applied"], serde_json::json!([2, 3]));
    let repeated = run("2");
    assert!(repeated.status.success());
    let report: serde_json::Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(report["already_applied"], 3);
    {
        let db = romedb::Database::open(file.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        assert_eq!(
            c.execute("SELECT n FROM docs ORDER BY n", &romedb::Parameters::new())
                .unwrap()
                .rows,
            vec![
                vec![romedb::Value::Integer(11)],
                vec![romedb::Value::Integer(12)]
            ]
        );
        assert_eq!(
            c.check_collection_integrity("docs", Default::default())
                .unwrap()
                .documents,
            2
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn migration_execution_reports_failure_and_allows_corrected_retry() {
    let root = std::env::temp_dir().join(format!(
        "romedb-cli-migration-report-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let dir = root.join("migrations");
    std::fs::create_dir_all(&dir).unwrap();
    let file = root.join("database.db");
    std::fs::write(dir.join("001_create.sql"),"CREATE TABLE docs; CREATE UNIQUE INDEX docs_n ON docs(n); INSERT INTO docs {id:docs:saved,n:1};").unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
            .arg("--migrate")
            .arg(&dir)
            .arg(&file)
            .output()
            .unwrap()
    };
    let first = run();
    assert!(first.status.success(), "{first:?}");
    let report: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(
        report["transaction"],
        serde_json::json!({"before":"autocommit","after":"autocommit"})
    );
    let pending = dir.join("002_insert.sql");
    std::fs::write(
        &pending,
        "INSERT INTO docs {id:docs:pending,n:2}; INSERT INTO docs {id:docs:bad,n:1};",
    )
    .unwrap();
    let failure = run();
    assert!(!failure.status.success());
    assert!(failure.stdout.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&failure.stderr).unwrap();
    assert_eq!(report["error"]["code"], "FDB_MIGRATION");
    assert_eq!(report["error"]["migration"]["version"], 2);
    assert_eq!(
        report["error"]["migration"]["offset"],
        "INSERT INTO docs {id:docs:pending,n:2}; ".len()
    );
    assert_eq!(
        report["error"]["migration"]["cause"]["code"],
        "FDB_CONSTRAINT"
    );
    assert!(report["error"]["message"]
        .as_str()
        .unwrap()
        .contains("migration 2 at byte"));
    assert_eq!(
        report["transaction"],
        serde_json::json!({"before":"autocommit","after":"autocommit"})
    );
    {
        let db = romedb::Database::open(file.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        assert_eq!(
            c.execute("SELECT n FROM docs", &romedb::Parameters::new())
                .unwrap()
                .rows,
            vec![vec![romedb::Value::Integer(1)]]
        );
        assert!(c
            .lookup_index("docs", "docs_n", &romedb::Value::Integer(2))
            .unwrap()
            .is_empty());
        assert_eq!(
            c.check_collection_integrity("docs", romedb::IntegrityLimits::default())
                .unwrap()
                .documents,
            1
        );
    }
    std::fs::write(&pending, "INSERT INTO docs {id:docs:pending,n:2};").unwrap();
    let retried = run();
    assert!(retried.status.success(), "{retried:?}");
    assert!(retried.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&retried.stdout).unwrap();
    assert_eq!(report["already_applied"], 1);
    assert_eq!(report["applied"], serde_json::json!([2]));
    let repeated = run();
    assert!(repeated.status.success(), "{repeated:?}");
    let report: serde_json::Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(report["already_applied"], 2);
    assert_eq!(report["applied"], serde_json::json!([]));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn migration_file_limit_precedes_utf8_decoding_and_allows_exact_limit_retry() {
    let root = std::env::temp_dir().join(format!(
        "romedb-migration-byte-boundary-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let dir = root.join("migrations");
    std::fs::create_dir_all(&dir).unwrap();
    let file = root.join("database.db");
    let source = dir.join("001_create.sql");
    let limit = 4 * 1024 * 1024;
    let mut oversized = vec![b' '; limit - 1];
    oversized.extend_from_slice("ไทย".as_bytes());
    std::fs::write(&source, &oversized).unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
            .arg("--migrate")
            .arg(&dir)
            .arg(&file)
            .output()
            .unwrap()
    };
    let failed = run();
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    let message = String::from_utf8(failed.stderr).unwrap();
    assert!(
        message.contains("migration files exceed runner limits"),
        "{message}"
    );
    assert!(message.contains("001_create.sql"));
    // An exactly-sized UTF-8 source ending in a multibyte scalar is valid.
    let mut exact = b"CREATE TABLE docs; --".to_vec();
    exact.resize(limit - "ไทย".len(), b' ');
    exact.extend_from_slice("ไทย".as_bytes());
    assert_eq!(exact.len(), limit);
    std::fs::write(&source, &exact).unwrap();
    let retry = run();
    assert!(retry.status.success(), "{retry:?}");
    let report: serde_json::Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(report["applied"], serde_json::json!([1]));
    let again = run();
    assert!(again.status.success(), "{again:?}");
    let report: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(report["already_applied"], 1);
    std::fs::remove_dir_all(root).unwrap();
}
