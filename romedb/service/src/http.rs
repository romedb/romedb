use crate::{
    directory::{valid_id, DirectoryService},
    files::{self, Content, FileError, FileResult, FileStore, Refusal},
    DatabaseService, Error, FileDownload, Result,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Either, Full};
use hyper::{
    body::{Frame, Incoming, SizeHint},
    header,
    server::conn::http1,
    service::service_fn,
    Method, Request, Response, StatusCode,
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
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
    time::timeout,
};

const MAX_DATABASE_JOBS: u32 = 36;
const MAX_BODY_BYTES_IN_MEMORY: usize = 64 * 1024 * 1024;
const FILE_CHUNK_BYTES: usize = 256 * 1024;
const MAX_FILE_QUERY_BYTES: usize = 4096;
const DATABASE_FILE_TYPE: &str = "application/vnd.romedb.database";
const MAX_FILE_REQUEST_BYTES: usize = 32 * 1024;
const PART_UPLOAD_SECONDS: u64 = 900;

type Body = Either<Full<Bytes>, FileBody>;

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
                    "RDB_AUTH",
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
                "RDB_AUTH",
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
            return Err(Error::new("RDB_AUTH", "token file contents are invalid"));
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

#[derive(Clone)]
enum Target {
    Database(Arc<DatabaseService>),
    Directory(Arc<DirectoryService>),
}

impl Target {
    fn begin_shutdown(&self) {
        match self {
            Self::Database(database) => database.begin_shutdown(),
            Self::Directory(directory) => directory.begin_shutdown(),
        }
    }

    fn checkpoint_for_shutdown(&self) {
        match self {
            Self::Database(database) => {
                if let Err(error) = database.checkpoint_for_shutdown() {
                    eprintln!("romedb-service: {}: {}", error.code, error.message);
                }
            }
            Self::Directory(directory) => directory.checkpoint_for_shutdown(),
        }
    }
}

enum Route {
    Info,
    Read,
    Query,
    File,
    List,
    Create,
    Details(String),
    Delete(String),
    DatabaseRead(String),
    DatabaseQuery(String),
    DatabaseFile(String),
}

enum Reply {
    Json(StatusCode, String),
    File(FileDownload, OwnedSemaphorePermit),
}

pub async fn serve(
    listener: TcpListener,
    database: Arc<DatabaseService>,
    token: Token,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let files = Arc::new(FileStore::new(
        database.files_folder(),
        &database.database_id,
        &database.database_id,
        files::signing_key()?,
    ));
    let started = {
        let (database, files) = (database.clone(), files.clone());
        tokio::task::spawn_blocking(move || files.start(&database)).await
    };
    if let Ok(Err(error)) = started {
        eprintln!("romedb-service: files: {}", error.message);
    }
    serve_target(
        listener,
        Target::Database(database),
        Some(files),
        token,
        shutdown,
    )
    .await
}

pub async fn serve_directory(
    listener: TcpListener,
    directory: Arc<DirectoryService>,
    token: Token,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    serve_target(
        listener,
        Target::Directory(directory),
        None,
        token,
        shutdown,
    )
    .await
}

