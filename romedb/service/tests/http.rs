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
    let body = "x".repeat(65537);
    assert_eq!(
        send(address, &token, "POST", "/v1/database/query", "", &body)
            .await
            .0,
        413
    );
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert_eq!(database.info().unwrap_err().code, "FDB_CLOSED");
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
    assert_eq!(reply["error"]["code"], "FDB_REQUEST_IDENTITY");
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
