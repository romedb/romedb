use crate::{DatabaseService, Error, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    body::Incoming, header, server::conn::http1, service::service_fn, Method, Request, Response,
    StatusCode,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use sha2::{Digest, Sha256};
use std::{
    convert::Infallible,
    fs::{File, OpenOptions},
    future::Future,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{watch, Semaphore},
    task::JoinSet,
    time::timeout,
};

const MAX_DATABASE_JOBS: u32 = 36;

pub struct Token {
    hash: [u8; 32],
}

impl Token {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        let opened = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path);
        let mut file = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(path)?;
                let token = format!(
                    "{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                );
                file.write_all(token.as_bytes())?;
                file.write_all(b"\n")?;
                file.sync_all()?;
                if let Some(parent) = path.parent() {
                    File::open(parent)?.sync_all()?;
                }
                return Ok(Self {
                    hash: Sha256::digest(token.as_bytes()).into(),
                });
            }
            Err(_) => {
                return Err(Error::new(
                    "FDB_AUTH",
                    "token file cannot be securely opened",
                ))
            }
        };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o600
            || metadata.len() > 256
        {
            return Err(Error::new(
                "FDB_AUTH",
                "token file must be an owned regular file with mode 0600 and one link",
            ));
        }
        let mut contents = String::new();
        Read::by_ref(&mut file)
            .take(257)
            .read_to_string(&mut contents)?;
        let token = contents.strip_suffix('\n').unwrap_or(&contents);
        if token.len() < 32
            || token.len() > 128
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err(Error::new("FDB_AUTH", "token file contents are invalid"));
        }
        Ok(Self {
            hash: Sha256::digest(token.as_bytes()).into(),
        })
    }

    fn authorizes(&self, supplied: &[u8]) -> bool {
        let digest = Sha256::digest(supplied);
        self.hash
            .iter()
            .zip(digest.iter())
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
    }
}

pub async fn serve(
    listener: TcpListener,
    database: Arc<DatabaseService>,
    token: Token,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let address = listener.local_addr()?;
    if !address.ip().is_loopback() {
        return Err(Error::new(
            "FDB_VALIDATION",
            "database service must bind to loopback",
        ));
    }
    let token = Arc::new(token);
    let connections = Arc::new(Semaphore::new(64));
    let jobs = Arc::new(Semaphore::new(MAX_DATABASE_JOBS as usize));
    let (stopping, _) = watch::channel(false);
    let mut tasks = JoinSet::new();
    tokio::pin!(shutdown);
    let mut outcome = loop {
        tokio::select! {
            _ = &mut shutdown => break Ok(()),
            completed = tasks.join_next(), if !tasks.is_empty() => {
                if completed.is_some_and(|result| result.is_err()) { break Err(Error::new("FDB_STORAGE", "HTTP connection task failed")); }
            }
            accepted = listener.accept() => {
                let (stream, _) = match accepted { Ok(accepted) => accepted, Err(error) => break Err(error.into()) };
                let permit = match connections.clone().try_acquire_owned() { Ok(permit) => permit, Err(_) => { drop(stream); continue; } };
                let context = Context { database: database.clone(), token: token.clone(), jobs: jobs.clone(), port: address.port() };
                let mut stopped = stopping.subscribe();
                tasks.spawn(async move {
                    let _permit = permit;
                    let mut builder = http1::Builder::new();
                    builder.timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(5)).max_headers(32).max_buf_size(16384);
                    let connection = builder.serve_connection(TokioIo::new(stream), service_fn(move |request| dispatch(context.clone(), request)));
                    tokio::pin!(connection);
                    tokio::select! {
                        _ = &mut connection => {},
                        _ = stopped.changed() => {
                            connection.as_mut().graceful_shutdown();
                            let _ = timeout(Duration::from_secs(20), &mut connection).await;
                        }
                    }
                });
            }
        }
    };
    drop(listener);
    database.begin_shutdown();
    let _ = stopping.send(true);
    while let Some(completed) = tasks.join_next().await {
        if completed.is_err() && outcome.is_ok() {
            outcome = Err(Error::new("FDB_STORAGE", "HTTP connection task failed"));
        }
    }
    let _completed_jobs = jobs
        .acquire_many(MAX_DATABASE_JOBS)
        .await
        .map_err(|_| Error::new("FDB_STORAGE", "database job tracking closed"))?;
    if let Err(error) = database.checkpoint_for_shutdown() {
        eprintln!("romedb-service: {}: {}", error.code, error.message);
    }
    outcome
}

#[derive(Clone)]
struct Context {
    database: Arc<DatabaseService>,
    token: Arc<Token>,
    jobs: Arc<Semaphore>,
    port: u16,
}

