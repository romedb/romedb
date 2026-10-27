//! Source-preserving SQLite adoption. Only a private backup is opened by RomeDB.
use romedb::{Database, Parameters, Value};
use rusqlite::{backup::Backup, backup::StepResult, Connection, OpenFlags};
use serde_json::json;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Object {
    kind: String,
    name: String,
    sql: Option<String>,
}

pub fn run(args: Vec<String>) -> Result<ExitCode> {
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        println!("Usage: romedb-cli sqlite check SOURCE\n       romedb-cli sqlite import SOURCE DESTINATION\nCheck a consistent SQLite snapshot, or adopt it into a new RomeDB file.\nThe source is opened read-only. Existing destinations and sidecars are refused.\nTables remain relational; this does not convert rows into document collections.");
        return Ok(ExitCode::SUCCESS);
    }
    let import = args.first().map(String::as_str) == Some("import");
    if !(import && args.len() == 3
        || args.first().map(String::as_str) == Some("check") && args.len() == 2)
    {
        return Err(
            "usage: romedb-cli sqlite check SOURCE | sqlite import SOURCE DESTINATION".into(),
        );
    }
    let source = Path::new(&args[1]);
    if !fs::metadata(source)?.is_file() || fs::metadata(source)?.len() < 100 {
        return Err("source must be an existing nonempty SQLite database file".into());
    }
    let destination = if import {
        let requested = Path::new(&args[2]);
        let parent = requested
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let target = fs::canonicalize(parent)?.join(
            requested
                .file_name()
                .ok_or("destination needs a file name")?,
        );
        require_unused(&target)?;
        Some(target)
    } else {
        None
    };
    // Private directory, same filesystem as the destination for no-clobber publication.
    let staging = match &destination {
        Some(path) => tempfile::Builder::new()
            .prefix(".romedb-sqlite-")
            .tempdir_in(path.parent().unwrap())?,
        None => tempfile::Builder::new()
            .prefix("romedb-sqlite-")
            .tempdir()?,
    };
    let snapshot = staging.path().join("snapshot.db");
    snapshot_source(source, &snapshot)?;
    let (objects, mut unsupported) = inspect(&snapshot)?;
    if unsupported.is_empty() {
        if let Err(error) = validate_schema(&objects) {
            unsupported.push(error.to_string());
        }
    }
    if unsupported.is_empty() {
        // Check mode exercises the same initialization and physical checks as import.
        if let Err(error) = initialize(&snapshot, &objects) {
            unsupported.push(format!("RomeDB snapshot validation: {error}"));
        }
    }
    let supported = unsupported.is_empty();
    let mut report = json!({
        "operation": if import { "import" } else { "check" },
        "source": source,
        "compatible": supported,
        "sqlite_version": rusqlite::version(),
        "objects": objects.iter().map(|o| json!({"type":o.kind,"name":o.name})).collect::<Vec<_>>(),
        "unsupported": unsupported,
        "notes": [
            "Tables remain relational; JSON text and ordinary primary keys retain SQLite types.",
            "Enable PRAGMA foreign_keys=ON on every application connection if required.",
            "Preflight checks schema and integrity, not every application query or trigger execution.",
            "Use one owning RomeDB process per file; do not share a live file with SQLite."
        ]
    });
    if supported {
        if let Some(target) = &destination {
            // Both engines have closed. The successful TRUNCATE checkpoint and SQLite
            // verification below ensure publication needs no staging WAL sidecar.
            require_unused(target)?;
            File::open(&snapshot)?.sync_all()?;
            fs::hard_link(&snapshot, target)?; // Atomic, refuses replacement (including symlinks).
                                               // After link succeeds the file is complete. A directory-sync failure is
                                               // an ambiguous publication, so retain it and identify it in the error.
            File::open(target.parent().unwrap())?.sync_all().map_err(|error| {
                format!("complete destination exists at {}; directory sync failed: {error}; inspect before retrying", target.display())
            })?;
            report["destination"] = json!(target);
        }
    }
    serde_json::to_writer(std::io::stdout().lock(), &report)?;
    writeln!(std::io::stdout().lock())?;
    Ok(if supported {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

fn require_unused(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let candidate = sidecar(path, suffix);
        match fs::symlink_metadata(&candidate) {
            Ok(_) => {
                return Err(format!(
                    "destination or sidecar already exists: {}",
                    candidate.display()
                )
                .into())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn sqlite_readonly(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA trusted_schema=OFF;")?;
    Ok(conn)
}

fn snapshot_source(source: &Path, snapshot: &Path) -> Result<()> {
    let from = sqlite_readonly(source)?;
    // Pin one read snapshot across backup steps, including committed WAL frames.
    from.execute_batch("BEGIN;")?;
    from.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))?;
    let mut create = fs::OpenOptions::new();
    create.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        create.mode(0o600);
    }
    drop(create.open(snapshot)?);
    let mut to = Connection::open(snapshot)?;
    to.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    to.execute_batch("PRAGMA trusted_schema=OFF;")?;
    {
        let backup = Backup::new(&from, &mut to)?;
        let mut busy_since = None;
        loop {
            let attempt_started = Instant::now();
            match backup.step(256)? {
                StepResult::Done => break,
                StepResult::More => busy_since = None,
                StepResult::Busy | StepResult::Locked => {
                    let start = busy_since.get_or_insert(attempt_started);
                    if start.elapsed() >= Duration::from_secs(5) {
                        return Err("SQLite snapshot remained locked for five seconds; retry when available".into());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => return Err("unrecognized SQLite backup status".into()),
            }
        }
    }
    from.execute_batch("ROLLBACK;")?;
    to.execute_batch("PRAGMA journal_mode=DELETE;")?;
    to.close().map_err(|(_, e)| e)?;
    Ok(())
}

fn inspect(path: &Path) -> Result<(Vec<Object>, Vec<String>)> {
    let conn = sqlite_readonly(path)?;
    let mut objects = Vec::new();
    let mut stmt = conn.prepare("SELECT type,name,sql FROM sqlite_schema ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'index' THEN 1 WHEN 'view' THEN 2 ELSE 3 END,rowid")?;
    for object in stmt.query_map([], |row| {
        Ok(Object {
            kind: row.get(0)?,
            name: row.get(1)?,
            sql: row.get(2)?,
        })
    })? {
        objects.push(object?);
    }
    let mut reasons = Vec::new();
    let encoding: String = conn.query_row("PRAGMA encoding", [], |row| row.get(0))?;
    if encoding != "UTF-8" {
        reasons.push(format!(
            "encoding {encoding}: only UTF-8 databases are supported"
        ));
    }
    let auto_vacuum: i64 = conn.query_row("PRAGMA auto_vacuum", [], |row| row.get(0))?;
    if auto_vacuum != 0 {
        reasons.push("auto_vacuum databases are unsupported; rebuild a separate SQLite copy with auto_vacuum=NONE before adoption".into());
    }
    for object in &objects {
        let name = object.name.to_ascii_lowercase();
        if name.starts_with("__romedb_") || name.starts_with("__turso_internal_") {
            reasons.push(format!(
                "reserved object name: {} (use ordinary open for an existing RomeDB database)",
                object.name
            ));
        }
    }
    let mut tables = conn.prepare("PRAGMA table_list")?;
    for table in tables.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(4)?,
        ))
    })? {
        let (schema, name, kind, without_rowid) = table?;
        if schema != "main" || name.starts_with("sqlite_") {
            continue;
        }
        if kind == "virtual" || kind == "shadow" {
            reasons.push(format!("{name}: SQLite {kind} tables are unsupported; recreate search indexes using RomeDB"));
        }
        if without_rowid != 0 {
            reasons.push(format!(
                "{name}: WITHOUT ROWID is unsupported for adoption (UPDATE/DELETE are incomplete)"
            ));
        }
        if kind != "table" {
            continue;
        }
        let generated: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_xinfo(?1) WHERE hidden IN (2,3)",
            [&name],
            |row| row.get(0),
        )?;
        if generated != 0 {
            reasons.push(format!("{name}: generated columns are unsupported"));
        }
    }
    // Unsupported extensions/virtual schemas may make integrity_check impossible.
    // Report those incompatibilities first rather than pretending they were omitted.
    if reasons.is_empty() {
        sqlite_integrity(&conn)?;
    }
    Ok((objects, reasons))
}