async fn serve_target(
    listener: TcpListener,
    target: Target,
    files: Option<Arc<FileStore>>,
    token: Token,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let address = listener.local_addr()?;
    if !address.ip().is_loopback() {
        return Err(Error::new(
            "RDB_VALIDATION",
            "database service must bind to loopback",
        ));
    }
    let token = Arc::new(token);
    let connections = Arc::new(Semaphore::new(64));
    let jobs = Arc::new(Semaphore::new(MAX_DATABASE_JOBS as usize));
    let body_bytes = Arc::new(Semaphore::new(MAX_BODY_BYTES_IN_MEMORY));
    let (stopping, _) = watch::channel(false);
    let mut tasks = JoinSet::new();
    tokio::pin!(shutdown);
    let mut outcome = loop {
        tokio::select! {
            _ = &mut shutdown => break Ok(()),
            completed = tasks.join_next(), if !tasks.is_empty() => {
                if completed.is_some_and(|result| result.is_err()) { break Err(Error::new("RDB_STORAGE", "HTTP connection task failed")); }
            }
            accepted = listener.accept() => {
                let (stream, _) = match accepted { Ok(accepted) => accepted, Err(error) => break Err(error.into()) };
                let permit = match connections.clone().try_acquire_owned() { Ok(permit) => permit, Err(_) => { drop(stream); continue; } };
                let context = Context { target: target.clone(), files: files.clone(), token: token.clone(), jobs: jobs.clone(), body_bytes: body_bytes.clone(), port: address.port() };
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
    target.begin_shutdown();
    let _ = stopping.send(true);
    while let Some(completed) = tasks.join_next().await {
        if completed.is_err() && outcome.is_ok() {
            outcome = Err(Error::new("RDB_STORAGE", "HTTP connection task failed"));
        }
    }
    let _completed_jobs = jobs
        .acquire_many(MAX_DATABASE_JOBS)
        .await
        .map_err(|_| Error::new("RDB_STORAGE", "database job tracking closed"))?;
    target.checkpoint_for_shutdown();
    outcome
}

#[derive(Clone)]
struct Context {
    target: Target,
    /// The file store of the one database. A directory keeps a store for each database.
    files: Option<Arc<FileStore>>,
    token: Arc<Token>,
    jobs: Arc<Semaphore>,
    body_bytes: Arc<Semaphore>,
    port: u16,
}

async fn dispatch(
    context: Context,
    request: Request<Incoming>,
) -> std::result::Result<Response<Body>, Infallible> {
    if let Some(path) = file_path(&context.target, request.uri().path()) {
        return Ok(respond_to_file_request(context, request, path).await);
    }
    let management = request.uri().path().starts_with("/v1/organizations/")
        && !request.uri().path().ends_with("/read")
        && !request.uri().path().ends_with("/query")
        && !request.uri().path().ends_with("/file");
    let response = match process(context, request).await {
        Ok(response) => response,
        Err(error) => {
            let status = match error.code.as_str() {
                "RDB_AUTH" => StatusCode::UNAUTHORIZED,
                "RDB_ORIGIN" => StatusCode::FORBIDDEN,
                "RDB_NOT_FOUND" => StatusCode::NOT_FOUND,
                "RDB_METHOD" => StatusCode::METHOD_NOT_ALLOWED,
                "RDB_BUSY" | "RDB_CLOSED" => StatusCode::SERVICE_UNAVAILABLE,
                "RDB_CANCELLED" => StatusCode::REQUEST_TIMEOUT,
                "RDB_LIMIT" => StatusCode::PAYLOAD_TOO_LARGE,
                "RDB_STORAGE" => StatusCode::SERVICE_UNAVAILABLE,
                "RDB_CONFLICT" | "RDB_REQUEST_IDENTITY" => StatusCode::CONFLICT,
                _ => StatusCode::BAD_REQUEST,
            };
            response(
                status,
                if management {
                    serde_json::json!({"error":error.message})
                } else {
                    serde_json::json!({"error":{"code":error.code,"message":error.message}})
                }
                .to_string(),
            )
        }
    };
    Ok(response)
}

/// A request under `.../files`, or the usage of an organization's files.
struct FilePath {
    /// The database named in the path. A server of one database has none.
    database: Option<String>,
    route: FileRoute,
}

enum FileRoute {
    Usage,
    Uploads,
    Upload(String),
    UploadAction(String, UploadAction),
    Part(String, u64),
    Files,
    Downloads,
    File(String),
    Download(String),
    Content(String),
    Deletion(String),
    Processing(&'static [&'static str]),
    Unknown,
}

enum UploadAction {
    Parts,
    Complete,
    Abort,
}

impl FileRoute {
    fn methods(&self) -> &'static [&'static str] {
        match self {
            Self::Usage | Self::Upload(_) | Self::Files | Self::Content(_) | Self::Deletion(_) => {
                &["GET"]
            }
            Self::Uploads | Self::UploadAction(..) | Self::Downloads | Self::Download(_) => {
                &["POST"]
            }
            Self::Part(..) => &["PUT"],
            Self::File(_) => &["GET", "PATCH", "DELETE"],
            Self::Processing(methods) => methods,
            Self::Unknown => &["GET", "POST", "PUT", "PATCH", "DELETE"],
        }
    }

    /// A page of another origin may call these: the status, part URLs, completion and
    /// abort of one upload with its session token, and the two signed URLs.
    fn open_to_browsers(&self) -> bool {
        matches!(
            self,
            Self::Upload(_) | Self::UploadAction(..) | Self::Part(..) | Self::Content(_)
        )
    }

    /// The URLs that carry their own signature: one part of an upload, and one download.
    fn is_signed(&self) -> bool {
        matches!(self, Self::Part(..) | Self::Content(_))
    }

    fn takes_query(&self) -> bool {
        matches!(self, Self::Files | Self::Part(..) | Self::Content(_))
    }
}

fn file_path(target: &Target, path: &str) -> Option<FilePath> {
    let (database, rest) = match target {
        Target::Database(_) => (None, path.strip_prefix("/v1/database/files")?),
        Target::Directory(directory) => {
            let rest = path
                .strip_prefix("/v1/organizations/")?
                .strip_prefix(directory.organization_id())?;
            if rest == "/files/usage" {
                return Some(FilePath {
                    database: None,
                    route: FileRoute::Usage,
                });
            }
            let (id, rest) = rest.strip_prefix("/databases/")?.split_at_checked(36)?;
            if !valid_id(id) {
                return None;
            }
            (Some(id.to_owned()), rest.strip_prefix("/files")?)
        }
    };
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    let segments: Vec<&str> = rest.split('/').skip(1).collect();
    let id = |text: &str| files::is_uuid(text).then(|| text.to_owned());
    let route = match segments.as_slice() {
        [] => Some(FileRoute::Files),
        ["usage"] if database.is_none() => Some(FileRoute::Usage),
        ["uploads"] => Some(FileRoute::Uploads),
        ["uploads", upload] => id(upload).map(FileRoute::Upload),
        ["uploads", upload, "parts"] => {
            id(upload).map(|upload| FileRoute::UploadAction(upload, UploadAction::Parts))
        }
        ["uploads", upload, "complete"] => {
            id(upload).map(|upload| FileRoute::UploadAction(upload, UploadAction::Complete))
        }
        ["uploads", upload, "abort"] => {
            id(upload).map(|upload| FileRoute::UploadAction(upload, UploadAction::Abort))
        }
        ["uploads", upload, "parts", number] => id(upload)
            .zip(part_number(number))
            .map(|(upload, number)| FileRoute::Part(upload, number)),
        ["downloads"] => Some(FileRoute::Downloads),
        ["jobs", job] => id(job).map(|_| FileRoute::Processing(&["GET", "DELETE"])),
        [file] => id(file).map(FileRoute::File),
        [file, "download"] => id(file).map(FileRoute::Download),
        [file, "content"] => id(file).map(FileRoute::Content),
        [file, "deletion"] => id(file).map(FileRoute::Deletion),
        [file, "jobs"] => id(file).map(|_| FileRoute::Processing(&["POST"])),
        [file, "derivatives"] => id(file).map(|_| FileRoute::Processing(&["GET"])),
        _ => None,
    };
    Some(FilePath {
        database,
        route: route.unwrap_or(FileRoute::Unknown),
    })
}

fn part_number(text: &str) -> Option<u64> {
    ((1..=5).contains(&text.len())
        && !text.starts_with('0')
        && text.bytes().all(|byte| byte.is_ascii_digit()))
    .then(|| text.parse().ok())
    .flatten()
}

/// Why a file request got no result. Every file route reports it as `{"error": text}`.
struct Refused {
    status: StatusCode,
    message: String,
}

impl Refused {
    fn new(status: StatusCode, message: &str) -> Self {
        Self {
            status,
            message: message.to_owned(),
        }
    }
}

impl From<FileError> for Refused {
    fn from(error: FileError) -> Self {
        Self {
            status: match error.refusal {
                Refusal::Invalid => StatusCode::BAD_REQUEST,
                Refusal::Unauthorized => StatusCode::UNAUTHORIZED,
                Refusal::Forbidden => StatusCode::FORBIDDEN,
                Refusal::NotFound => StatusCode::NOT_FOUND,
                Refusal::Conflict => StatusCode::CONFLICT,
                Refusal::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
                Refusal::Limit => StatusCode::TOO_MANY_REQUESTS,
                Refusal::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            },
            message: error.message,
        }
    }
}

/// Who a file request comes from, once its headers were checked.
enum Access {
    /// A browser asks whether it may send the request.
    Preflight,
    /// The service token.
    Service,
    /// The token of one upload session. The store checks which upload it belongs to.
    Session(String),
    /// No credential: the URL has a signature that the store checks.
    Signed,
}

enum FileReply {
    Json(StatusCode, serde_json::Value),
    Content(Content, OwnedSemaphorePermit),
}

async fn respond_to_file_request(
    context: Context,
    request: Request<Incoming>,
    path: FilePath,
) -> Response<Body> {
    let open_to_browsers = path.route.open_to_browsers();
    let mut reply = match process_file_request(context, request, path).await {
        Ok(None) => response(StatusCode::NO_CONTENT, String::new()),
        Ok(Some(FileReply::Json(status, body))) => response(status, body.to_string()),
        Ok(Some(FileReply::Content(content, permit))) => content_response(content, permit),
        Err(refused) => response(
            refused.status,
            serde_json::json!({"error": refused.message}).to_string(),
        ),
    };
    if open_to_browsers {
        for (name, value) in [
            ("access-control-allow-origin", "*"),
            ("access-control-allow-methods", "GET, POST, PUT, OPTIONS"),
            (
                "access-control-allow-headers",
                "authorization, content-type",
            ),
            ("access-control-max-age", "86400"),
            ("vary", "origin"),
        ] {
            reply
                .headers_mut()
                .insert(name, header::HeaderValue::from_static(value));
        }
    }
    reply
}

async fn process_file_request(
    context: Context,
    request: Request<Incoming>,
    path: FilePath,
) -> std::result::Result<Option<FileReply>, Refused> {
    let access = validate_file_request(&context, &request, &path)?;
    if matches!(access, Access::Preflight) {
        return Ok(None);
    }
    let base = format!(
        "http://{}{}",
        host(&request).unwrap_or_default(),
        match (&context.target, &path.database) {
            (Target::Directory(directory), Some(database)) => format!(
                "/v1/organizations/{}/databases/{database}",
                directory.organization_id()
            ),
            _ => "/v1/database".to_owned(),
        }
    );
    let method = request.method().clone();
    let query = request.uri().query().map(str::to_owned);
    let permit = context.jobs.clone().try_acquire_owned().map_err(|_| {
        Refused::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "File request capacity is full",
        )
    })?;
    if let FileRoute::Part(upload, number) = &path.route {
        let stored = store_part(
            &context,
            request,
            path.database.clone(),
            upload.clone(),
            *number,
            query.unwrap_or_default(),
            permit,
        );
        stored.await?;
        return Ok(Some(FileReply::Json(StatusCode::OK, serde_json::json!({}))));
    }
    let body = if matches!(method, Method::POST | Method::PATCH) {
        small_body(request).await?
    } else {
        Vec::new()
    };
    let operation = tokio::task::spawn_blocking(move || {
        let outcome = file_operation(
            &context,
            &path,
            method.as_str(),
            query.as_deref(),
            &body,
            &base,
            &access,
        );
        (outcome, permit)
    });
    let (outcome, permit) = operation
        .await
        .map_err(|_| Refused::new(StatusCode::SERVICE_UNAVAILABLE, "File request task failed"))?;
    Ok(Some(match outcome? {
        Operation::Json(status, body) => FileReply::Json(status, body),
        Operation::Content(content) => FileReply::Content(content, permit),
    }))
}