async fn dispatch(
    context: Context,
    request: Request<Incoming>,
) -> std::result::Result<Response<Full<Bytes>>, Infallible> {
    let response = match process(context, request).await {
        Ok(response) => response,
        Err(error) => {
            let status = match error.code.as_str() {
                "FDB_AUTH" => StatusCode::UNAUTHORIZED,
                "FDB_ORIGIN" => StatusCode::FORBIDDEN,
                "FDB_NOT_FOUND" => StatusCode::NOT_FOUND,
                "FDB_METHOD" => StatusCode::METHOD_NOT_ALLOWED,
                "FDB_BUSY" | "FDB_CLOSED" => StatusCode::SERVICE_UNAVAILABLE,
                "FDB_CANCELLED" => StatusCode::REQUEST_TIMEOUT,
                "FDB_LIMIT" => StatusCode::PAYLOAD_TOO_LARGE,
                "FDB_STORAGE" => StatusCode::SERVICE_UNAVAILABLE,
                "FDB_CONFLICT" | "FDB_REQUEST_IDENTITY" => StatusCode::CONFLICT,
                _ => StatusCode::BAD_REQUEST,
            };
            response(
                status,
                serde_json::json!({"error":{"code":error.code,"message":error.message}})
                    .to_string(),
            )
        }
    };
    Ok(response)
}

async fn process(context: Context, request: Request<Incoming>) -> Result<Response<Full<Bytes>>> {
    validate_request(&context, &request)?;
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let body = if method == Method::POST {
        if request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            != Some("application/json")
            || request.headers().contains_key(header::CONTENT_ENCODING)
        {
            return Err(Error::new(
                "FDB_PROTOCOL",
                "requests require uncompressed application/json",
            ));
        }
        if request
            .headers()
            .get(header::CONTENT_LENGTH)
            .is_some_and(|v| {
                v.to_str()
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .is_none_or(|length| length > 65536)
            })
        {
            return Err(Error::new("FDB_LIMIT", "request exceeds 64 KiB"));
        }
        let collected = timeout(Duration::from_secs(5), async {
            let mut bytes = Vec::new();
            let mut body = request.into_body();
            while let Some(frame) = body.frame().await {
                let frame = frame
                    .map_err(|_| Error::new("FDB_PROTOCOL", "request body could not be read"))?;
                if let Ok(data) = frame.into_data() {
                    if bytes.len() + data.len() > 65536 {
                        return Err(Error::new("FDB_LIMIT", "request exceeds 64 KiB"));
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            String::from_utf8(bytes)
                .map_err(|_| Error::new("FDB_PROTOCOL", "request JSON must be UTF-8"))
        })
        .await
        .map_err(|_| Error::new("FDB_CANCELLED", "request body deadline expired"))??;
        Some(collected)
    } else {
        None
    };
    let permit = context
        .jobs
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::new("FDB_BUSY", "service request capacity is full"))?;
    let operation = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        match body {
            None => context.database.info().map(|value| value.to_string()),
            Some(body) if path.ends_with("/read") => context.database.read(&body),
            Some(body) => context.database.query(&body),
        }
    });
    let body = operation
        .await
        .map_err(|_| Error::new("FDB_STORAGE", "database request task failed"))??;
    Ok(response(StatusCode::OK, body))
}

fn validate_request(context: &Context, request: &Request<Incoming>) -> Result<()> {
    if request.uri().scheme().is_some()
        || request.uri().authority().is_some()
        || request.uri().query().is_some()
    {
        return Err(Error::new(
            "FDB_PROTOCOL",
            "request requires a relative database path without a query",
        ));
    }
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    let hosts = [
        format!("127.0.0.1:{}", context.port),
        format!("localhost:{}", context.port),
        format!("[::1]:{}", context.port),
    ];
    if !hosts.iter().any(|allowed| Some(allowed.as_str()) == host)
        || request.headers().contains_key(header::ORIGIN)
        || request.headers().contains_key(header::COOKIE)
        || request.headers().contains_key("sec-fetch-site")
    {
        return Err(Error::new(
            "FDB_ORIGIN",
            "database service accepts native loopback clients without browser origins or cookies",
        ));
    }
    if request
        .headers()
        .get_all(header::AUTHORIZATION)
        .iter()
        .count()
        != 1
        || request.headers().get_all(header::HOST).iter().count() != 1
    {
        return Err(Error::new(
            "FDB_AUTH",
            "exactly one host and authorization header are required",
        ));
    }
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if !authorization.is_some_and(|value| context.token.authorizes(value.as_bytes())) {
        return Err(Error::new("FDB_AUTH", "database token is required"));
    }
    if request
        .headers()
        .get_all("x-romedb-database-id")
        .iter()
        .count()
        > 1
        || request
            .headers()
            .get("x-romedb-database-id")
            .is_some_and(|id| id.to_str().ok() != Some(context.database.database_id.as_str()))
    {
        return Err(Error::new(
            "FDB_REQUEST_IDENTITY",
            "database identity changed; connect to the intended database before continuing",
        ));
    }
    match (request.method(), request.uri().path()) {
        (&Method::GET, "/v1/database")
        | (&Method::POST, "/v1/database/read" | "/v1/database/query") => Ok(()),
        (_, "/v1/database" | "/v1/database/read" | "/v1/database/query") => {
            Err(Error::new("FDB_METHOD", "method is not supported"))
        }
        _ => Err(Error::new(
            "FDB_NOT_FOUND",
            "database endpoint does not exist",
        )),
    }
}

fn response(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONNECTION, "close")
        .header("x-content-type-options", "nosniff")
        .body(Full::new(Bytes::from(body)))
        .expect("fixed HTTP response")
}
