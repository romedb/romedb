mod cloud;
mod input;
#[cfg(target_os = "linux")]
mod migration;
#[cfg(unix)]
mod service;
#[cfg(unix)]
mod signals;
mod sqlite;
use romedb::{Database, ExecutionReport, Parameters, TransferFormat};
use std::io::{self, BufRead, IsTerminal, Read, Write};
fn output(
    writer: &mut impl Write,
    report: ExecutionReport,
    offset: Option<usize>,
) -> Result<bool, Box<dyn std::error::Error>> {
    output_with_metrics(writer, report, offset, None)
}
fn output_with_metrics(
    writer: &mut impl Write,
    report: ExecutionReport,
    offset: Option<usize>,
    metrics: Option<romedb::QueryMetrics>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let failed = report.result.is_err();
    writer.write_all(b"{")?;
    match report.result {
        Ok(result) => {
            writer.write_all(b"\"affected\":")?;
            serde_json::to_writer(&mut *writer, &result.affected).map_err(io::Error::from)?;
            writer.write_all(b",\"columns\":")?;
            serde_json::to_writer(&mut *writer, &result.columns).map_err(io::Error::from)?;
            writer.write_all(b",\"rows\":")?;
            serde_json::to_writer(&mut *writer, &result.rows).map_err(io::Error::from)?;
        }
        Err(error) => {
            writer.write_all(b"\"error\":")?;
            serde_json::to_writer(
                &mut *writer,
                &serde_json::json!({"code": error.code(), "message": error.to_string()}),
            )
            .map_err(io::Error::from)?;
        }
    }
    writer.write_all(b",\"transaction\":")?;
    serde_json::to_writer(&mut *writer,
        &serde_json::json!({"before": report.transaction_before, "after": report.transaction_after}))
        .map_err(io::Error::from)?;
    if let Some(metrics) = metrics {
        writer.write_all(b",\"profile\":")?;
        serde_json::to_writer(&mut *writer, &metrics).map_err(io::Error::from)?;
    }
    if let Some(offset) = offset {
        writer.write_all(b",\"offset\":")?;
        serde_json::to_writer(&mut *writer, &offset).map_err(io::Error::from)?;
    }
    writer.write_all(b"}\n")?;
    writer.flush()?;
    Ok(failed)
}
fn main() -> Result<std::process::ExitCode, Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    match std::env::args().nth(1).as_deref() {
        Some("migrate-to-mvcc") => return migration::migrate(std::env::args().skip(2).collect()),
        Some("restore-migration") => return migration::restore(std::env::args().skip(2).collect()),
        _ => {}
    }
    #[cfg(unix)]
    if std::env::args().nth(1).as_deref() == Some("serve") {
        return service::run(std::env::args().skip(2).collect());
    }
    if std::env::args().nth(1).as_deref() == Some("sqlite") {
        return sqlite::run(std::env::args().skip(2).collect());
    }
    if std::env::args().nth(1).as_deref() == Some("cloud") {
        return cloud::run(std::env::args().skip(2).collect());
    }
    let mut path = None;
    let mut input_limit = None;
    let mut write_buffer_limits = None;
    let mut line_mode = false;
    let mut interactive = false;
    let mut script_mode = false;
    let mut history = None;
    let mut transfer = None;
    let mut migrations = None;
    let mut audit = None;
    let mut audit_documents = None;
    let mut audit_bytes = None;
    let mut format = TransferFormat::Json;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--write-buffer-limits" => {
                if write_buffer_limits.is_some() {
                    return Err("choose one write buffer policy".into());
                }
                write_buffer_limits = Some(romedb::ResultLimits {
                    max_rows: args
                        .next()
                        .ok_or("expected write buffer row limit")?
                        .parse::<usize>()?,
                    max_payload_bytes: args
                        .next()
                        .ok_or("expected write buffer byte limit")?
                        .parse::<usize>()?,
                });
            }
            "--max-input-bytes" => {
                let limit = args
                    .next()
                    .ok_or("expected input byte limit")?
                    .parse::<usize>()?;
                if limit == 0 || limit.checked_add(1).is_none() {
                    return Err("input byte limit must be positive and below usize::MAX".into());
                }
                input_limit = Some(limit);
            }
            "--check-collection" => {
                if audit.is_some() {
                    return Err("choose one collection audit".into());
                }
                audit = Some(args.next().ok_or("expected collection name")?);
            }
            "--max-documents" => {
                audit_documents = Some(
                    args.next()
                        .ok_or("expected document limit")?
                        .parse::<u64>()?,
                )
            }
            "--max-encoded-bytes" => {
                audit_bytes = Some(
                    args.next()
                        .ok_or("expected encoded-byte limit")?
                        .parse::<u64>()?,
                )
            }
            "--migrate" => {
                if migrations.is_some() {
                    return Err("choose one migration directory".into());
                }
                migrations = Some(args.next().ok_or("expected migration directory")?);
            }
            "--history" => {
                if history.is_some() {
                    return Err("choose one history path".into());
                }
                history = Some(std::path::PathBuf::from(
                    args.next().ok_or("expected history path")?,
                ));
            }
            "--line" => line_mode = true,
            "--interactive" => interactive = true,
            "--script" => script_mode = true,
            "--ndjson" => format = TransferFormat::Ndjson,
            "--import" | "--export" => {
                if transfer.is_some() {
                    return Err("choose one import/export operation".into());
                }
                transfer = Some((
                    arg == "--import",
                    args.next().ok_or("expected collection name")?,
                ));
            }
            "--version" | "-V" => {
                writeln!(io::stdout().lock(), "romedb {}", romedb::VERSION)?;
                return Ok(std::process::ExitCode::SUCCESS);
            }
            "--help" | "-h" => {
                writeln!(io::stdout().lock(), "Usage: romedb-cli [--interactive | --script | --line] [--max-input-bytes N] [--history PATH] [DATABASE]\n       romedb-cli --version\n       romedb-cli --migrate DIRECTORY [DATABASE]\n       romedb-cli (--import COLLECTION | --export COLLECTION) [--ndjson] [DATABASE]\n       romedb-cli --check-collection COLLECTION [--max-documents N] [--max-encoded-bytes N] DATABASE\nSQLite adoption: romedb-cli sqlite check SOURCE | sqlite import SOURCE DESTINATION\nTerminal input opens an interactive prompt; piped input runs a script.\n--script reads through EOF and stops on the first error.\n--interactive accepts multiline statements and .help, .clear, .quit.\nUnix terminals support line editing and in-memory history; --history PATH saves history.\nCtrl-C clears pending input at the prompt or requests cancellation of running engine work.\n--line retains one-statement-per-line execution and continues after errors.\n--write-buffer-limits ROWS BYTES caps each frontend collection-write buffer for SQL input and migrations.\nInput buffers default to 16 MiB; --max-input-bytes changes this byte limit.\n.select-limit ROWS BYTES SELECT ... and .profile-limit ROWS BYTES SELECT ... bound returned results.\n.timeout MILLISECONDS SQL requests cooperative cancellation at its deadline.\n.write-limit ROWS BYTES SQL checks write results atomically; these are not process memory caps.")?;
                #[cfg(unix)]
                writeln!(io::stdout().lock(), "Local service: romedb serve DATABASE [--port PORT] [--token-file PATH] [--create]\nUse romedb serve --help for MVCC ownership and credential details.")?;
                #[cfg(target_os = "linux")]
                writeln!(io::stdout().lock(), "Offline migration: romedb migrate-to-mvcc SOURCE OUTPUT_DIRECTORY --backup BACKUP_DIRECTORY\nRecovery: romedb restore-migration BACKUP_DIRECTORY OUTPUT_DIRECTORY")?;
                io::stdout().lock().flush()?;
                return Ok(std::process::ExitCode::SUCCESS);
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option {arg}").into()),
            _ if path.is_none() => path = Some(arg),
            _ => return Err("expected one database path".into()),
        }
    }
    if usize::from(line_mode) + usize::from(interactive) + usize::from(script_mode) > 1 {
        return Err("choose one input mode".into());
    }
    if (interactive || script_mode) && (migrations.is_some() || transfer.is_some()) {
        return Err("input modes cannot be combined with migration/import/export".into());
    }
    if migrations.is_some()
        && (line_mode || transfer.is_some() || matches!(format, TransferFormat::Ndjson))
    {
        return Err("--migrate cannot be combined with script or transfer options".into());
    }
    if line_mode && transfer.is_some() {
        return Err("--line cannot be combined with import/export".into());
    }
    if transfer.is_none() && matches!(format, TransferFormat::Ndjson) {
        return Err("--ndjson requires import/export".into());
    }
    if input_limit.is_some() && (migrations.is_some() || transfer.is_some()) {
        return Err("--max-input-bytes applies only to SQL input modes".into());
    }
    if audit.is_none() && (audit_documents.is_some() || audit_bytes.is_some()) {
        return Err("audit limits require --check-collection".into());
    }
    if let Some(table) = audit {
        if line_mode
            || interactive
            || script_mode
            || migrations.is_some()
            || transfer.is_some()
            || history.is_some()
            || input_limit.is_some()
            || write_buffer_limits.is_some()
        {
            return Err("collection audit cannot be combined with input, history, migration or transfer options".into());
        }
        let path = path
            .as_deref()
            .ok_or("collection audit requires a database path")?;
        let mut limits = romedb::IntegrityLimits::default();
        if let Some(value) = audit_documents {
            limits.max_documents = value;
        }
        if let Some(value) = audit_bytes {
            limits.max_encoded_bytes = value;
        }
        return run_audit(path, &table, limits);
    }
    if write_buffer_limits.is_some() && transfer.is_some() {
        return Err("write buffer limits apply only to SQL input and migrations".into());
    }
    let interactive_mode = interactive || (!line_mode && !script_mode && io::stdin().is_terminal());
    let terminal = interactive_mode
        && migrations.is_none()
        && transfer.is_none()
        && io::stdin().is_terminal()
        && io::stderr().is_terminal()
        && input::terminal_available();
    if history.is_some() && !terminal {
        return Err("--history requires interactive terminal input and terminal stderr".into());
    }
    let mut editor = if terminal {
        Some(input::Terminal::new(
            history.as_deref(),
            std::path::Path::new(path.as_deref().unwrap_or(":memory:")),
        )?)
    } else {
        None
    };
    let input_limit = input_limit.unwrap_or(16 * 1024 * 1024);
    let db = Database::open(path.as_deref().unwrap_or(":memory:"))?;
    let conn = db.connect()?;
    let conn = if let Some(limits) = write_buffer_limits {
        conn.with_write_buffer_limits(limits)
    } else {
        conn
    };
    #[cfg(unix)]
    let _interrupts = if terminal {
        Some(signals::Interrupts::new(conn.interrupt_handle())?)
    } else {
        None
    };
    if let Some(directory) = migrations {
        let plan = migration_plan(&directory)?;
        let before = conn.transaction_state();
        let report = match conn.migrate(&plan) {
            Ok(report) => report,
            Err(error) => {
                operation_error(
                    &mut io::stderr().lock(),
                    &error,
                    before,
                    conn.transaction_state(),
                )?;
                return Ok(std::process::ExitCode::FAILURE);
            }
        };
        writeln!(
            io::stdout().lock(),
            "{}",
            serde_json::json!({"applied":report.applied,"already_applied":report.already_applied,
                "transaction":{"before":before,"after":conn.transaction_state()}})
        )?;
        io::stdout().lock().flush()?;
        return Ok(std::process::ExitCode::SUCCESS);
    }
    if let Some((import, table)) = transfer {
        let before = conn.transaction_state();
        let result = if import {
            let limit = 64 * 1024 * 1024;
            let input = read_input(&mut io::stdin().lock(), limit, false)?;
            transfer_text(input, limit)
                .and_then(|input| conn.import_documents(&table, &input, format))
                .map(|count| {
                    format!(
                        "{}\n",
                        serde_json::json!({"imported":count,
                    "transaction":{"before":before,"after":conn.transaction_state()}})
                    )
                })
        } else {
            conn.export_documents(&table, format)
        };
        let data = match result {
            Ok(data) => data,
            Err(error) => {
                operation_error(
                    &mut io::stderr().lock(),
                    &error,
                    before,
                    conn.transaction_state(),
                )?;
                return Ok(std::process::ExitCode::FAILURE);
            }
        };
        io::stdout().lock().write_all(data.as_bytes())?;
        io::stdout().lock().flush()?;
        return Ok(std::process::ExitCode::SUCCESS);
    }
    let mut writer = io::stdout().lock();
    let mut failed = false;
    if interactive_mode {
        if let Some(editor) = &mut editor {
            failed = run_interactive(
                &conn,
                editor,
                &mut writer,
                &mut io::stderr().lock(),
                input_limit,
            )?;
            editor.save()?;
        } else {
            failed = run_interactive(
                &conn,
                &mut input::Plain(&mut io::stdin().lock()),
                &mut writer,
                &mut io::stderr().lock(),
                input_limit,
            )?;
        }
    } else if line_mode {
        let mut reader = io::stdin().lock();
        loop {
            let bytes = read_input(&mut reader, input_limit, true)?;
            if bytes.len() > input_limit {
                report_input_limit(&conn, &mut writer, input_limit)?;
                return Ok(std::process::ExitCode::FAILURE);
            }
            if bytes.is_empty() {
                break;
            }
            let line = String::from_utf8(bytes)?;
            if !line.trim().is_empty() {
                failed |= if let Some(failed) = run_limited_command(&conn, &line, &mut writer)? {
                    failed
                } else if let Some(sql) = profile_sql(&line) {
                    run_profile(&conn, sql, &mut writer)?
                } else {
                    output(
                        &mut writer,
                        conn.execute_report(&line, &Parameters::new()),
                        None,
                    )?
                };
            }
        }
    } else {
        let bytes = read_input(&mut io::stdin().lock(), input_limit, false)?;
        if bytes.len() > input_limit {
            report_input_limit(&conn, &mut writer, input_limit)?;
            return Ok(std::process::ExitCode::FAILURE);
        }
        failed = run_script(&conn, &String::from_utf8(bytes)?, &mut writer)?;
    }

    Ok(if failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    })
}