/// The checks every file request passes before anything is read or done. The host
/// must be this server on loopback. A browser origin is accepted only on the routes a
/// page may call. A cookie is refused everywhere but on the two signed URLs: a browser
/// sends a page's cookies to every port of its host name, so an `<img>` on
/// `localhost:5173` that shows a file of `localhost:4321` carries them. There the
/// signature is the whole authority, and the cookie is never read.
fn validate_file_request(
    context: &Context,
    request: &Request<Incoming>,
    path: &FilePath,
) -> std::result::Result<Access, Refused> {
    let route = &path.route;
    if request.uri().scheme().is_some() || request.uri().authority().is_some() {
        return Err(Refused::new(
            StatusCode::BAD_REQUEST,
            "request requires a relative database path",
        ));
    }
    let host = host(request);
    let hosts = [
        format!("127.0.0.1:{}", context.port),
        format!("localhost:{}", context.port),
        format!("[::1]:{}", context.port),
    ];
    let from_browser = request.headers().contains_key(header::ORIGIN)
        || request.headers().contains_key("sec-fetch-site");
    if !hosts.iter().any(|allowed| Some(allowed.as_str()) == host)
        || request.headers().get_all(header::HOST).iter().count() != 1
        || (request.headers().contains_key(header::COOKIE) && !route.is_signed())
        || (from_browser && !route.open_to_browsers())
    {
        return Err(Refused::new(
            StatusCode::FORBIDDEN,
            "this file route accepts loopback clients without cookies, and browser origins only on upload session and signed URLs",
        ));
    }
    if route.open_to_browsers() && request.method() == Method::OPTIONS {
        return Ok(Access::Preflight);
    }
    if !route.methods().contains(&request.method().as_str()) {
        return Err(Refused::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "Method not allowed",
        ));
    }
    let mut credentials = request.headers().get_all(header::AUTHORIZATION).iter();
    let bearer = match (credentials.next(), credentials.next()) {
        (Some(value), None) => value.to_str().ok().and_then(|v| v.strip_prefix("Bearer ")),
        _ => None,
    };
    let service = bearer.is_some_and(|value| context.token.authorizes(value.as_bytes()));
    let access = match route {
        _ if route.is_signed() => Access::Signed,
        _ if service => Access::Service,
        FileRoute::Upload(_) | FileRoute::UploadAction(..) => match bearer {
            Some(token) if is_session_token(token) => Access::Session(token.to_owned()),
            _ => {
                return Err(Refused::new(
                    StatusCode::UNAUTHORIZED,
                    "Database token or upload session token is required",
                ))
            }
        },
        _ => {
            return Err(Refused::new(
                StatusCode::UNAUTHORIZED,
                "Database token is required",
            ))
        }
    };
    let expected = match (&context.target, &path.database) {
        (Target::Database(database), _) => Some(database.database_id.as_str()),
        (Target::Directory(_), database) => database.as_deref(),
    };
    let mut identities = request.headers().get_all("x-romedb-database-id").iter();
    if identities
        .next()
        .is_some_and(|id| expected.is_none() || id.to_str().ok() != expected)
        || identities.next().is_some()
    {
        return Err(Refused::new(
            StatusCode::CONFLICT,
            "database identity changed; connect to the intended database before continuing",
        ));
    }
    if let Some(query) = request.uri().query() {
        if !route.takes_query() || query.len() > MAX_FILE_QUERY_BYTES {
            return Err(Refused::new(
                StatusCode::BAD_REQUEST,
                "This file route takes no query",
            ));
        }
    }
    if request.headers().contains_key(header::CONTENT_ENCODING) {
        return Err(Refused::new(
            StatusCode::BAD_REQUEST,
            "File requests take an uncompressed body",
        ));
    }
    Ok(access)
}