fn sqlite_integrity(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA integrity_check(1)")?;
    let results = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if results != ["ok"] {
        return Err(format!("SQLite integrity check failed: {results:?}").into());
    }
    Ok(())
}

fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn validate_schema(objects: &[Object]) -> Result<()> {
    let db = Database::open(":memory:")?;
    let conn = db.connect()?;
    for object in objects {
        if object.name.starts_with("sqlite_") {
            continue;
        }
        if let Some(sql) = &object.sql {
            conn.execute(sql, &Parameters::new()).map_err(|e| {
                format!(
                    "{} {}: schema unsupported by RomeDB: {e}",
                    object.kind, object.name
                )
            })?;
        }
    }
    Ok(())
}

fn initialize(path: &Path, objects: &[Object]) -> Result<()> {
    {
        let db = Database::open(path.to_str().ok_or("snapshot path must be UTF-8")?)?;
        let conn = db.connect()?;
        for object in objects.iter().filter(|o| {
            matches!(o.kind.as_str(), "table" | "view") && !o.name.starts_with("sqlite_")
        }) {
            conn.execute(
                &format!("SELECT * FROM {} LIMIT 0", quoted(&object.name)),
                &Parameters::new(),
            )
            .map_err(|e| format!("{} {} cannot be queried: {e}", object.kind, object.name))?;
        }
        let integrity = conn.execute("PRAGMA integrity_check", &Parameters::new())?;
        if integrity.rows != vec![vec![Value::String("ok".into())]] {
            return Err(format!("RomeDB integrity check failed: {:?}", integrity.rows).into());
        }
        let checkpoint = conn.execute("PRAGMA wal_checkpoint(TRUNCATE)", &Parameters::new())?;
        if checkpoint.rows
            != vec![vec![
                Value::Integer(0),
                Value::Integer(0),
                Value::Integer(0),
            ]]
        {
            return Err(format!("RomeDB checkpoint incomplete: {:?}", checkpoint.rows).into());
        }
    }
    if fs::metadata(sidecar(path, "-wal")).is_ok_and(|m| m.len() != 0) {
        return Err("snapshot still has WAL data after checkpoint".into());
    }
    sqlite_integrity(&sqlite_readonly(path)?)?;
    Ok(())
}
