#![cfg(unix)]
use romedb_service::{
    directory::DirectoryService,
    http::{serve_directory, Token},
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::Path,
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

struct Server {
    address: SocketAddr,
    token: String,
    organization: String,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<romedb_service::Result<()>>,
}

impl Server {
    async fn start(root: &Path, max_open: usize) -> Self {
        let directory = Arc::new(DirectoryService::open(root, true, max_open).unwrap());
        let organization = directory.organization_id().to_owned();
        let token_path = root.join("romedb.token");
        let auth = Token::load_or_create(&token_path).unwrap();
        let token = std::fs::read_to_string(&token_path)
            .unwrap()
            .trim_end()
            .to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(serve_directory(listener, directory, auth, async {
            let _ = stopped.await;
        }));
        Self {
            address,
            token,
            organization,
            stop: Some(stop),
            task,
        }
    }

    async fn stop(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        self.task.await.unwrap().unwrap();
    }

    fn path(&self, rest: &str) -> String {
        format!("/v1/organizations/{}/databases{rest}", self.organization)
    }

    async fn send(&self, method: &str, path: &str, extra: &str, body: &str) -> (u16, Value) {
        let request = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}", self.address, self.token, body.len());
        let address = self.address;
        tokio::task::spawn_blocking(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(20)))
                .unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
            let (_, body) = response.split_once("\r\n\r\n").unwrap();
            (
                status,
                if body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_str(body).unwrap()
                },
            )
        })
        .await
        .unwrap()
    }

    async fn download(&self, path: &str) -> (u16, String, Vec<u8>) {
        let request = format!("GET {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n", self.address, self.token);
        let address = self.address;
        tokio::task::spawn_blocking(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(20)))
                .unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let head = String::from_utf8(response[..split].to_vec()).unwrap();
            let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
            (
                status,
                head.to_ascii_lowercase(),
                response[split + 4..].to_vec(),
            )
        })
        .await
        .unwrap()
    }

    async fn create(&self, name: &str) -> (u16, Value) {
        self.send(
            "POST",
            &self.path(""),
            "",
            &json!({ "name": name }).to_string(),
        )
        .await
    }
}