fn is_session_token(text: &str) -> bool {
    text.strip_prefix("rdbu_").is_some_and(|random| {
        random.len() == 43
            && random
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    })
}

/// Reads the JSON body of a file request, which is small.
async fn small_body(request: Request<Incoming>) -> std::result::Result<Vec<u8>, Refused> {
    let too_large = || {
        Refused::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "File request body is too large",
        )
    };
    if declared_length(&request).is_some_and(|length| length > MAX_FILE_REQUEST_BYTES as u64) {
        return Err(too_large());
    }
    let collected = timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        let mut body = request.into_body();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| {
                Refused::new(StatusCode::BAD_REQUEST, "request body could not be read")
            })?;
            if let Ok(data) = frame.into_data() {
                if bytes.len() + data.len() > MAX_FILE_REQUEST_BYTES {
                    return Err(too_large());
                }
                bytes.extend_from_slice(&data);
            }
        }
        Ok(bytes)
    });
    collected
        .await
        .map_err(|_| Refused::new(StatusCode::REQUEST_TIMEOUT, "request body deadline expired"))?
}

fn declared_length(request: &Request<Incoming>) -> Option<u64> {
    request.headers().get(header::CONTENT_LENGTH).map(|value| {
        value
            .to_str()
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(u64::MAX)
    })
}