fn run_audit(
    path: &str,
    table: &str,
    limits: romedb::IntegrityLimits,
) -> Result<std::process::ExitCode, Box<dyn std::error::Error>> {
    let opened = if std::path::Path::new(path).is_file() {
        Database::open(path).and_then(|db| db.connect().map(|conn| (db, conn)))
    } else {
        Err(romedb::Error::NotFound(format!(
            "audit requires an existing database file: {path}"
        )))
    };
    let error_value = |error: romedb::Error| serde_json::json!({"error":{"code":error.code(),"message":error.to_string()}});
    let (value, failed) = match opened {
        Err(error) => (error_value(error), true),
        Ok((_db, conn)) => {
            let before = conn.transaction_state();
            let (mut value, failed) = match conn.check_collection_integrity(table, limits) {
                Ok(report) => (serde_json::to_value(report)?, false),
                Err(error) => (error_value(error), true),
            };
            value["transaction"] =
                serde_json::json!({"before":before,"after":conn.transaction_state()});
            (value, failed)
        }
    };
    let mut writer = io::stdout().lock();
    writeln!(writer, "{}", serde_json::to_string(&value)?)?;
    writer.flush()?;
    Ok(if failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    })
}

// Read at most one sentinel byte beyond the limit; never execute this prefix
// when input is oversized, and check size before decoding a split UTF-8 scalar.
fn read_input(reader: &mut impl BufRead, limit: usize, line: bool) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut bounded = reader.take((limit + 1) as u64);
    if line {
        bounded.read_until(b'\n', &mut bytes)?;
    } else {
        bounded.read_to_end(&mut bytes)?;
    }
    Ok(bytes)
}
fn report_input_limit(
    conn: &romedb::Connection,
    writer: &mut impl Write,
    limit: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = conn.transaction_state();
    output(
        writer,
        ExecutionReport {
            result: Err(romedb::Error::Limit(format!(
                "CLI input exceeds {limit} bytes"
            ))),
            transaction_before: state,
            transaction_after: state,
        },
        None,
    )?;
    Ok(())
}

