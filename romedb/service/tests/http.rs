#![cfg(unix)]
use romedb_service::{
    http::{serve, Token},
    DatabaseService,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    os::unix::fs::{symlink, PermissionsExt},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot};

async fn send(
    address: SocketAddr,
    token: &str,
    method: &str,
    path: &str,
    extra: &str,
    body: &str,
) -> (u16, Value) {
    let request = format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}", body.len());
    tokio::task::spawn_blocking(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        (status, serde_json::from_str(body).unwrap())
    })
    .await
    .unwrap()
}

async fn send_declared_length(address: SocketAddr, token: &str, path: &str, length: usize) -> u16 {
    let request = format!("POST {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n");
    tokio::task::spawn_blocking(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response.split_whitespace().nth(1).unwrap().parse().unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_http_requires_token_and_rejects_browser_access() {
    let directory = tempfile::tempdir().unwrap();
    let database = Arc::new(DatabaseService::create(&directory.path().join("app.romedb")).unwrap());
    let token_path = directory.path().join("service.token");
    let auth = Token::load_or_create(&token_path).unwrap();
    let token = std::fs::read_to_string(&token_path)
        .unwrap()
        .trim_end()
        .to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let server = tokio::spawn(serve(listener, database.clone(), auth, async {
        let _ = stopped.await;
    }));
    assert_eq!(
        send(address, "wrong", "GET", "/v1/database", "", "")
            .await
            .0,
        401
    );
    for extra in [
        "Origin: http://127.0.0.1\r\n",
        "Cookie: session=anything\r\n",
        "Sec-Fetch-Site: same-origin\r\n",
    ] {
        assert_eq!(
            send(address, &token, "GET", "/v1/database", extra, "")
                .await
                .0,
            403
        );
    }
    assert_eq!(
        send(address, &token, "GET", "/v1/database?token=bad", "", "")
            .await
            .0,
        400
    );
    assert_eq!(
        send(address, &token, "POST", "/v1/database", "", "")
            .await
            .0,
        405
    );
    assert_eq!(
        send(address, &token, "GET", "/missing", "", "").await.0,
        404
    );
    let details = send(address, &token, "GET", "/v1/database", "", "").await.1;
    assert_eq!(details["sequence"], 0);
    assert_eq!(details["engineVersion"], romedb::VERSION);
    assert_eq!(
        send_declared_length(
            address,
            &token,
            "/v1/database/query",
            romedb::MAX_REQUEST_BYTES + 1
        )
        .await,
        413
    );
    let setup = json!({"requestId":"large-setup","afterSequence":0,"statements":[{"sql":"CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)"}]}).to_string();
    assert_eq!(
        send(address, &token, "POST", "/v1/database/query", "", &setup)
            .await
            .0,
        200
    );
    let text = "x".repeat(2 * 1024 * 1024);
    let large = json!({"requestId":"large","afterSequence":1,"statements":[{"sql":"INSERT INTO notes VALUES(1, $body) RETURNING length(body)","params":{"$body":{"type":"String","value":text}}}]}).to_string();
    assert!(large.len() > 2 * 1024 * 1024);
    let (status, reply) = send(address, &token, "POST", "/v1/database/query", "", &large).await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(
        reply["results"][0]["rows"][0][0],
        json!({"type":"Integer","value":"2097152"})
    );
    let rows = |limit: u32| {
        json!({"readVersion":2,"requestId":"rows","afterSequence":2,"statements":[{"sql":format!("WITH RECURSIVE t(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM t WHERE n<{limit}) SELECT n FROM t")}]}).to_string()
    };
    let declared = "X-RomeDB-Result-Limits: rows=10000; bytes=8388608\r\n";
    let (status, earlier) = send(
        address,
        &token,
        "POST",
        "/v1/database/read",
        "",
        &rows(1001),
    )
    .await;
    assert_eq!(
        (status, &earlier["error"]["code"]),
        (413, &json!("RDB_LIMIT")),
        "{earlier}"
    );
    let (status, raised) = send(
        address,
        &token,
        "POST",
        "/v1/database/read",
        declared,
        &rows(10_000),
    )
    .await;
    assert_eq!(status, 200, "{raised}");
    assert_eq!(
        raised["results"][0]["rows"].as_array().unwrap().len(),
        10_000
    );
    let invalid = "X-RomeDB-Result-Limits: rows=10000\r\n";
    assert_eq!(
        send(
            address,
            &token,
            "POST",
            "/v1/database/read",
            invalid,
            &rows(1)
        )
        .await
        .0,
        400
    );
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert_eq!(database.info().unwrap_err().code, "RDB_CLOSED");
    assert!(TcpStream::connect(address).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writes_and_replay_work_over_http_across_service_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("app.romedb");
    let token_path = directory.path().join("service.token");
    let mut created = Some(DatabaseService::create(&path).unwrap());
    let mut previous = None;
    let write = json!({"requestId":"setup","afterSequence":0,"statements":[{"sql":"CREATE TABLE docs;"},{"sql":"INSERT INTO docs {id:docs:p1,title:'original'}"}]}).to_string();
    for index in 0..2 {
        let database = if index == 0 {
            created.take().unwrap()
        } else {
            DatabaseService::open(&path).unwrap()
        };
        let auth = Token::load_or_create(&token_path).unwrap();
        let token = std::fs::read_to_string(&token_path)
            .unwrap()
            .trim_end()
            .to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let owner = Arc::new(database);
        let server = tokio::spawn(serve(listener, owner, auth, async {
            let _ = stopped.await;
        }));
        let (status, response) =
            send(address, &token, "POST", "/v1/database/query", "", &write).await;
        assert_eq!(status, 200);
        assert!(response["error"].is_null(), "{response}");
        if let Some(previous) = previous {
            assert_eq!(response, previous);
        }
        previous = Some(response);
        let read = json!({"requestId":"read","afterSequence":1,"readVersion":2,"statements":[{"sql":"SELECT title FROM docs"}]}).to_string();
        let (status, response) =
            send(address, &token, "POST", "/v1/database/read", "", &read).await;
        assert_eq!(status, 200);
        assert_eq!(response["results"][0]["rows"][0][0]["value"], "original");
        stop.send(()).unwrap();
        server.await.unwrap().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_requests_cannot_cross_database_identity_when_a_token_is_reused() {
    let directory = tempfile::tempdir().unwrap();
    let previous = DatabaseService::create(&directory.path().join("previous.romedb")).unwrap();
    let previous_id = previous.info().unwrap()["id"].as_str().unwrap().to_owned();
    drop(previous);
    let database =
        Arc::new(DatabaseService::create(&directory.path().join("replacement.romedb")).unwrap());
    let current_id = database.info().unwrap()["id"].as_str().unwrap().to_owned();
    assert_ne!(previous_id, current_id);
    let token_path = directory.path().join("shared.token");
    let auth = Token::load_or_create(&token_path).unwrap();
    let token = std::fs::read_to_string(&token_path)
        .unwrap()
        .trim_end()
        .to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let server = tokio::spawn(serve(listener, database.clone(), auth, async {
        let _ = stopped.await;
    }));
    let body = json!({"requestId":"saved","afterSequence":0,"statements":[{"sql":"CREATE TABLE unexpected;"}]}).to_string();
    let (status, reply) = send(
        address,
        &token,
        "POST",
        "/v1/database/query",
        &format!("X-RomeDB-Database-ID: {previous_id}\r\n"),
        &body,
    )
    .await;
    assert_eq!(status, 409, "{reply}");
    assert_eq!(reply["error"]["code"], "RDB_REQUEST_IDENTITY");
    assert_eq!(database.info().unwrap()["sequence"], 0);
    let read = json!({"readVersion":2,"requestId":"read","afterSequence":0,"statements":[{"sql":"INFO FOR DB"}]}).to_string();
    assert_eq!(
        send(
            address,
            &token,
            "POST",
            "/v1/database/read",
            &format!("X-RomeDB-Database-ID: {previous_id}\r\n"),
            &read
        )
        .await
        .0,
        409
    );
    assert_eq!(
        send(
            address,
            &token,
            "POST",
            "/v1/database/query",
            &format!(
                "X-RomeDB-Database-ID: {current_id}\r\nX-RomeDB-Database-ID: {current_id}\r\n"
            ),
            &body
        )
        .await
        .0,
        409
    );
    assert_eq!(database.info().unwrap()["sequence"], 0);
    let (status, reply) = send(
        address,
        &token,
        "POST",
        "/v1/database/query",
        &format!("X-RomeDB-Database-ID: {current_id}\r\n"),
        &body,
    )
    .await;
    assert_eq!(status, 200);
    assert!(reply["error"].is_null(), "{reply}");
    assert_eq!(reply["sequence"], 1);
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

fn download(address: SocketAddr, token: &str, path: &str) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(response[..split].to_vec()).unwrap();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(": ").unwrap();
            (name.to_ascii_lowercase(), value.to_owned())
        })
        .collect();
    (status, headers, response[split + 4..].to_vec())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or_else(|| panic!("missing {name}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_database_file_downloads_at_one_sequence_while_writes_continue() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("app.romedb");
    std::fs::File::create(&path).unwrap();
    {
        let database = romedb::Database::create_mvcc(path.to_str().unwrap()).unwrap();
        let c = database.connect().unwrap();
        let body = romedb::Value::String("x".repeat(2000));
        for sql in ["CREATE TABLE posts", "CREATE TABLE marks", "BEGIN"] {
            c.execute(sql, &romedb::Parameters::new()).unwrap();
        }
        for n in 0..5000 {
            c.insert(
                "posts",
                romedb::Document::from([
                    ("n".into(), romedb::Value::Integer(n)),
                    ("body".into(), body.clone()),
                ]),
            )
            .unwrap();
        }
        c.execute("COMMIT", &romedb::Parameters::new()).unwrap();
    }
    let database = Arc::new(DatabaseService::open(&path).unwrap());
    let id = database.info().unwrap()["id"].as_str().unwrap().to_owned();
    let token_path = directory.path().join("service.token");
    let auth = Token::load_or_create(&token_path).unwrap();
    let token = std::fs::read_to_string(&token_path)
        .unwrap()
        .trim_end()
        .to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let server = tokio::spawn(serve(listener, database.clone(), auth, async {
        let _ = stopped.await;
    }));
    assert_eq!(
        send(address, &token, "POST", "/v1/database/file", "", "{}")
            .await
            .0,
        405
    );
    let writing = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let writer = std::thread::spawn({
        let (database, writing) = (database.clone(), writing.clone());
        move || {
            let mut sequence = 0;
            let mut written = Vec::new();
            let mut n = 0;
            while writing.load(std::sync::atomic::Ordering::SeqCst) || n < 3 {
                n += 1;
                let body = json!({"requestId":format!("mark-{n}"),"afterSequence":sequence,"statements":[{"sql":format!("INSERT INTO marks {{n:{n}}}")}]}).to_string();
                let Ok(reply) = database.query(&body) else {
                    continue;
                };
                let reply: Value = serde_json::from_str(&reply).unwrap();
                sequence = reply["sequence"].as_u64().unwrap();
                if reply["error"].is_null() {
                    written.push((n, sequence));
                }
            }
            written
        }
    });
    while database.info().unwrap()["sequence"].as_u64().unwrap() < 3 {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let (status, headers, body) = {
        let token = token.clone();
        tokio::task::spawn_blocking(move || download(address, &token, "/v1/database/file"))
            .await
            .unwrap()
    };
    let during = database.info().unwrap()["sequence"].as_u64().unwrap();
    writing.store(false, std::sync::atomic::Ordering::SeqCst);
    let written = writer.join().unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        header(&headers, "content-type"),
        "application/vnd.romedb.database"
    );
    assert_eq!(header(&headers, "content-length"), body.len().to_string());
    assert_eq!(header(&headers, "x-romedb-database-id"), id);
    assert_eq!(header(&headers, "cache-control"), "no-store");
    let sequence: u64 = header(&headers, "x-romedb-sequence").parse().unwrap();
    assert!(sequence >= 3 && sequence < during, "{sequence} {during}");
    let names: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with('.'))
        .collect();
    assert!(names.is_empty(), "{names:?}");

    let copy = directory.path().join("copy.romedb");
    std::fs::write(&copy, &body).unwrap();
    let served = DatabaseService::open(&copy).unwrap();
    assert_eq!(served.info().unwrap()["sequence"], sequence);
    assert_eq!(served.info().unwrap()["id"], id);
    let read = json!({"readVersion":2,"requestId":"read","afterSequence":sequence,"statements":[{"sql":"SELECT n FROM marks ORDER BY n"},{"sql":"SELECT count() FROM posts"}]}).to_string();
    let reply: Value = serde_json::from_str(&served.read(&read).unwrap()).unwrap();
    let expected: Vec<Value> = written
        .iter()
        .filter(|(_, at)| *at <= sequence)
        .map(|(n, _)| json!([{"type":"Integer","value":n.to_string()}]))
        .collect();
    assert!(!expected.is_empty());
    assert_eq!(reply["results"][0]["rows"], Value::Array(expected));
    assert_eq!(reply["results"][1]["rows"][0][0]["value"], "5000");
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[test]
fn token_files_are_private_exclusive_and_reusable() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("token");
    Token::load_or_create(&path).unwrap();
    let original = std::fs::read(&path).unwrap();
    Token::load_or_create(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Token::load_or_create(&path).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let linked = directory.path().join("symlink");
    symlink(&path, &linked).unwrap();
    assert!(Token::load_or_create(&linked).is_err());
    let hardlink = directory.path().join("hardlink");
    std::fs::hard_link(&path, &hardlink).unwrap();
    assert!(Token::load_or_create(&path).is_err());
}
