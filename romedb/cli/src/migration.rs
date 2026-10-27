use romedb::{Connection, Database, IntegrityLimits, Parameters, Value};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    process::ExitCode,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const MAIN: &str = "database.romedb";
const WAL: &str = "database.romedb-wal";

pub fn migrate(args: Vec<String>) -> Result<ExitCode> {
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        println!("Usage: romedb migrate-to-mvcc SOURCE OUTPUT_DIRECTORY --backup BACKUP_DIRECTORY [--max-documents N] [--max-encoded-bytes N]\nStop the application's database owner first. Only existing WAL files are accepted.\nThe source stays unchanged. Both output and backup directories must be unused.\nA private, hash-verified backup is completed before conversion.\nConversion audits documents, managed indexes, relational counts and physical integrity.\nAudit defaults: 100000 documents and 64 MiB of document bytes across all collections.\nThe converted database is OUTPUT_DIRECTORY/database.romedb.\nRecovery: romedb restore-migration BACKUP_DIRECTORY RECOVERY_DIRECTORY");
        return Ok(ExitCode::SUCCESS);
    }
    let options = Options::parse(args, true)?;
    let source = absolute(&options.paths[0])?;
    let output = absolute(&options.paths[1])?;
    let backup = absolute(
        options
            .backup
            .as_ref()
            .ok_or("migration requires --backup BACKUP_DIRECTORY")?,
    )?;
    require_unused(&output)?;
    require_unused(&backup)?;
    if backup == output || source == output || source == backup {
        return Err("source, backup and output must be different paths".into());
    }
    for sidecar in [
        source.with_extension("db-log"),
        appended(&source, "-shm"),
        appended(&source, "-log"),
        appended(&source, "-journal"),
    ] {
        require_unused(&sidecar).map_err(|_| {
            "unexpected journal or shared-memory file; recover and close its owner before migration"
        })?;
    }
    let mut source_file = secure_open(&source, true)?;
    lock_source(&source_file)?;
    fs::DirBuilder::new().mode(0o700).create(&backup)?;
    File::open(
        backup
            .parent()
            .ok_or("backup requires a parent directory")?,
    )?
    .sync_all()?;
    let mut files = Vec::new();
    files.push(copy_file(&mut source_file, &backup.join(MAIN), MAIN)?);
    let wal_path = appended(&source, "-wal");
    match fs::symlink_metadata(&wal_path) {
        Ok(_) => files.push(copy_file(
            &mut secure_open(&wal_path, false)?,
            &backup.join(WAL),
            WAL,
        )?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    File::open(&backup)?.sync_all()?;
    verify_files(&backup, &files)?;
    let staging = tempfile::Builder::new()
        .prefix(".romedb-mvcc-")
        .tempdir_in(
            output
                .parent()
                .ok_or("output requires a parent directory")?,
        )?;
    copy_snapshot(&backup, staging.path(), &files)?;
    let working = staging.path().join(MAIN);
    let database = Database::open(path_string(&working)?)?;
    let connection = database.connect()?;
    require_mode(&connection, "wal")?;
    let before = audit(&connection, options.limits)?;
    let manifest = json!({"format":"romedb.mvcc-migration-backup","version":1,"source":source,"files":files,"verified":true});
    write_private(
        &backup.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    File::open(&backup)?.sync_all()?;
    connection.execute("PRAGMA journal_mode=mvcc", &Parameters::new())?;
    require_mode(&connection, "mvcc")?;
    let after = audit(&connection, options.limits)?;
    if before != after {
        return Err("database metadata, row counts or index audit changed during conversion; source and verified backup are preserved".into());
    }
    checkpoint(&connection)?;
    drop(connection);
    drop(database);
    {
        let database = Database::open_for_service(path_string(&working)?)?;
        let connection = database.connect()?;
        require_mode(&connection, "mvcc")?;
        if audit(&connection, options.limits)? != before {
            return Err("reopened conversion differs from the verified snapshot".into());
        }
        checkpoint(&connection)?;
    }
    verify_files(&backup, &files)?;
    sync_directory_files(staging.path())?;
    publish_directory(staging.path(), &output)?;
    drop(source_file);
    println!(
        "{}",
        json!({"database":output.join(MAIN),"backup":backup,"journalMode":"mvcc","sourcePreserved":true})
    );
    Ok(ExitCode::SUCCESS)
}

pub fn restore(args: Vec<String>) -> Result<ExitCode> {
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        println!("Usage: romedb restore-migration BACKUP_DIRECTORY OUTPUT_DIRECTORY [--max-documents N] [--max-encoded-bytes N]\nRestore a verified migration backup into a new private directory.\nThe backup and any existing database stay unchanged.\nThe restored WAL database is OUTPUT_DIRECTORY/database.romedb.");
        return Ok(ExitCode::SUCCESS);
    }
    let options = Options::parse(args, false)?;
    let backup = absolute(&options.paths[0])?;
    let output = absolute(&options.paths[1])?;
    require_unused(&output)?;
    let metadata = fs::symlink_metadata(&backup)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700
    {
        return Err("backup must be an owned directory with mode 0700".into());
    }
    let mut input = Vec::new();
    secure_open(&backup.join("manifest.json"), false)?
        .take(65537)
        .read_to_end(&mut input)?;
    if input.len() > 65536 {
        return Err("backup manifest exceeds 64 KiB".into());
    }
    let manifest: serde_json::Value = serde_json::from_slice(&input)?;
    if manifest["format"] != "romedb.mvcc-migration-backup"
        || manifest["version"] != 1
        || manifest["verified"] != true
    {
        return Err("backup manifest is missing a completed verification record".into());
    }
    let files = manifest["files"]
        .as_array()
        .ok_or("backup manifest has no files")?;
    validate_files(files)?;
    verify_files(&backup, files)?;
    let staging = tempfile::Builder::new()
        .prefix(".romedb-restore-")
        .tempdir_in(
            output
                .parent()
                .ok_or("output requires a parent directory")?,
        )?;
    copy_snapshot(&backup, staging.path(), files)?;
    {
        let database = Database::open(path_string(&staging.path().join(MAIN))?)?;
        let connection = database.connect()?;
        require_mode(&connection, "wal")?;
        audit(&connection, options.limits)?;
        checkpoint(&connection)?;
    }
    sync_directory_files(staging.path())?;
    publish_directory(staging.path(), &output)?;
    println!(
        "{}",
        json!({"database":output.join(MAIN),"journalMode":"wal","backupPreserved":true})
    );
    Ok(ExitCode::SUCCESS)
}

struct Options {
    paths: Vec<PathBuf>,
    backup: Option<PathBuf>,
    limits: IntegrityLimits,
}

impl Options {
    fn parse(args: Vec<String>, migrate: bool) -> Result<Self> {
        let mut paths = Vec::new();
        let mut backup = None;
        let mut documents = None;
        let mut bytes = None;
        let mut args = args.into_iter();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--backup" if migrate && backup.is_none() => {
                    backup = Some(PathBuf::from(
                        args.next().ok_or("--backup requires a path")?,
                    ))
                }
                "--max-documents" if documents.is_none() => {
                    documents = Some(
                        args.next()
                            .ok_or("--max-documents requires a number")?
                            .parse::<u64>()?,
                    )
                }
                "--max-encoded-bytes" if bytes.is_none() => {
                    bytes = Some(
                        args.next()
                            .ok_or("--max-encoded-bytes requires a number")?
                            .parse::<u64>()?,
                    )
                }
                _ if argument.starts_with('-') => {
                    return Err("unknown or repeated migration option".into())
                }
                _ => paths.push(PathBuf::from(argument)),
            }
        }
        if paths.len() != 2 {
            return Err("migration and restore require two paths; see --help".into());
        }
        Ok(Self {
            paths,
            backup,
            limits: IntegrityLimits {
                max_documents: documents.unwrap_or(100000),
                max_encoded_bytes: bytes.unwrap_or(64 * 1024 * 1024),
            },
        })
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(parent
        .canonicalize()?
        .join(path.file_name().ok_or("path requires a final name")?))
}