fn run_interactive(
    conn: &romedb::Connection,
    reader: &mut impl input::Input,
    writer: &mut impl Write,
    prompt: &mut impl Write,
    input_limit: usize,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut buffer = String::new();
    let mut failed = false;
    loop {
        let label = if !buffer.is_empty() {
            "...> "
        } else if conn.transaction_state() == romedb::TransactionState::Active {
            "romedb(tx)> "
        } else {
            "romedb> "
        };
        let bytes = match reader.read(label, prompt, input_limit)? {
            input::Read::Line(bytes) => bytes,
            input::Read::Interrupted => {
                buffer.clear();
                writeln!(prompt)?;
                continue;
            }
        };
        if bytes.len() > input_limit {
            report_input_limit(conn, writer, input_limit)?;
            return Ok(true);
        }
        if bytes.is_empty() {
            if !buffer.trim().is_empty() {
                reader.remember(&buffer)?;
                failed |= run_script(conn, &buffer, writer)?;
            }
            return Ok(failed);
        }
        let line = String::from_utf8(bytes)?;
        match line.trim() {
            ".quit" | ".exit" => return Ok(failed),
            ".clear" => {
                buffer.clear();
                continue;
            }
            ".help" => {
                writeln!(
                    prompt,
                    "End statements with a semicolon. .clear discards pending input; .quit exits.\n.profile SELECT ... executes one SELECT with primary engine counters.\n.select-limit ROWS BYTES SELECT ... and .profile-limit ROWS BYTES SELECT ... bound returned results.\n.timeout MILLISECONDS SQL requests cooperative cancellation at its deadline.\n.write-limit ROWS BYTES SQL checks write results atomically; budgets do not cap process memory.
Transactions use BEGIN, COMMIT and ROLLBACK. JSON results go to stdout.\nTerminal editing supports arrows and history. Ctrl-C clears pending input or cancels running engine work.\nHistory stays in memory unless --history PATH is supplied; leading spaces omit entries."
                )?;
                continue;
            }
            _ => {}
        }
        if line.len() > input_limit - buffer.len() {
            report_input_limit(conn, writer, input_limit)?;
            return Ok(true);
        }
        buffer.push_str(&line);
        match interactive_complete(&buffer) {
            Ok(false) => continue,
            Ok(true) | Err(_) => {
                reader.remember(&buffer)?;
                failed |= run_script(conn, &buffer, writer)?;
                buffer.clear();
            }
        }
    }
}

