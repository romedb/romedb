use romedb_service::{
    http::{serve, Token},
    DatabaseService,
};
use std::{io::Write, path::PathBuf, process::ExitCode, sync::Arc};

pub fn run(args: Vec<String>) -> Result<ExitCode, Box<dyn std::error::Error>> {
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        println!("Usage: romedb serve DATABASE [--port PORT] [--token-file PATH] [--create]\nServe one MVCC database on 127.0.0.1 (default port 4321).\n--create exclusively creates a new MVCC file; ordinary startup never creates a file.\nExisting WAL files require explicit backup and migration before serving.\nThe token is stored in a private file and is never printed.\nCtrl-C or SIGTERM stops admission, finishes active requests and closes the database.");
        return Ok(ExitCode::SUCCESS);
    }
    let mut path = None;
    let mut token_file = None;
    let mut create = false;
    let mut port = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--create" if !create => create = true,
            "--port" if port.is_none() => {
                let value = args
                    .next()
                    .ok_or("--port requires a number")?
                    .parse::<u16>()?;
                if value == 0 {
                    return Err("port must be between 1 and 65535".into());
                }
                port = Some(value);
            }
            "--token-file" if token_file.is_none() => {
                token_file = Some(PathBuf::from(
                    args.next().ok_or("--token-file requires a path")?,
                ))
            }
            _ if arg.starts_with('-') => {
                return Err("unknown or repeated serve option; use romedb serve --help".into())
            }
            _ if path.is_none() => path = Some(PathBuf::from(arg)),
            _ => return Err("serve requires exactly one database path".into()),
        }
    }
    let requested = path.ok_or("serve requires a database path; use romedb serve --help")?;
    let parent = requested
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let path = parent.canonicalize()?.join(
        requested
            .file_name()
            .ok_or("database requires a file name")?,
    );
    if create && path.try_exists()? {
        return Err("--create requires a missing database file".into());
    }
    if !create && !path.is_file() {
        return Err("database is missing; use --create to create a new MVCC database".into());
    }
    let token_file = token_file.unwrap_or_else(|| {
        let mut name = path.as_os_str().to_os_string();
        name.push(".token");
        PathBuf::from(name)
    });
    let token_file = if token_file.is_absolute() {
        token_file
    } else {
        std::env::current_dir()?.join(token_file)
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(36)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let listener =
            tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port.unwrap_or(4321)))
                .await?;
        let address = listener.local_addr()?;
        let database = Arc::new(if create {
            DatabaseService::create(&path)?
        } else {
            DatabaseService::open(&path)?
        });
        let token = Token::load_or_create(&token_file)?;
        println!(
            "Database URL: http://{address}/v1/database\nToken file: {}",
            token_file.display()
        );
        std::io::stdout().flush()?;
        serve(listener, database, token, async move {
            tokio::select! { _ = interrupt.recv() => {}, _ = terminate.recv() => {} }
        })
        .await?;
        Ok::<_, Box<dyn std::error::Error>>(ExitCode::SUCCESS)
    })
}