fn write(request_id: &str, sql: &str) -> String {
    json!({"requestId":request_id,"afterSequence":0,"statements":[{"sql":sql}]}).to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_serves_cloud_database_routes_and_keeps_databases_across_restarts() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(root.path(), 2).await;
    let (status, first) = server.create("site-one").await;
    assert_eq!(status, 201, "{first}");
    let id = first["id"].as_str().unwrap().to_owned();
    assert_eq!(first["name"], "site-one");
    assert_eq!(first["organizationId"], server.organization.as_str());
    assert_eq!(first["status"], "active");
    assert_eq!(
        first["url"],
        format!(
            "http://{}{}/query",
            server.address,
            server.path(&format!("/{id}"))
        )
    );
    assert!(root.path().join(format!("{id}.romedb")).is_file());
    assert_eq!(server.create("site-one").await, (201, first.clone()));
    assert_eq!(
        server.create("Site").await,
        (400, json!({"error":"Invalid database name"}))
    );
    let mut ids = vec![id.clone()];
    for name in ["site-two", "site-three"] {
        let (status, created) = server.create(name).await;
        assert_eq!(status, 201);
        ids.push(created["id"].as_str().unwrap().to_owned());
    }
    let setup = write("setup", "CREATE TABLE posts");
    for id in &ids {
        let (status, reply) = server
            .send("POST", &server.path(&format!("/{id}/query")), "", &setup)
            .await;
        assert_eq!((status, &reply["sequence"]), (200, &json!(1)), "{reply}");
    }
    let (status, replay) = server
        .send("POST", &server.path(&format!("/{id}/query")), "", &setup)
        .await;
    assert_eq!((status, &replay["sequence"]), (200, &json!(1)));
    let (status, listed) = server.send("GET", &server.path(""), "", "").await;
    assert_eq!(status, 200);
    let names: Vec<&str> = listed["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["site-one", "site-two", "site-three"]);
    let (status, details) = server
        .send("GET", &server.path(&format!("/{id}")), "", "")
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        (&details["id"], &details["name"], &details["sequence"]),
        (&json!(id), &json!("site-one"), &json!(1))
    );
    assert_eq!(details["serviceVersion"], 1);
    assert_eq!(details["journalMode"], "mvcc");
    assert_eq!(details["engineVersion"], romedb::VERSION);
    let missing = "00000000-0000-4000-8000-000000000000";
    assert_eq!(
        server
            .send("GET", &server.path(&format!("/{missing}")), "", "")
            .await,
        (404, json!({"error":"Database not found"}))
    );
    let other = format!("/v1/organizations/{missing}/databases");
    assert_eq!(server.send("GET", &other, "", "").await.0, 404);
    assert_eq!(
        server
            .send("PUT", &server.path(&format!("/{id}")), "", "")
            .await
            .0,
        405
    );
    let (status, wrong) = server
        .send(
            "POST",
            &server.path(&format!("/{id}/read")),
            &format!("x-romedb-database-id: {}\r\n", ids[1]),
            &json!({"readVersion":2,"requestId":"read","afterSequence":0,"statements":[{"sql":"SELECT 1"}]}).to_string(),
        )
        .await;
    assert_eq!(
        (status, &wrong["error"]["code"]),
        (409, &json!("RDB_REQUEST_IDENTITY"))
    );
    let removed = ids.remove(1);
    assert_eq!(
        server
            .send("DELETE", &server.path(&format!("/{removed}")), "", "")
            .await,
        (204, Value::Null)
    );
    assert!(!root.path().join(format!("{removed}.romedb")).exists());
    assert!(!root.path().join(format!("{removed}.db-log")).exists());
    assert_eq!(
        server
            .send(
                "POST",
                &server.path(&format!("/{removed}/query")),
                "",
                &setup
            )
            .await
            .0,
        404
    );
    assert_eq!(
        DirectoryService::open(root.path(), false, 2)
            .err()
            .unwrap()
            .code,
        "RDB_BUSY"
    );
    let organization = server.organization.clone();
    server.stop().await;
    for id in &ids {
        assert_eq!(
            std::fs::metadata(root.path().join(format!("{id}.db-log")))
                .unwrap()
                .len(),
            0
        );
        romedb::Database::open_mvcc(root.path().join(format!("{id}.romedb")).to_str().unwrap())
            .unwrap();
    }
    let server = Server::start(root.path(), 1).await;
    assert_eq!(server.organization, organization);
    let (_, listed) = server.send("GET", &server.path(""), "", "").await;
    assert_eq!(listed["databases"].as_array().unwrap().len(), 2);
    let (status, replay) = server
        .send("POST", &server.path(&format!("/{id}/query")), "", &setup)
        .await;
    assert_eq!((status, &replay["sequence"]), (200, &json!(1)));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_adopts_new_files_and_refuses_copies_and_wal_files() {
    let root = tempfile::tempdir().unwrap();
    let adopted = "11111111-1111-4111-8111-111111111111";
    let adopted_path = root.path().join(format!("{adopted}.romedb"));
    std::fs::File::create(&adopted_path).unwrap();
    romedb::Database::create_mvcc(adopted_path.to_str().unwrap()).unwrap();
    let legacy = "22222222-2222-4222-8222-222222222222";
    romedb::Database::open(
        root.path()
            .join(format!("{legacy}.romedb"))
            .to_str()
            .unwrap(),
    )
    .unwrap();
    std::fs::write(root.path().join("notes.romedb"), b"").unwrap();
    let server = Server::start(root.path(), 4).await;
    let (_, listed) = server.send("GET", &server.path(""), "", "").await;
    let mut names: Vec<&str> = listed["databases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, [format!("db-{adopted}"), format!("db-{legacy}")]);
    let (status, reply) = server
        .send(
            "POST",
            &server.path(&format!("/{adopted}/query")),
            "",
            &write("adopted", "CREATE TABLE posts"),
        )
        .await;
    assert_eq!(status, 200, "{reply}");
    let (status, refused) = server
        .send("GET", &server.path(&format!("/{legacy}")), "", "")
        .await;
    assert_eq!(status, 400);
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("migrate-to-mvcc"),
        "{refused}"
    );
    server.stop().await;
    let copy = "33333333-3333-4333-8333-333333333333";
    std::fs::copy(&adopted_path, root.path().join(format!("{copy}.romedb"))).unwrap();
    let server = Server::start(root.path(), 4).await;
    let (status, refused) = server
        .send("GET", &server.path(&format!("/{copy}")), "", "")
        .await;
    assert_eq!(status, 409, "{refused}");
    assert!(refused["error"]
        .as_str()
        .unwrap()
        .contains("another database's identity"));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_database_downloads_as_a_file_that_serves_alone() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let server = Server::start(root.path(), 2).await;
    let (_, created) = server.create("site-one").await;
    let id = created["id"].as_str().unwrap().to_owned();
    for (index, sql) in ["CREATE TABLE posts", "INSERT INTO posts {title:'kept'}"]
        .into_iter()
        .enumerate()
    {
        let body = json!({"requestId":format!("w{index}"),"afterSequence":index,"statements":[{"sql":sql}]}).to_string();
        let (status, reply) = server
            .send("POST", &server.path(&format!("/{id}/query")), "", &body)
            .await;
        assert_eq!(status, 200, "{reply}");
    }
    let (status, head, body) = server.download(&server.path(&format!("/{id}/file"))).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert!(head.contains("\r\nx-romedb-sequence: 2\r\n"), "{head}");
    assert!(
        head.contains(&format!("\r\nx-romedb-database-id: {id}\r\n")),
        "{head}"
    );
    assert!(
        head.contains(&format!("\r\ncontent-length: {}\r\n", body.len())),
        "{head}"
    );
    let missing = "00000000-0000-4000-8000-000000000000";
    let (status, reply) = server
        .send("GET", &server.path(&format!("/{missing}/file")), "", "")
        .await;
    assert_eq!(status, 404);
    assert_eq!(reply["error"]["code"], "RDB_NOT_FOUND", "{reply}");
    assert_eq!(
        server
            .send("POST", &server.path(&format!("/{id}/file")), "", "{}")
            .await
            .0,
        405
    );
    let mut names: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            format!("{id}.db-log"),
            format!("{id}.romedb"),
            format!("{id}.romedb-wal"),
            "romedb-directory.json".into(),
            "romedb-directory.lock".into(),
            "romedb.token".into()
        ]
    );
    server.stop().await;

    let copy = elsewhere.path().join("site.romedb");
    std::fs::write(&copy, &body).unwrap();
    let served = romedb_service::DatabaseService::open(&copy).unwrap();
    assert_eq!(served.info().unwrap()["id"], id.as_str());
    let read = json!({"readVersion":2,"requestId":"r","afterSequence":2,"statements":[{"sql":"SELECT title FROM posts"}]}).to_string();
    let reply: Value = serde_json::from_str(&served.read(&read).unwrap()).unwrap();
    assert_eq!(reply["results"][0]["rows"][0][0]["value"], "kept");
}