fn command_word<'a>(input: &mut &'a str) -> &'a str {
    *input = input.trim_start();
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    let token = &input[..end];
    *input = &input[end..];
    token
}

fn profile_sql(input: &str) -> Option<&str> {
    let mut rest = input;
    (command_word(&mut rest) == ".profile").then_some(rest.trim_start())
}

// Dot commands wrap SQL: only the SQL suffix determines statement completion.
fn interactive_complete(input: &str) -> romeql_parser::Result<bool> {
    let mut rest = input;
    match command_word(&mut rest) {
        ".profile" if rest.trim().is_empty() => Ok(false),
        ".profile" => romeql_parser::script_complete(rest),
        ".timeout" => {
            if command_word(&mut rest).parse::<u32>().is_err() {
                return Ok(true);
            }
            if rest.trim().is_empty() {
                return Ok(false);
            }
            romeql_parser::script_complete(rest)
        }
        ".select-limit" | ".profile-limit" | ".write-limit" => {
            for _ in 0..2 {
                if command_word(&mut rest).parse::<usize>().is_err() {
                    // Dispatch invalid headers immediately to the normal coded
                    // error path rather than waiting for more SQL indefinitely.
                    return Ok(true);
                }
            }
            if rest.trim().is_empty() {
                return Ok(false);
            }
            romeql_parser::script_complete(rest)
        }
        _ => romeql_parser::script_complete(input),
    }
}