/// Writes the body of a part to disk as it arrives. The part is kept only when the
/// body has exactly the size the upload declared for it.
async fn store_part(
    context: &Context,
    request: Request<Incoming>,
    database: Option<String>,
    upload: String,
    number: u64,
    query: String,
    permit: OwnedSemaphorePermit,
) -> std::result::Result<(), Refused> {
    let failed = || Refused::new(StatusCode::SERVICE_UNAVAILABLE, "File request task failed");
    let opened = {
        let context = context.clone();
        tokio::task::spawn_blocking(move || {
            file_store(&context, database.as_deref())?.begin_part(&upload, number, &query)
        })
    };
    let mut part = opened.await.map_err(|_| failed())??;
    if declared_length(&request).is_some_and(|length| length != part.expected()) {
        return Err(part.wrong_size().into());
    }
    let (sender, mut chunks) = mpsc::channel::<Bytes>(8);
    let writer = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        while let Some(chunk) = chunks.blocking_recv() {
            part.write(&chunk)?;
        }
        part.finish()
    });
    let mut body = request.into_body();
    let received = timeout(Duration::from_secs(PART_UPLOAD_SECONDS), async {
        while let Some(Ok(frame)) = body.frame().await {
            if let Ok(data) = frame.into_data() {
                if sender.send(data).await.is_err() {
                    break;
                }
            }
        }
        drop(sender);
    });
    let in_time = received.await.is_ok();
    let written = writer.await.map_err(|_| failed())?;
    if !in_time {
        return Err(Refused::new(
            StatusCode::REQUEST_TIMEOUT,
            "request body deadline expired",
        ));
    }
    Ok(written?)
}

enum Operation {
    Json(StatusCode, serde_json::Value),
    Content(Content),
}

fn file_operation(
    context: &Context,
    path: &FilePath,
    method: &str,
    query: Option<&str>,
    body: &[u8],
    base: &str,
    access: &Access,
) -> FileResult<Operation> {
    let done = |reply| Ok(Operation::Json(StatusCode::OK, reply));
    match (&path.route, &context.target, &path.database) {
        (FileRoute::Unknown, _, _) => {
            return Err(FileError::new(
                Refusal::NotFound,
                "File route does not exist",
            ))
        }
        (FileRoute::Processing(_), _, _) => {
            return Err(FileError::new(
                Refusal::Unavailable,
                "File processing is unavailable",
            ))
        }
        (FileRoute::Usage, Target::Directory(directory), None) => {
            return done(directory.file_usage()?)
        }
        (FileRoute::Content(file), _, _) => {
            let store = file_store(context, path.database.as_deref())?;
            let download = store.check_download(file, query.unwrap_or_default())?;
            let database = database_of(context, path.database.as_deref())?;
            return Ok(Operation::Content(
                store.content(&database, file, &download)?,
            ));
        }
        _ => {}
    }
    let store = file_store(context, path.database.as_deref())?;
    if let (
        Access::Session(token),
        FileRoute::Upload(upload) | FileRoute::UploadAction(upload, _),
    ) = (access, &path.route)
    {
        if !store.token_matches(upload, token) {
            return Err(FileError::new(
                Refusal::Unauthorized,
                "Upload session token is not valid for this upload",
            ));
        }
    }
    let database = database_of(context, path.database.as_deref())?;
    let sent = || serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
    let sent_or_empty = || {
        if body.is_empty() {
            serde_json::json!({})
        } else {
            sent()
        }
    };
    match (&path.route, method) {
        (FileRoute::Usage, _) => done(store.totals(&database)?.report(store.organization_id())),
        (FileRoute::Uploads, _) => Ok(Operation::Json(
            StatusCode::CREATED,
            store.create_upload(&database, &sent(), base)?,
        )),
        (FileRoute::Upload(upload), _) => done(store.upload_status(&database, upload, base)?),
        (FileRoute::UploadAction(upload, UploadAction::Parts), _) => {
            done(store.part_urls(upload, &sent(), base)?)
        }
        (FileRoute::UploadAction(upload, UploadAction::Complete), _) => {
            done(store.complete_upload(&database, upload, &sent_or_empty())?)
        }
        (FileRoute::UploadAction(upload, UploadAction::Abort), _) => {
            done(store.abort_upload(upload, base)?)
        }
        (FileRoute::Files, _) => done(store.files(&database, query)?),
        (FileRoute::Downloads, _) => done(store.download_urls(&database, &sent(), base)?),
        (FileRoute::File(file), "GET") => done(store.file(&database, file)?),
        (FileRoute::File(file), "PATCH") => done(store.update_file(&database, file, &sent())?),
        (FileRoute::File(file), _) => done(store.delete_file(&database, file)?),
        (FileRoute::Download(file), _) => {
            done(store.download_url(&database, file, &sent_or_empty(), base)?)
        }
        (FileRoute::Deletion(file), _) => done(store.deletion(file)?),
        (
            FileRoute::Part(..)
            | FileRoute::Content(_)
            | FileRoute::Processing(_)
            | FileRoute::Unknown,
            _,
        ) => Err(FileError::new(
            Refusal::NotFound,
            "File route does not exist",
        )),
    }
}