fn appended(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn require_unused(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err("destination or sidecar already exists; no files were replaced".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn secure_open(path: &Path, writable: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(writable)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(
            "migration files must be owned regular files with one link; symlinks are refused"
                .into(),
        );
    }
    Ok(file)
}

fn lock_source(file: &File) -> Result<()> {
    let mut lock = libc::flock {
        l_type: libc::F_WRLCK as _,
        l_whence: libc::SEEK_SET as _,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    };
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &mut lock) } == -1 {
        return Err(
            "database is owned by another process; stop it before offline migration".into(),
        );
    }
    Ok(())
}

fn copy_file(input: &mut File, output: &Path, name: &str) -> Result<serde_json::Value> {
    input.rewind()?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(output)?;
    let mut buffer = [0u8; 65536];
    let mut bytes = 0u64;
    let mut hash = Sha256::new();
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        hash.update(&buffer[..read]);
        bytes = bytes
            .checked_add(read as u64)
            .ok_or("snapshot size overflow")?;
    }
    output.sync_all()?;
    Ok(json!({"name":name,"bytes":bytes,"sha256":format!("{:x}",hash.finalize())}))
}

fn validate_files(files: &[serde_json::Value]) -> Result<()> {
    if files.is_empty()
        || files.len() > 2
        || files[0]["name"] != MAIN
        || (files.len() == 2 && files[1]["name"] != WAL)
    {
        return Err("backup file list is invalid".into());
    }
    for file in files {
        if file["bytes"].as_u64().is_none()
            || !file["sha256"].as_str().is_some_and(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err("backup fingerprint is invalid".into());
        }
    }
    Ok(())
}