fn run_limited_command(
    conn: &romedb::Connection,
    command: &str,
    writer: &mut impl Write,
) -> Result<Option<bool>, Box<dyn std::error::Error>> {
    let mut rest = command;
    let name = command_word(&mut rest);
    if !matches!(
        name,
        ".select-limit" | ".profile-limit" | ".write-limit" | ".timeout"
    ) {
        return Ok(None);
    }
    let before = conn.transaction_state();
    let execution = (|| -> romedb::Result<_> {
        if name == ".timeout" {
            let millis = command_word(&mut rest).parse::<u32>().map_err(|_| {
                romedb::Error::Validation("expected uint32 timeout milliseconds".into())
            })?;
            let sql = rest.trim_start();
            if sql.is_empty() {
                return Err(romedb::Error::Validation(
                    "expected one SQL statement after timeout".into(),
                ));
            }
            let deadline = std::time::Instant::now()
                .checked_add(std::time::Duration::from_millis(u64::from(millis)))
                .ok_or_else(|| romedb::Error::Validation("timeout deadline overflow".into()))?;
            let token = romedb::CancellationToken::with_deadline(deadline);
            return conn
                .execute_cancellable(sql, &Parameters::new(), &token)
                .map(|result| (result, None));
        }
        let max_rows = command_word(&mut rest).parse::<usize>().map_err(|_| {
            romedb::Error::Validation("expected nonnegative result row limit".into())
        })?;
        let max_payload_bytes = command_word(&mut rest).parse::<usize>().map_err(|_| {
            romedb::Error::Validation("expected nonnegative result payload byte limit".into())
        })?;
        let sql = rest.trim_start();
        if sql.trim().is_empty() {
            return Err(romedb::Error::Validation(
                "expected one SQL statement after result limits".into(),
            ));
        }
        let limits = romedb::ResultLimits {
            max_rows,
            max_payload_bytes,
        };
        let params = Parameters::new();
        match name {
            ".select-limit" => conn
                .select_with_limits(sql, &params, limits)
                .map(|result| (result, None)),
            ".profile-limit" => conn
                .profile_select_with_limits(sql, &params, limits)
                .map(|profile| (profile.result, Some(profile.metrics))),
            ".write-limit" => conn
                .write_with_result_limits(sql, &params, limits)
                .map(|result| (result, None)),
            _ => unreachable!("limited command dispatched"),
        }
    })();
    let (result, metrics) = match execution {
        Ok((result, metrics)) => (Ok(result), metrics),
        Err(error) => (Err(error), None),
    };
    output_with_metrics(
        writer,
        ExecutionReport {
            result,
            transaction_before: before,
            transaction_after: conn.transaction_state(),
        },
        None,
        metrics,
    )
    .map(Some)
}