/// The store of the files of the database in the path. No database is opened for it,
/// so the signature of a URL can be checked first.
fn file_store(context: &Context, database: Option<&str>) -> FileResult<Arc<FileStore>> {
    match (&context.target, &context.files, database) {
        (Target::Database(_), Some(files), None) => Ok(files.clone()),
        (Target::Directory(directory), _, Some(id)) => Ok(directory.file_store(id)?),
        _ => Err(FileError::new(
            Refusal::NotFound,
            "File route does not exist",
        )),
    }
}

fn database_of(context: &Context, database: Option<&str>) -> FileResult<Arc<DatabaseService>> {
    match (&context.target, database) {
        (Target::Database(database), None) => Ok(database.clone()),
        (Target::Directory(directory), Some(id)) => Ok(directory.database(id)?),
        _ => Err(FileError::new(
            Refusal::NotFound,
            "File route does not exist",
        )),
    }
}

fn content_response(content: Content, permit: OwnedSemaphorePermit) -> Response<Body> {
    let Content {
        file,
        bytes,
        media_type,
        disposition,
    } = content;
    let header_value = |text: &str, otherwise: &'static str| {
        header::HeaderValue::from_str(text)
            .unwrap_or_else(|_| header::HeaderValue::from_static(otherwise))
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            header_value(&media_type, "application/octet-stream"),
        )
        .header(header::CONTENT_LENGTH, bytes)
        .header(
            header::CONTENT_DISPOSITION,
            header_value(&disposition, "attachment"),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .body(Either::Right(FileBody {
            chunks: file_chunks(file, permit),
            remaining: bytes,
        }))
        .expect("fixed HTTP response")
}