fn verify_files(directory: &Path, files: &[serde_json::Value]) -> Result<()> {
    validate_files(files)?;
    for expected in files {
        let mut file = secure_open(
            &directory.join(expected["name"].as_str().ok_or("invalid backup name")?),
            false,
        )?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        let mut bytes = 0u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
            bytes += read as u64;
        }
        if expected["bytes"].as_u64() != Some(bytes)
            || expected["sha256"].as_str() != Some(format!("{:x}", hash.finalize()).as_str())
        {
            return Err("backup fingerprint differs; no database was published".into());
        }
    }
    Ok(())
}

fn copy_snapshot(source: &Path, target: &Path, files: &[serde_json::Value]) -> Result<()> {
    for expected in files {
        let name = expected["name"].as_str().ok_or("invalid backup name")?;
        let actual = copy_file(
            &mut secure_open(&source.join(name), false)?,
            &target.join(name),
            name,
        )?;
        if &actual != expected {
            return Err("backup changed while copying; no database was published".into());
        }
    }
    Ok(())
}

fn path_string(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| "database paths must be UTF-8".into())
}

fn require_mode(connection: &Connection, mode: &str) -> Result<()> {
    if connection
        .execute("PRAGMA journal_mode", &Parameters::new())?
        .rows
        != vec![vec![Value::String(mode.into())]]
    {
        return Err(format!("database must be in {mode} mode").into());
    }
    Ok(())
}

fn audit(connection: &Connection, limits: IntegrityLimits) -> Result<serde_json::Value> {
    if connection
        .execute("PRAGMA integrity_check", &Parameters::new())?
        .rows
        != vec![vec![Value::String("ok".into())]]
    {
        return Err("physical database integrity check failed".into());
    }
    let metadata = connection.info("db", None)?;
    let [row] = metadata.rows.as_slice() else {
        return Err("database metadata is invalid".into());
    };
    let [Value::Object(info)] = row.as_slice() else {
        return Err("database metadata is invalid".into());
    };
    let Some(Value::Array(tables)) = info.get("tables") else {
        return Err("database metadata has no tables".into());
    };
    let mut remaining = limits;
    let mut checks = Vec::new();
    for table in tables {
        let Value::Object(table) = table else {
            return Err("invalid table metadata".into());
        };
        let Some(Value::String(name)) = table.get("name") else {
            return Err("invalid table name".into());
        };
        let table_metadata = connection.info("table", Some(name))?;
        let check = if table.get("model") == Some(&Value::String("document".into())) {
            let report = connection.check_collection_integrity(name, remaining)?;
            remaining.max_documents = remaining
                .max_documents
                .checked_sub(report.documents)
                .ok_or("document audit budget exceeded")?;
            remaining.max_encoded_bytes = remaining
                .max_encoded_bytes
                .checked_sub(report.encoded_bytes)
                .ok_or("document audit byte budget exceeded")?;
            json!({"table":name,"metadata":table_metadata.rows,"integrity":report})
        } else {
            let quoted = format!("\"{}\"", name.replace('"', "\"\""));
            let count = connection.execute(
                &format!("SELECT count(*) FROM {quoted}"),
                &Parameters::new(),
            )?;
            json!({"table":name,"metadata":table_metadata.rows,"count":count.rows})
        };
        checks.push(check);
    }
    Ok(json!({"metadata":metadata.rows,"tables":checks}))
}

fn checkpoint(connection: &Connection) -> Result<()> {
    let result = connection.execute("PRAGMA wal_checkpoint(TRUNCATE)", &Parameters::new())?;
    if result.rows
        != vec![vec![
            Value::Integer(0),
            Value::Integer(0),
            Value::Integer(0),
        ]]
    {
        return Err("database checkpoint did not complete; no output was published".into());
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn sync_directory_files(directory: &Path) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        secure_open(&entry?.path(), false)?.sync_all()?;
    }
    File::open(directory)?.sync_all()?;
    Ok(())
}

fn publish_directory(staging: &Path, output: &Path) -> Result<()> {
    let from = CString::new(staging.as_os_str().as_bytes())?;
    let to = CString::new(output.as_os_str().as_bytes())?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } == -1
    {
        return Err(std::io::Error::last_os_error().into());
    }
    File::open(
        output
            .parent()
            .ok_or("output requires a parent directory")?,
    )?
    .sync_all()?;
    Ok(())
}