fn run_profile(
    conn: &romedb::Connection,
    sql: &str,
    writer: &mut impl Write,
) -> Result<bool, Box<dyn std::error::Error>> {
    let before = conn.transaction_state();
    let (result, metrics) = match conn.profile_select(sql, &Parameters::new()) {
        Ok(profile) => (Ok(profile.result), Some(profile.metrics)),
        Err(error) => (Err(error), None),
    };
    output_with_metrics(
        writer,
        ExecutionReport {
            result,
            transaction_before: before,
            transaction_after: conn.transaction_state(),
        },
        None,
        metrics,
    )
}
fn run_script(
    conn: &romedb::Connection,
    script: &str,
    writer: &mut impl Write,
) -> Result<bool, Box<dyn std::error::Error>> {
    if let Some(failed) = run_limited_command(conn, script, writer)? {
        return Ok(failed);
    }
    if let Some(sql) = profile_sql(script) {
        return run_profile(conn, sql, writer);
    }
    let mut failed = false;
    let mut output_error = None;
    let execution = conn.visit_batch(script, |report| {
        match output(writer, report.execution, Some(report.offset)) {
            Ok(statement_failed) => failed |= statement_failed,
            Err(error) => output_error = Some(error),
        }
        Ok(output_error.is_none())
    });
    if let Some(error) = output_error {
        return Err(error);
    }
    if let Err(error) = execution {
        let state = conn.transaction_state();
        output(
            writer,
            ExecutionReport {
                result: Err(error),
                transaction_before: state,
                transaction_after: state,
            },
            None,
        )?;
        failed = true;
    }
    Ok(failed)
}

fn migration_plan(directory: &str) -> Result<Vec<romedb::Migration>, Box<dyn std::error::Error>> {
    let mut plan = Vec::new();
    let mut bytes = 0usize;
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "sql") {
            continue;
        }
        if plan.len() >= 1000 {
            return Err(format!(
                "migration files exceed runner limits while loading {}",
                path.display()
            )
            .into());
        }
        if !std::fs::metadata(&path)
            .map_err(|error| format!("cannot inspect migration {}: {error}", path.display()))?
            .is_file()
        {
            return Err(format!(
                "migration source must be a regular file: {}",
                path.display()
            )
            .into());
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("migration filename must be UTF-8: {}", path.display()))?
            .to_owned();
        let (version, _) = name.split_once('_').ok_or_else(|| {
            format!(
                "expected VERSION_name.sql migration filename: {}",
                path.display()
            )
        })?;
        let version = version
            .parse::<i64>()
            .map_err(|error| format!("invalid migration version in {}: {error}", path.display()))?;
        let limit = (4 * 1024 * 1024).min(16 * 1024 * 1024 - bytes);
        let mut input = Vec::new();
        std::fs::File::open(&path)
            .map_err(|error| format!("cannot open migration {}: {error}", path.display()))?
            .take((limit + 1) as u64)
            .read_to_end(&mut input)
            .map_err(|error| format!("cannot read migration {}: {error}", path.display()))?;
        if input.len() > limit {
            return Err(format!(
                "migration files exceed runner limits while loading {}",
                path.display()
            )
            .into());
        }
        let sql = String::from_utf8(input)
            .map_err(|error| format!("cannot read migration {}: {error}", path.display()))?;
        bytes += sql.len();
        plan.push(romedb::Migration { version, name, sql });
    }
    plan.sort_by_key(|m| m.version);
    Ok(plan)
}