async fn process(context: Context, request: Request<Incoming>) -> Result<Response<Body>> {
    let route = validate_request(&context, &request)?;
    let export = file_options(request.uri().query())?;
    let result_limits = declared_result_limits(&request)?;
    let origin = format!("http://{}", host(&request).unwrap_or_default());
    let body = if request.method() == Method::POST {
        if request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            != Some("application/json")
            || request.headers().contains_key(header::CONTENT_ENCODING)
        {
            return Err(Error::new(
                "RDB_PROTOCOL",
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
                    .is_none_or(|length| length > romedb::MAX_REQUEST_BYTES as u64)
            })
        {
            return Err(Error::new("RDB_LIMIT", "request exceeds 8 MiB"));
        }
        let budget = context.body_bytes.clone();
        let collected = timeout(Duration::from_secs(5), async {
            let mut bytes = Vec::new();
            let mut reserved: Option<OwnedSemaphorePermit> = None;
            let mut body = request.into_body();
            while let Some(frame) = body.frame().await {
                let frame = frame
                    .map_err(|_| Error::new("RDB_PROTOCOL", "request body could not be read"))?;
                if let Ok(data) = frame.into_data() {
                    if bytes.len() + data.len() > romedb::MAX_REQUEST_BYTES {
                        return Err(Error::new("RDB_LIMIT", "request exceeds 8 MiB"));
                    }
                    let permit = budget
                        .clone()
                        .try_acquire_many_owned(data.len() as u32)
                        .map_err(|_| Error::new("RDB_BUSY", "service request memory is full"))?;
                    match reserved.as_mut() {
                        Some(reserved) => reserved.merge(permit),
                        None => reserved = Some(permit),
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            let text = String::from_utf8(bytes)
                .map_err(|_| Error::new("RDB_PROTOCOL", "request JSON must be UTF-8"))?;
            Ok((text, reserved))
        })
        .await
        .map_err(|_| Error::new("RDB_CANCELLED", "request body deadline expired"))??;
        Some(collected)
    } else {
        None
    };
    let permit = context
        .jobs
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::new("RDB_BUSY", "service request capacity is full"))?;
    let operation = tokio::task::spawn_blocking(move || {
        let (body, _body_bytes) = body.unwrap_or_default();
        match (&context.target, &route) {
            (Target::Database(database), Route::File) => {
                return Ok(Reply::File(database.export_file(&export)?, permit))
            }
            (Target::Directory(directory), Route::DatabaseFile(id)) => {
                return Ok(Reply::File(
                    directory.database(id)?.export_file(&export)?,
                    permit,
                ))
            }
            _ => {}
        }
        let _permit = permit;
        match (&context.target, route) {
            (Target::Database(database), Route::Info) => {
                Ok((StatusCode::OK, database.info()?.to_string()))
            }
            (Target::Database(database), Route::Read) => Ok((
                StatusCode::OK,
                database.read_with_result_limits(&body, result_limits)?,
            )),
            (Target::Database(database), Route::Query) => Ok((
                StatusCode::OK,
                database.query_with_result_limits(&body, result_limits)?,
            )),
            (Target::Directory(directory), Route::List) => {
                Ok((StatusCode::OK, directory.list(&origin)?.to_string()))
            }
            (Target::Directory(directory), Route::Create) => Ok((
                StatusCode::CREATED,
                directory.create(&body, &origin)?.to_string(),
            )),
            (Target::Directory(directory), Route::Details(id)) => {
                Ok((StatusCode::OK, directory.details(&id, &origin)?.to_string()))
            }
            (Target::Directory(directory), Route::Delete(id)) => {
                directory.delete(&id)?;
                Ok((StatusCode::NO_CONTENT, String::new()))
            }
            (Target::Directory(directory), Route::DatabaseRead(id)) => Ok((
                StatusCode::OK,
                directory
                    .database(&id)?
                    .read_with_result_limits(&body, result_limits)?,
            )),
            (Target::Directory(directory), Route::DatabaseQuery(id)) => Ok((
                StatusCode::OK,
                directory
                    .database(&id)?
                    .query_with_result_limits(&body, result_limits)?,
            )),
            _ => Err(Error::new(
                "RDB_NOT_FOUND",
                "database endpoint does not exist",
            )),
        }
        .map(|(status, body)| Reply::Json(status, body))
    });
    match operation
        .await
        .map_err(|_| Error::new("RDB_STORAGE", "database request task failed"))??
    {
        Reply::Json(status, body) => Ok(response(status, body)),
        Reply::File(download, permit) => Ok(file_response(download, permit)),
    }
}

fn declared_result_limits(request: &Request<Incoming>) -> Result<romedb::RequestResultLimits> {
    let Some(value) = request.headers().get("x-romedb-result-limits") else {
        return Ok(romedb::RequestResultLimits::EARLIER);
    };
    let invalid = || {
        Error::new(
            "RDB_VALIDATION",
            "x-romedb-result-limits takes rows=<n>; bytes=<n>",
        )
    };
    let text = value.to_str().map_err(|_| invalid())?;
    let (rows, bytes) = text
        .trim()
        .strip_prefix("rows=")
        .and_then(|rest| rest.split_once("; bytes="))
        .ok_or_else(invalid)?;
    let number = |digits: &str| {
        (!digits.is_empty() && digits.len() <= 12 && digits.bytes().all(|b| b.is_ascii_digit()))
            .then(|| digits.parse::<usize>().ok())
            .flatten()
            .filter(|n| *n >= 1)
            .ok_or_else(invalid)
    };
    Ok(romedb::RequestResultLimits::declared(
        number(rows)?,
        number(bytes)?,
    ))
}

fn validate_request(context: &Context, request: &Request<Incoming>) -> Result<Route> {
    if request.uri().scheme().is_some() || request.uri().authority().is_some() {
        return Err(Error::new(
            "RDB_PROTOCOL",
            "request requires a relative database path",
        ));
    }
    let host = host(request);
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
            "RDB_ORIGIN",
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
            "RDB_AUTH",
            "exactly one host and authorization header are required",
        ));
    }
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if !authorization.is_some_and(|value| context.token.authorizes(value.as_bytes())) {
        return Err(Error::new("RDB_AUTH", "database token is required"));
    }
    let route = match &context.target {
        Target::Database(_) => database_route(request.method(), request.uri().path())?,
        Target::Directory(directory) => directory_route(
            request.method(),
            request.uri().path(),
            directory.organization_id(),
        )?,
    };
    if request.uri().query().is_some() && !matches!(route, Route::File | Route::DatabaseFile(_)) {
        return Err(Error::new(
            "RDB_PROTOCOL",
            "only database file requests take a query",
        ));
    }
    let expected = match (&context.target, &route) {
        (Target::Database(database), _) => Some(database.database_id.as_str()),
        (
            Target::Directory(_),
            Route::Details(id)
            | Route::Delete(id)
            | Route::DatabaseRead(id)
            | Route::DatabaseQuery(id)
            | Route::DatabaseFile(id),
        ) => Some(id.as_str()),
        (Target::Directory(_), _) => None,
    };
    if request
        .headers()
        .get_all("x-romedb-database-id")
        .iter()
        .count()
        > 1
        || request
            .headers()
            .get("x-romedb-database-id")
            .is_some_and(|id| expected.is_none() || id.to_str().ok() != expected)
    {
        return Err(Error::new(
            "RDB_REQUEST_IDENTITY",
            "database identity changed; connect to the intended database before continuing",
        ));
    }
    Ok(route)
}

fn database_route(method: &Method, path: &str) -> Result<Route> {
    match (method, path) {
        (&Method::GET, "/v1/database") => Ok(Route::Info),
        (&Method::POST, "/v1/database/read") => Ok(Route::Read),
        (&Method::POST, "/v1/database/query") => Ok(Route::Query),
        (&Method::GET, "/v1/database/file") => Ok(Route::File),
        (_, "/v1/database" | "/v1/database/read" | "/v1/database/query" | "/v1/database/file") => {
            Err(Error::new("RDB_METHOD", "method is not supported"))
        }
        _ => Err(Error::new(
            "RDB_NOT_FOUND",
            "database endpoint does not exist",
        )),
    }
}

