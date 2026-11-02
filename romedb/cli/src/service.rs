use romedb_service::{
    directory::DirectoryService,
    http::{serve, serve_directory, Token},
    DatabaseService,
};
use std::{io::Write, path::PathBuf, process::ExitCode, sync::Arc};

pub fn run(args: Vec<String>) -> Result<ExitCode, Box<dyn std::error::Error>> {
    if args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h") {
        println!("Usage: romedb serve DATABASE [--port PORT] [--token-file PATH] [--create]\n       romedb serve --directory DIRECTORY [--port PORT] [--token-file PATH] [--create] [--max-open N]\nServe one MVCC database on 127.0.0.1 (default port 4321).\n--create exclusively creates a new MVCC file; ordinary startup never creates a file.\nExisting WAL files require explicit backup and migration before serving.\n--directory serves every DIRECTORY/ID.romedb file behind one token with RomeDB Cloud's\norganization routes; --create makes the directory if it is missing, and --max-open\n(default 32) bounds the databases kept open at once.\nThe token is stored in a private file and is never printed.\nCtrl-C or SIGTERM stops admission, finishes active requests and closes the database.");
        return Ok(ExitCode::SUCCESS);
    }
    let mut path = None;
    let mut token_file = None;
    let mut create = false;
    let mut port = None;
    let mut directory = None;
    let mut max_open = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--create" if !create => create = true,
            "--directory" if directory.is_none() => {
                directory = Some(PathBuf::from(
                    args.next().ok_or("--directory requires a path")?,
                ))
            }
            "--max-open" if max_open.is_none() => {
                let value = args
                    .next()
                    .ok_or("--max-open requires a number")?
                    .parse::<usize>()?;
                if !(1..=1024).contains(&value) {
                    return Err("--max-open must be between 1 and 1024".into());
                }
                max_open = Some(value);
            }
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
    if let Some(directory) = directory {
        if path.is_some() {
            return Err("serve takes a database path or --directory, not both".into());
        }
        return run_directory(directory, token_file, create, port, max_open.unwrap_or(32));
    }
    if max_open.is_some() {
        return Err("--max-open applies to --directory".into());
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
    runtime()?.block_on(async {
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

fn run_directory(
    directory: PathBuf,
    token_file: Option<PathBuf>,
    create: bool,
    port: Option<u16>,
    max_open: usize,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let directory = if directory.is_absolute() {
        directory
    } else {
        std::env::current_dir()?.join(directory)
    };
    let service = Arc::new(DirectoryService::open(&directory, create, max_open)?);
    let directory = directory.canonicalize()?;
    let token_file = match token_file {
        Some(path) if path.is_absolute() => path,
        Some(path) => std::env::current_dir()?.join(path),
        None => directory.join("romedb.token"),
    };
    runtime()?.block_on(async {
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let listener =
            tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port.unwrap_or(4321)))
                .await?;
        let address = listener.local_addr()?;
        let token = Token::load_or_create(&token_file)?;
        println!(
            "Organization URL: http://{address}/v1/organizations/{}\nToken file: {}",
            service.organization_id(),
            token_file.display()
        );
        std::io::stdout().flush()?;
        serve_directory(listener, service, token, async move {
            tokio::select! { _ = interrupt.recv() => {}, _ = terminate.recv() => {} }
        })
        .await?;
        Ok::<_, Box<dyn std::error::Error>>(ExitCode::SUCCESS)
    })
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(36)
        .enable_all()
        .build()
}