fn transfer_text(input: Vec<u8>, limit: usize) -> romedb::Result<String> {
    if input.len() > limit {
        return Err(romedb::Error::Limit(format!(
            "transfer exceeds {limit} bytes"
        )));
    }
    String::from_utf8(input).map_err(|error| {
        romedb::Error::Validation(format!("transfer input must be UTF-8: {error}"))
    })
}

fn operation_error(
    writer: &mut impl Write,
    error: &romedb::Error,
    before: romedb::TransactionState,
    after: romedb::TransactionState,
) -> io::Result<()> {
    let mut diagnostic = serde_json::json!({"code":error.code(),"message":error.to_string()});
    if let romedb::Error::Migration {
        version,
        offset,
        source,
    } = error
    {
        diagnostic["migration"] = serde_json::json!({
            "version":version,"offset":offset,
            "cause":{"code":source.code(),"message":source.to_string()}
        });
    }
    serde_json::to_writer(
        &mut *writer,
        &serde_json::json!({
            "error":diagnostic,
            "transaction":{"before":before,"after":after}
        }),
    )
    .map_err(io::Error::from)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_input_checks_bytes_before_decoding_split_utf8() {
        let mut reader = io::Cursor::new("abcไทย".as_bytes());
        let input = read_input(&mut reader, 4, false).unwrap();
        assert_eq!(reader.position(), 5);
        assert_eq!(transfer_text(input, 4).unwrap_err().code(), "FDB_LIMIT");
        assert_eq!(transfer_text("aไ".as_bytes().to_vec(), 4).unwrap(), "aไ");
        assert_eq!(
            transfer_text(vec![0xff], 4).unwrap_err().code(),
            "FDB_VALIDATION"
        );
    }

    #[test]
    fn migration_loader_enforces_aggregate_bytes_before_utf8_decoding() {
        let root = std::env::temp_dir().join(format!(
            "romedb-migration-aggregate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let mut sql = b"--".to_vec();
        sql.resize(4 * 1024 * 1024 - "ไทย".len(), b' ');
        sql.extend_from_slice("ไทย".as_bytes());
        for version in 1..=4 {
            std::fs::write(root.join(format!("{version}_source.sql")), &sql).unwrap();
        }
        let excess = root.join("5_source.sql");
        std::fs::write(&excess, b" ").unwrap();
        let error = migration_plan(root.to_str().unwrap()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("migration files exceed runner limits"),
            "{error}"
        );
        std::fs::remove_file(excess).unwrap();
        let plan = migration_plan(root.to_str().unwrap()).unwrap();
        assert_eq!(plan.len(), 4);
        assert_eq!(
            plan.iter().map(|entry| entry.sql.len()).sum::<usize>(),
            16 * 1024 * 1024
        );
        assert!(plan.iter().all(|entry| entry.sql.ends_with("ไทย")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn direct_report_output_preserves_json_contract() {
        let db = Database::open(":memory:").unwrap();
        let conn = db.connect().unwrap();
        let metrics = conn
            .profile_select("SELECT 1", &Parameters::new())
            .unwrap()
            .metrics;
        for failed in [false, true] {
            for offset in [None, Some(17)] {
                for profile in [None, Some(metrics)] {
                    let report = ExecutionReport {
                        result: if failed {
                            Err(romedb::Error::Validation("quoted\"\nไทย".into()))
                        } else {
                            Ok(romedb::QueryResult {
                                columns: vec!["quoted\"\nไทย".into()],
                                rows: vec![vec![romedb::Value::Array(vec![
                                    romedb::Value::Integer(i64::MAX),
                                    romedb::Value::Number(-0.0),
                                    romedb::Value::Binary(vec![0, 255]),
                                    romedb::Value::String("[\"\\ไทย".into()),
                                ])]],
                                affected: 0,
                            })
                        },
                        transaction_before: romedb::TransactionState::Active,
                        transaction_after: romedb::TransactionState::Active,
                    };
                    let mut expected = match &report.result {
                        Ok(result) => serde_json::to_value(result).unwrap(),
                        Err(error) => {
                            serde_json::json!({"error":{"code":error.code(),"message":error.to_string()}})
                        }
                    };
                    expected["transaction"] =
                        serde_json::json!({"before":"active","after":"active"});
                    if let Some(profile) = profile {
                        expected["profile"] = serde_json::to_value(profile).unwrap();
                    }
                    if let Some(offset) = offset {
                        expected["offset"] = offset.into();
                    }
                    let mut output = Vec::new();
                    assert_eq!(
                        output_with_metrics(&mut output, report, offset, profile).unwrap(),
                        failed
                    );
                    assert_eq!(output.last(), Some(&b'\n'));
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
                        expected
                    );
                }
            }
        }
    }

    #[test]
    fn failure_inside_serialized_rows_stops_later_script_writes() {
        struct PartialOutput(Vec<u8>);
        impl Write for PartialOutput {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let count = bytes.len().min(400 - self.0.len()).min(3);
                if count == 0 {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                self.0.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let db = Database::open(":memory:").unwrap();
        let conn = db.connect().unwrap();
        conn.execute("CREATE TABLE samples(n INTEGER)", &Parameters::new())
            .unwrap();
        let mut writer = PartialOutput(Vec::new());
        let error = run_script(
            &conn,
            "INSERT INTO samples VALUES(1); SELECT zeroblob(4096); INSERT INTO samples VALUES(2);",
            &mut writer,
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert!(String::from_utf8(writer.0).unwrap().contains("Binary"));
        assert_eq!(
            conn.execute("SELECT n FROM samples", &Parameters::new())
                .unwrap()
                .rows,
            vec![vec![romedb::Value::Integer(1)]]
        );
    }

    struct FailingOutput {
        flushes: usize,
        fail_flush: bool,
    }
    impl Write for FailingOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.flushes == 1 && !self.fail_flush {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.fail_flush {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            Ok(())
        }
    }

    #[test]
    fn output_failure_stops_scripts_before_later_writes() {
        for fail_flush in [false, true] {
            let db = Database::open(":memory:").unwrap();
            let conn = db.connect().unwrap();
            conn.execute("CREATE TABLE samples(value INTEGER)", &Parameters::new())
                .unwrap();
            let mut writer = FailingOutput {
                flushes: 0,
                fail_flush,
            };
            let script = if fail_flush {
                "INSERT INTO samples VALUES (1); INSERT INTO samples VALUES (2);"
            } else {
                "INSERT INTO samples VALUES (1); SELECT * FROM samples; INSERT INTO samples VALUES (2);"
            };
            let error = run_script(&conn, script, &mut writer).unwrap_err();
            assert_eq!(
                error.downcast_ref::<io::Error>().unwrap().kind(),
                io::ErrorKind::BrokenPipe
            );
            assert_eq!(writer.flushes, 1);
            assert_eq!(
                conn.execute("SELECT * FROM samples", &Parameters::new())
                    .unwrap()
                    .rows,
                vec![vec![romedb::Value::Integer(1)]]
            );
            assert_eq!(
                conn.transaction_state(),
                romedb::TransactionState::Autocommit
            );
        }
    }

    #[test]
    fn output_failure_preserves_explicit_transaction_for_caller() {
        let db = Database::open(":memory:").unwrap();
        let conn = db.connect().unwrap();
        conn.execute("CREATE TABLE samples(value INTEGER)", &Parameters::new())
            .unwrap();
        conn.execute("BEGIN", &Parameters::new()).unwrap();
        let mut writer = FailingOutput {
            flushes: 0,
            fail_flush: true,
        };
        assert!(run_script(
            &conn,
            "INSERT INTO samples VALUES (1); COMMIT;",
            &mut writer
        )
        .is_err());
        assert_eq!(conn.transaction_state(), romedb::TransactionState::Active);
        conn.execute("ROLLBACK", &Parameters::new()).unwrap();
        assert!(conn
            .execute("SELECT * FROM samples", &Parameters::new())
            .unwrap()
            .rows
            .is_empty());
    }
}