fn directory_route(method: &Method, path: &str, organization_id: &str) -> Result<Route> {
    let not_found = || Error::new("RDB_NOT_FOUND", "database endpoint does not exist");
    let rest = path
        .strip_prefix("/v1/organizations/")
        .and_then(|rest| rest.strip_prefix(organization_id))
        .and_then(|rest| rest.strip_prefix("/databases"))
        .ok_or_else(not_found)?;
    let segments: Vec<&str> = rest.split('/').skip(1).collect();
    if rest.is_empty() {
        return match *method {
            Method::GET => Ok(Route::List),
            Method::POST => Ok(Route::Create),
            _ => Err(Error::new("RDB_METHOD", "method is not supported")),
        };
    }
    match (method, segments.as_slice()) {
        (_, [id, ..]) if !valid_id(id) => Err(not_found()),
        (&Method::GET, [id]) => Ok(Route::Details((*id).to_owned())),
        (&Method::DELETE, [id]) => Ok(Route::Delete((*id).to_owned())),
        (&Method::POST, [id, "read"]) => Ok(Route::DatabaseRead((*id).to_owned())),
        (&Method::POST, [id, "query"]) => Ok(Route::DatabaseQuery((*id).to_owned())),
        (&Method::GET, [id, "file"]) => Ok(Route::DatabaseFile((*id).to_owned())),
        (_, [_] | [_, "read" | "query" | "file"]) => {
            Err(Error::new("RDB_METHOD", "method is not supported"))
        }
        _ => Err(not_found()),
    }
}

/// Read `?empty=a,b&identity=new`. Names are percent-encoded, and a name cannot
/// hold a comma.
fn file_options(query: Option<&str>) -> Result<romedb::FileExportOptions> {
    let mut options = romedb::FileExportOptions::default();
    let Some(query) = query else {
        return Ok(options);
    };
    if query.len() > MAX_FILE_QUERY_BYTES {
        return Err(Error::new("RDB_LIMIT", "file request query is too long"));
    }
    let invalid = || {
        Error::new(
            "RDB_VALIDATION",
            "file requests take empty and identity=new",
        )
    };
    for pair in query.split('&') {
        match pair.split_once('=').ok_or_else(invalid)? {
            ("empty", names) if options.empty_tables.is_empty() && !names.is_empty() => {
                options.empty_tables = names
                    .split(',')
                    .map(percent_decoded)
                    .collect::<Result<_>>()?;
            }
            ("identity", "new") if !options.new_identity => options.new_identity = true,
            _ => return Err(invalid()),
        }
    }
    Ok(options)
}

fn percent_decoded(text: &str) -> Result<String> {
    let invalid = || Error::new("RDB_VALIDATION", "invalid percent-encoded table name");
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        rest = tail;
        if byte != b'%' {
            bytes.push(byte);
            continue;
        }
        let digits = rest.get(..2).ok_or_else(invalid)?;
        let digits = std::str::from_utf8(digits).map_err(|_| invalid())?;
        bytes.push(u8::from_str_radix(digits, 16).map_err(|_| invalid())?);
        rest = &rest[2..];
    }
    String::from_utf8(bytes).map_err(|_| invalid())
}

fn host(request: &Request<Incoming>) -> Option<&str> {
    request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
}

fn response(status: StatusCode, body: String) -> Response<Body> {
    let mut builder = Response::builder().status(status);
    if status != StatusCode::NO_CONTENT {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONNECTION, "close")
        .header("x-content-type-options", "nosniff")
        .body(Either::Left(Full::new(Bytes::from(body))))
        .expect("fixed HTTP response")
}

fn file_response(download: FileDownload, permit: OwnedSemaphorePermit) -> Response<Body> {
    let FileDownload {
        file,
        bytes,
        sequence,
        database_id,
    } = download;
    let chunks = file_chunks(file, permit);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, DATABASE_FILE_TYPE)
        .header(header::CONTENT_LENGTH, bytes)
        .header("x-romedb-sequence", sequence)
        .header("x-romedb-database-id", database_id)
        .header(header::CACHE_CONTROL, "no-store")
        // No `connection: close`: the client ends this connection. Node's fetch
        // crashes the process if the socket closes while its reader is paused.
        .header("x-content-type-options", "nosniff")
        .body(Either::Right(FileBody {
            chunks,
            remaining: bytes,
        }))
        .expect("fixed HTTP response")
}

/// Reads a file in chunks on a blocking thread, which keeps `permit` until it ends.
fn file_chunks(
    mut file: File,
    permit: OwnedSemaphorePermit,
) -> mpsc::Receiver<std::io::Result<Bytes>> {
    let (sender, chunks) = mpsc::channel(4);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        loop {
            let mut chunk = vec![0; FILE_CHUNK_BYTES];
            let read = match file.read(&mut chunk) {
                Ok(0) => return,
                Ok(read) => read,
                Err(error) => {
                    let _ = sender.blocking_send(Err(error));
                    return;
                }
            };
            chunk.truncate(read);
            if sender.blocking_send(Ok(Bytes::from(chunk))).is_err() {
                return;
            }
        }
    });
    chunks
}

struct FileBody {
    chunks: mpsc::Receiver<std::io::Result<Bytes>>,
    remaining: u64,
}

impl hyper::body::Body for FileBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Bytes>, std::io::Error>>> {
        if self.remaining == 0 {
            return Poll::Ready(None);
        }
        match self.chunks.poll_recv(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(chunk))) if chunk.len() as u64 <= self.remaining => {
                self.remaining -= chunk.len() as u64;
                Poll::Ready(Some(Ok(Frame::data(chunk))))
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error))),
            Poll::Ready(_) => Poll::Ready(Some(Err(std::io::Error::other(
                "database file size changed while it was sent",
            )))),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.remaining == 0
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining)
    }
}
