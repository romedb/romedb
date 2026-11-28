#![cfg(unix)]
use romedb_service::{
    directory::DirectoryService,
    http::{serve, serve_directory, Token},
    DatabaseService,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

const MIB: usize = 1024 * 1024;
const PAGE: &str =
    "Origin: http://localhost:5173\r\nSec-Fetch-Site: cross-site\r\nSec-Fetch-Mode: cors\r\n";

struct Server {
    address: SocketAddr,
    token: String,
    /// The database URL without its origin.
    base: String,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<romedb_service::Result<()>>,
}

struct Reply {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&self.body)))
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().find_map(|line| {
            let (found, value) = line.split_once(": ")?;
            found.eq_ignore_ascii_case(name).then_some(value)
        })
    }
}

impl Server {
    async fn database(folder: &Path) -> Self {
        let database = Arc::new(DatabaseService::create(&folder.join("app.romedb")).unwrap());
        let (auth, token) = token(&folder.join("service.token"));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(serve(listener, database, auth, async {
            let _ = stopped.await;
        }));
        Self {
            address,
            token,
            base: "/v1/database".into(),
            stop: Some(stop),
            task,
        }
    }

    async fn directory(root: &Path) -> Self {
        let directory = Arc::new(DirectoryService::open(root, true, 32).unwrap());
        let base = format!("/v1/organizations/{}", directory.organization_id());
        let (auth, token) = token(&root.join("romedb.token"));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(serve_directory(listener, directory, auth, async {
            let _ = stopped.await;
        }));
        Self {
            address,
            token,
            base,
            stop: Some(stop),
            task,
        }
    }

    async fn stop(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        self.task.await.unwrap().unwrap();
    }

    /// One request as raw HTTP. `path` is under the database URL, or a full URL of
    /// this server. `headers` are whole header lines.
    async fn send(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        let origin = format!("http://{}", self.address);
        let path = match path.strip_prefix(&origin) {
            Some(path) => path.to_owned(),
            None => format!("{}{path}", self.base),
        };
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n",
            self.address,
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(body);
        let address = self.address;
        tokio::task::spawn_blocking(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(30)))
                .unwrap();
            stream.write_all(&request).unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            let end = response
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap();
            let head = String::from_utf8(response[..end].to_vec()).unwrap();
            Reply {
                status: head.split_whitespace().nth(1).unwrap().parse().unwrap(),
                head,
                body: response[end + 4..].to_vec(),
            }
        })
        .await
        .unwrap()
    }

    /// A JSON request with the service token.
    async fn call(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        self.with_token(&self.token, method, path, body).await
    }

    async fn with_token(&self, token: &str, method: &str, path: &str, body: Value) -> (u16, Value) {
        let body = if body.is_null() {
            String::new()
        } else {
            body.to_string()
        };
        let headers =
            format!("Authorization: Bearer {token}\r\nContent-Type: application/json\r\n");
        let reply = self.send(method, path, &headers, body.as_bytes()).await;
        (reply.status, reply.json())
    }

    async fn open_upload(&self, name: &str, bytes: &[u8], more: Value) -> Value {
        let mut declaration =
            json!({"id": uuid::Uuid::new_v4().to_string(), "name": name, "size": bytes.len()});
        for (key, value) in more.as_object().unwrap() {
            declaration[key] = value.clone();
        }
        let (status, session) = self.call("POST", "/files/uploads", declaration).await;
        assert_eq!(status, 201, "{session}");
        session
    }

    /// Sends every part with the session token only and completes the upload.
    async fn upload(&self, name: &str, bytes: &[u8], more: Value) -> Value {
        let session = self.open_upload(name, bytes, more).await;
        let (url, token) = (
            session["url"].as_str().unwrap(),
            session["token"].as_str().unwrap(),
        );
        let numbers: Vec<u64> = (1..=session["parts"].as_u64().unwrap()).collect();
        let (status, urls) = self
            .with_token(
                token,
                "POST",
                &format!("{url}/parts"),
                json!({"parts": numbers}),
            )
            .await;
        assert_eq!(status, 200, "{urls}");
        let size = session["partSize"].as_u64().unwrap() as usize;
        for (part, chunk) in urls["parts"]
            .as_array()
            .unwrap()
            .iter()
            .zip(bytes.chunks(size))
        {
            let stored = self
                .send("PUT", part["url"].as_str().unwrap(), "", chunk)
                .await;
            assert_eq!(stored.status, 200, "{}", stored.json());
        }
        let (status, done) = self
            .with_token(
                token,
                "POST",
                &format!("{url}/complete"),
                json!({"crc64nvme": crc64_nvme(bytes)}),
            )
            .await;
        assert_eq!(status, 200, "{done}");
        done["file"].clone()
    }

    async fn query(&self, path: &str, sql: &str) -> Value {
        let (_, details) = self
            .call(
                "GET",
                path.trim_end_matches("/query").trim_end_matches("/read"),
                Value::Null,
            )
            .await;
        let read = path.ends_with("/read");
        let mut request = json!({"requestId": uuid::Uuid::new_v4().simple().to_string(), "afterSequence": details["sequence"], "statements": [{"sql": sql}]});
        if read {
            request["readVersion"] = json!(2);
        }
        let (status, reply) = self.call("POST", path, request).await;
        assert_eq!(status, 200, "{reply}");
        assert!(reply["error"].is_null(), "{reply}");
        reply["results"][0].clone()
    }
}

fn token(path: &Path) -> (Token, String) {
    let auth = Token::load_or_create(path).unwrap();
    (
        auth,
        std::fs::read_to_string(path).unwrap().trim_end().to_owned(),
    )
}

/// CRC-64/NVME one bit at a time, apart from the table the service uses.
fn crc64_nvme(bytes: &[u8]) -> String {
    let mut crc = !0u64;
    for byte in bytes {
        crc ^= *byte as u64;
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0x9a6c_9329_ac4b_c9b5
            } else {
                crc >> 1
            };
        }
    }
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let value = (!crc).to_be_bytes();
    let mut text = String::new();
    for group in value.chunks(3) {
        let bits = group.iter().enumerate().fold(0u32, |bits, (at, byte)| {
            bits | ((*byte as u32) << (16 - 8 * at))
        });
        for at in 0..4 {
            if at <= group.len() {
                text.push(ALPHABET[((bits >> (18 - 6 * at)) & 63) as usize] as char);
            } else {
                text.push('=');
            }
        }
    }
    text
}

fn pattern(length: usize) -> Vec<u8> {
    (0..length).map(|at| (at * 31 % 251) as u8).collect()
}

fn objects(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(folder.join("objects")) {
        Ok(listed) => listed
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

fn files_folder(folder: &Path) -> PathBuf {
    folder.join("app.romedb.files")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_uploads_two_parts_with_the_session_token_and_the_file_downloads_again() {
    let folder = tempfile::tempdir().unwrap();
    let server = Server::database(folder.path()).await;
    let bytes = pattern(8 * MIB + 1000);
    let session = server
        .open_upload(
            "Report 2026.pdf",
            &bytes,
            json!({"mediaType": "application/pdf", "subject": "user-1", "metadata": {"year": 2026, "kind": "report"}}),
        )
        .await;
    let upload_id = session["id"].as_str().unwrap();
    let token = session["token"].as_str().unwrap();
    assert!(token.starts_with("rdbu_") && token.len() == 48, "{token}");
    assert!(token[5..]
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
    let url = session["url"].as_str().unwrap();
    assert_eq!(
        url,
        format!(
            "http://{}/v1/database/files/uploads/{upload_id}",
            server.address
        )
    );
    assert_eq!(session["state"], "uploading");
    assert_eq!(session["partSize"], 8 * MIB);
    assert_eq!(session["parts"], 2);
    assert_eq!(session["size"], bytes.len());
    assert_eq!(session["mediaType"], "application/pdf");
    assert_eq!(session["databaseId"], session["organizationId"]);
    let file_id = session["fileId"].as_str().unwrap();
    assert_eq!(&file_id[14..15], "7", "{file_id}");

    let page = format!("Authorization: Bearer {token}\r\nContent-Type: application/json\r\n{PAGE}");
    let status = server.send("GET", url, &page, b"").await;
    assert_eq!(status.status, 200);
    assert_eq!(status.header("access-control-allow-origin"), Some("*"));
    let status = status.json();
    assert_eq!(status["uploadedParts"], json!([]));
    assert!(status.get("token").is_none(), "{status}");

    let asked = server
        .send(
            "POST",
            &format!("{url}/parts"),
            &page,
            br#"{"parts":[1,2]}"#,
        )
        .await;
    assert_eq!(asked.status, 200, "{}", asked.json());
    let parts = asked.json()["parts"].clone();
    assert_eq!(parts[0]["size"], 8 * MIB);
    assert_eq!(parts[1]["size"], 1000);
    assert!(parts[0]["url"]
        .as_str()
        .unwrap()
        .starts_with(&format!("{url}/parts/1?expires=")));
    for (part, chunk) in [
        (&parts[0], &bytes[..8 * MIB]),
        (&parts[1], &bytes[8 * MIB..]),
    ] {
        let stored = server
            .send("PUT", part["url"].as_str().unwrap(), PAGE, chunk)
            .await;
        assert_eq!(stored.status, 200, "{}", stored.json());
        assert_eq!(stored.header("access-control-allow-origin"), Some("*"));
    }
    let status = server.send("GET", url, &page, b"").await.json();
    assert_eq!(status["uploadedParts"], json!([1, 2]));

    let complete = json!({"crc64nvme": crc64_nvme(&bytes)}).to_string();
    let done = server
        .send(
            "POST",
            &format!("{url}/complete"),
            &page,
            complete.as_bytes(),
        )
        .await;
    assert_eq!(done.status, 200, "{}", done.json());
    let file = done.json()["file"].clone();
    assert_eq!(file["id"], file_id);
    assert_eq!(file["status"], "ready");
    assert_eq!(file["name"], "Report 2026.pdf");
    assert_eq!(file["size"], bytes.len());
    assert_eq!(file["mediaType"], "application/pdf");
    assert_eq!(file["createdBy"], "user-1");
    assert_eq!(file["metadata"], json!({"year": 2026, "kind": "report"}));
    assert_eq!(
        file["checksum"],
        json!({"algorithm": "crc64nvme", "value": crc64_nvme(&bytes)})
    );
    assert_eq!(file["generation"].as_str().unwrap().len(), 32);
    let again = server
        .send("POST", &format!("{url}/complete"), &page, b"{}")
        .await;
    assert_eq!(again.json()["file"], file);
    let status = server.send("GET", url, &page, b"").await.json();
    assert_eq!(status["state"], "ready");
    assert_eq!(status["file"], file);
    assert!(!files_folder(folder.path())
        .join("uploads")
        .join(upload_id)
        .exists());
    assert_eq!(objects(&files_folder(folder.path())), [file_id]);

    let (status, download) = server
        .call("POST", &format!("/files/{file_id}/download"), json!({}))
        .await;
    assert_eq!(status, 200, "{download}");
    let content = server
        .send("GET", download["url"].as_str().unwrap(), PAGE, b"")
        .await;
    assert_eq!(content.status, 200);
    assert!(content.body == bytes);
    assert_eq!(content.header("content-type"), Some("application/pdf"));
    assert_eq!(
        content.header("content-length"),
        Some(bytes.len().to_string().as_str())
    );
    assert_eq!(
        content.header("content-disposition"),
        Some("attachment; filename=\"Report 2026.pdf\"; filename*=UTF-8''Report%202026.pdf")
    );
    assert_eq!(content.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(content.header("access-control-allow-origin"), Some("*"));
    let (_, inline) = server
        .call(
            "POST",
            &format!("/files/{file_id}/download"),
            json!({"disposition": "inline", "fileName": "été.pdf", "expiresInSeconds": 60}),
        )
        .await;
    let content = server
        .send("GET", inline["url"].as_str().unwrap(), "", b"")
        .await;
    assert_eq!(
        content.header("content-disposition"),
        Some("inline; filename=\"_t_.pdf\"; filename*=UTF-8''%C3%A9t%C3%A9.pdf")
    );
    assert!(content.body == bytes);

    let row = server
        .query(
            "/read",
            "SELECT id, name, description, tags, metadata, size, mediaType, checksum, generation, status, createdBy, createdAt, updatedAt, source FROM romedb_files",
        )
        .await;
    let text = |value: &str| json!({"type": "String", "value": value});
    assert_eq!(row["rows"].as_array().unwrap().len(), 1);
    let row = &row["rows"][0];
    assert_eq!(
        row[0],
        json!({"type": "Record", "value": {"table": "romedb_files", "key": {"type": "String", "value": file_id}}})
    );
    assert_eq!(row[1], text("Report 2026.pdf"));
    assert_eq!(row[2], text(""));
    assert_eq!(row[3], json!({"type": "Array", "value": []}));
    assert_eq!(
        row[4],
        json!({"type": "Object", "value": {"kind": text("report"), "year": {"type": "Integer", "value": "2026"}}})
    );
    assert_eq!(
        row[5],
        json!({"type": "Integer", "value": bytes.len().to_string()})
    );
    assert_eq!(row[6], text("application/pdf"));
    assert_eq!(row[7], text(&crc64_nvme(&bytes)));
    assert_eq!(row[8], text(file["generation"].as_str().unwrap()));
    assert_eq!(row[9], text("ready"));
    assert_eq!(row[10], text("user-1"));
    assert_eq!(
        row[11],
        json!({"type": "Integer", "value": file["createdAt"].to_string()})
    );
    assert_eq!(row[12], row[11]);
    assert_eq!(row[13], json!({"type": "Null"}));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_are_listed_read_changed_and_deleted_with_the_service_token() {
    let folder = tempfile::tempdir().unwrap();
    let server = Server::database(folder.path()).await;
    let (status, empty) = server.call("GET", "/files", Value::Null).await;
    assert_eq!((status, empty), (200, json!({"files": [], "next": null})));
    assert!(!files_folder(folder.path()).exists());
    let (status, usage) = server.call("GET", "/files/usage", Value::Null).await;
    assert_eq!(status, 200);
    assert_eq!(usage["files"], 0);

    let mut files = Vec::new();
    for (name, kind) in [
        ("a.txt", "text/plain"),
        ("b.png", "image/png"),
        ("c.html", "text/html"),
    ] {
        files.push(
            server
                .upload(name, name.as_bytes(), json!({"mediaType": kind}))
                .await,
        );
    }
    let ids: Vec<&str> = files
        .iter()
        .map(|file| file["id"].as_str().unwrap())
        .collect();
    let (_, listed) = server.call("GET", "/files", Value::Null).await;
    assert_eq!(listed, json!({"files": files, "next": null}));
    let (_, first) = server.call("GET", "/files?limit=2", Value::Null).await;
    assert_eq!(first, json!({"files": files[..2], "next": ids[1]}));
    let (_, rest) = server
        .call(
            "GET",
            &format!("/files?after={}&limit=2", ids[1]),
            Value::Null,
        )
        .await;
    assert_eq!(rest, json!({"files": files[2..], "next": null}));
    let (_, newest) = server
        .call("GET", "/files?order=desc&limit=1", Value::Null)
        .await;
    assert_eq!(newest, json!({"files": [files[2]], "next": ids[2]}));
    let (_, older) = server
        .call(
            "GET",
            &format!("/files?order=desc&after={}", ids[2]),
            Value::Null,
        )
        .await;
    assert_eq!(older, json!({"files": [files[1], files[0]], "next": null}));
    for query in [
        "limit=0",
        "limit=101",
        "order=up",
        "after=x",
        "limit=1&limit=2",
        "other=1",
    ] {
        let (status, refused) = server
            .call("GET", &format!("/files?{query}"), Value::Null)
            .await;
        assert_eq!(
            (status, refused),
            (400, json!({"error": "File list options"})),
            "{query}"
        );
    }

    let (status, read) = server
        .call("GET", &format!("/files/{}", ids[0]), Value::Null)
        .await;
    assert_eq!((status, &read), (200, &files[0]));
    let (status, missing) = server
        .call(
            "GET",
            "/files/0193a000-0000-7000-8000-000000000001",
            Value::Null,
        )
        .await;
    assert_eq!((status, missing), (404, json!({"error": "File not found"})));

    let change = json!({"name": "notes.txt", "description": "Meeting notes", "tags": ["a", "b", "a"], "metadata": {"pages": 2}});
    let (status, changed) = server
        .call("PATCH", &format!("/files/{}", ids[0]), change)
        .await;
    assert_eq!(status, 200, "{changed}");
    assert_eq!(changed["name"], "notes.txt");
    assert_eq!(changed["description"], "Meeting notes");
    assert_eq!(changed["tags"], json!(["a", "b"]));
    assert_eq!(changed["metadata"], json!({"pages": 2}));
    let (_, replaced) = server
        .call(
            "PATCH",
            &format!("/files/{}", ids[0]),
            json!({"metadata": {"author": "Ada"}}),
        )
        .await;
    assert_eq!(replaced["metadata"], json!({"author": "Ada"}));
    assert_eq!(replaced["name"], "notes.txt");
    assert_eq!(changed["size"], files[0]["size"]);
    assert_eq!(changed["createdAt"], files[0]["createdAt"]);
    assert!(changed["updatedAt"].as_u64() >= files[0]["updatedAt"].as_u64());
    for (change, status, error) in [
        (json!({}), 400, "File update"),
        (json!({"size": 1}), 400, "File update"),
        (json!({"name": ""}), 400, "File name"),
        (json!({"tags": "a"}), 400, "File tags"),
        (
            json!({"description": "x".repeat(2001)}),
            400,
            "File description",
        ),
        (
            json!({"metadata": {"text": "x".repeat(16 * 1024)}}),
            429,
            "File metadata is too large",
        ),
    ] {
        let reply = server
            .call("PATCH", &format!("/files/{}", ids[0]), change.clone())
            .await;
        assert_eq!(reply, (status, json!({"error": error})), "{change}");
    }
    let (status, _) = server
        .call(
            "PATCH",
            "/files/0193a000-0000-7000-8000-000000000001",
            json!({"name": "x"}),
        )
        .await;
    assert_eq!(status, 404);
    let unchanged = server
        .query("/read", "SELECT file FROM romedb_file_changes")
        .await;
    assert_eq!(unchanged["rows"], json!([]));

    let (status, inline) = server
        .call(
            "POST",
            &format!("/files/{}/download", ids[2]),
            json!({"disposition": "inline"}),
        )
        .await;
    assert_eq!(
        (status, inline),
        (
            403,
            json!({"error": "This media type downloads as an attachment only"})
        )
    );
    let (status, many) = server
        .call(
            "POST",
            "/files/downloads",
            json!({"files": [ids[0], ids[1], ids[2], "0193a000-0000-7000-8000-000000000001"], "disposition": "inline", "expiresInSeconds": 120}),
        )
        .await;
    assert_eq!(status, 200, "{many}");
    let downloads = many["downloads"].as_array().unwrap();
    assert_eq!(
        downloads
            .iter()
            .map(|d| d["fileId"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ids[..2]
    );
    let picture = server
        .send("GET", downloads[1]["url"].as_str().unwrap(), "", b"")
        .await;
    assert_eq!(picture.status, 200);
    assert_eq!(picture.body, b"b.png");
    assert_eq!(picture.header("content-type"), Some("image/png"));
    assert!(picture
        .header("content-disposition")
        .unwrap()
        .starts_with("inline; filename=\"b.png\""));
    for body in [
        json!({"files": []}),
        json!({"files": [ids[0], ids[0]]}),
        json!({"files": ["x"]}),
    ] {
        let (status, refused) = server.call("POST", "/files/downloads", body).await;
        assert_eq!(
            (status, refused),
            (
                400,
                json!({"error": "Ask for download URLs with 1 to 100 different file ids"})
            )
        );
    }

    let (_, usage) = server.call("GET", "/files/usage", Value::Null).await;
    assert_eq!(
        usage,
        json!({
            "organizationId": files[0]["databaseId"], "plan": "local", "bytes": 16, "reservedBytes": 0, "files": 3,
            "openUploads": 0, "peakBytes": 16, "allowanceBytes": 9007199254740991u64,
            "charge": {"blocks": 0, "blockBytes": 100000000000u64, "usdPerBlockMonth": 0, "billedBytes": 0, "monthlyUsd": 0},
        })
    );

    let (status, deleted) = server
        .call("DELETE", &format!("/files/{}", ids[1]), Value::Null)
        .await;
    assert_eq!(status, 200, "{deleted}");
    assert_eq!(deleted["fileId"], ids[1]);
    assert_eq!(deleted["databaseId"], files[0]["databaseId"]);
    assert_eq!(deleted["state"], "done");
    assert!(deleted["requestedAt"].is_u64() && deleted["finishedAt"].is_u64());
    assert_eq!(objects(&files_folder(folder.path())), [ids[0], ids[2]]);
    let again = server
        .call("DELETE", &format!("/files/{}", ids[1]), Value::Null)
        .await;
    assert_eq!(again, (200, deleted.clone()));
    let status = server
        .call("GET", &format!("/files/{}/deletion", ids[1]), Value::Null)
        .await;
    assert_eq!(status, (200, deleted));
    let (status, _) = server
        .call("GET", &format!("/files/{}/deletion", ids[0]), Value::Null)
        .await;
    assert_eq!(status, 404);
    let (status, _) = server
        .call("GET", &format!("/files/{}", ids[1]), Value::Null)
        .await;
    assert_eq!(status, 404);
    let gone = server
        .send("GET", downloads[1]["url"].as_str().unwrap(), "", b"")
        .await;
    assert_eq!(gone.status, 404);
    let (_, usage) = server.call("GET", "/files/usage", Value::Null).await;
    assert_eq!(
        (&usage["bytes"], &usage["files"], &usage["peakBytes"]),
        (&json!(11), &json!(2), &json!(16))
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_row_deleted_with_sql_loses_its_bytes_at_the_next_list() {
    let folder = tempfile::tempdir().unwrap();
    let server = Server::database(folder.path()).await;
    let kept = server.upload("kept.txt", b"kept", json!({})).await;
    let deleted = server.upload("deleted.txt", b"deleted", json!({})).await;
    let (kept_id, deleted_id) = (
        kept["id"].as_str().unwrap(),
        deleted["id"].as_str().unwrap(),
    );
    let store = files_folder(folder.path());
    assert_eq!(objects(&store), [kept_id, deleted_id]);

    server
        .query(
            "/query",
            &format!("UPDATE romedb_files:`{kept_id}` MERGE {{name: 'renamed.txt'}}"),
        )
        .await;
    server
        .query("/query", &format!("DELETE romedb_files:`{deleted_id}`"))
        .await;
    assert_eq!(objects(&store), [kept_id, deleted_id]);
    let changes = server
        .query("/read", "SELECT file FROM romedb_file_changes")
        .await;
    assert_eq!(changes["rows"].as_array().unwrap().len(), 2);

    let (status, listed) = server.call("GET", "/files", Value::Null).await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["files"].as_array().unwrap().len(), 1);
    assert_eq!(listed["files"][0]["id"], kept_id);
    assert_eq!(listed["files"][0]["name"], "renamed.txt");
    assert_eq!(objects(&store), [kept_id]);
    let changes = server
        .query("/read", "SELECT file FROM romedb_file_changes")
        .await;
    assert_eq!(changes["rows"], json!([]));

    let (_, details) = server.call("GET", "", Value::Null).await;
    let (_, refused) = server
        .call(
            "POST",
            "/query",
            json!({"requestId": "insert", "afterSequence": details["sequence"], "statements": [{"sql": "UPSERT romedb_files:`0193a000-0000-7000-8000-000000000001` {name: 'x'}"}]}),
        )
        .await;
    assert_eq!(refused["error"]["code"], "RDB_UNSUPPORTED", "{refused}");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_signatures_sizes_parts_and_checksums_are_refused() {
    let folder = tempfile::tempdir().unwrap();
    let server = Server::database(folder.path()).await;
    let store = files_folder(folder.path());
    let bytes = pattern(8 * MIB + 1000);
    let session = server.open_upload("big.bin", &bytes, json!({})).await;
    let (url, token) = (
        session["url"].as_str().unwrap(),
        session["token"].as_str().unwrap(),
    );
    let (_, urls) = server
        .with_token(
            token,
            "POST",
            &format!("{url}/parts"),
            json!({"parts": [1, 2]}),
        )
        .await;
    let (first, second) = (
        urls["parts"][0]["url"].as_str().unwrap(),
        urls["parts"][1]["url"].as_str().unwrap(),
    );
    let tail = &bytes[8 * MIB..];

    let (_, signature) = second.rsplit_once("signature=").unwrap();
    let flipped = if signature.starts_with('0') { "1" } else { "0" };
    let forged = format!(
        "{}signature={flipped}{}",
        second.rsplit_once("signature=").unwrap().0,
        &signature[1..]
    );
    let refused = server.send("PUT", &forged, "", tail).await;
    assert_eq!(
        (refused.status, refused.json()),
        (403, json!({"error": "Part URL signature is not valid"}))
    );
    let moved = second.replace("/parts/2?", "/parts/1?");
    assert_eq!(server.send("PUT", &moved, "", tail).await.status, 403);
    let unsigned = second.split('?').next().unwrap();
    assert_eq!(server.send("PUT", unsigned, "", tail).await.status, 403);
    let forged_with_cookie = server.send("PUT", &forged, "Cookie: a=b\r\n", tail).await;
    assert_eq!(forged_with_cookie.status, 403);

    for wrong in [&tail[..999], &bytes[8 * MIB - 1..]] {
        let refused = server.send("PUT", second, "", wrong).await;
        assert_eq!(
            (refused.status, refused.json()),
            (400, json!({"error": "Part 2 must be exactly 1000 bytes"}))
        );
    }
    let chunked = format!(
        "PUT {} HTTP/1.1\r\nHost: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        second.strip_prefix(&format!("http://{}", server.address)).unwrap(),
        server.address
    );
    let address = server.address;
    let reply = tokio::task::spawn_blocking(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(chunked.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    })
    .await
    .unwrap();
    assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
    assert_eq!(
        std::fs::read_dir(store.join("uploads").join(session["id"].as_str().unwrap()))
            .unwrap()
            .count(),
        0
    );

    assert_eq!(server.send("PUT", second, "", tail).await.status, 200);
    let (status, incomplete) = server
        .with_token(token, "POST", &format!("{url}/complete"), json!({}))
        .await;
    assert_eq!(
        (status, incomplete),
        (
            409,
            json!({"error": "Upload is incomplete; 1 of 2 parts are missing"})
        )
    );
    let (_, status) = server.with_token(token, "GET", url, Value::Null).await;
    assert_eq!(status["state"], "uploading");
    assert_eq!(status["uploadedParts"], json!([2]));

    assert_eq!(
        server
            .send("PUT", first, "", &bytes[..8 * MIB])
            .await
            .status,
        200
    );
    let (status, refused) = server
        .with_token(
            token,
            "POST",
            &format!("{url}/complete"),
            json!({"crc64nvme": "nope"}),
        )
        .await;
    assert_eq!(
        (status, refused),
        (400, json!({"error": "Upload completion"}))
    );
    let (status, rejected) = server
        .with_token(
            token,
            "POST",
            &format!("{url}/complete"),
            json!({"crc64nvme": crc64_nvme(b"other bytes")}),
        )
        .await;
    assert_eq!(
        (status, rejected),
        (
            409,
            json!({"error": "File checksum differs; the upload was rejected"})
        )
    );
    let (_, status) = server.with_token(token, "GET", url, Value::Null).await;
    assert_eq!(status["state"], "rejected");
    assert_eq!(
        status["reason"],
        "The stored bytes differ from the checksum the client computed"
    );
    assert_eq!(objects(&store), Vec::<String>::new());
    assert_eq!(
        server.call("GET", "/files", Value::Null).await.1["files"],
        json!([])
    );
    let after = server.send("PUT", second, "", tail).await;
    assert_eq!(
        (after.status, after.json()),
        (403, json!({"error": "Upload is not open"}))
    );
    let (status, closed) = server
        .with_token(
            token,
            "POST",
            &format!("{url}/parts"),
            json!({"parts": [1]}),
        )
        .await;
    assert_eq!(
        (status, closed),
        (409, json!({"error": "Upload is not open"}))
    );

    let file = server.upload("small.txt", b"small", json!({})).await;
    let (_, download) = server
        .call(
            "POST",
            &format!("/files/{}/download", file["id"].as_str().unwrap()),
            json!({"expiresInSeconds": 1}),
        )
        .await;
    let link = download["url"].as_str().unwrap();
    assert_eq!(server.send("GET", link, "", b"").await.body, b"small");
    let other_name = link.replace("disposition=attachment", "disposition=inline");
    let refused = server.send("GET", &other_name, "", b"").await;
    assert_eq!(
        (refused.status, refused.json()),
        (403, json!({"error": "Download URL signature is not valid"}))
    );
    let unsigned = link.split('?').next().unwrap();
    assert_eq!(server.send("GET", unsigned, "", b"").await.status, 403);
    let from_a_page = server
        .send(
            "GET",
            link,
            "Cookie: session=of-the-page\r\nSec-Fetch-Site: same-site\r\nSec-Fetch-Dest: image\r\n",
            b"",
        )
        .await;
    assert_eq!(
        (from_a_page.status, from_a_page.body.as_slice()),
        (200, &b"small"[..])
    );
    let unsigned_with_cookie = server.send("GET", unsigned, "Cookie: a=b\r\n", b"").await;
    assert_eq!(unsigned_with_cookie.status, 403);
    let with_token_and_cookie =
        format!("Authorization: Bearer {}\r\nCookie: a=b\r\n", server.token);
    let listed = server
        .send("GET", "/files", &with_token_and_cookie, b"")
        .await;
    assert_eq!(listed.status, 403);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let expired = server.send("GET", link, "", b"").await;
    assert_eq!(
        (expired.status, expired.json()),
        (403, json!({"error": "Download URL expired"}))
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_browser_reaches_only_the_upload_session_and_signed_routes() {
    let folder = tempfile::tempdir().unwrap();
    let server = Server::database(folder.path()).await;
    let service = format!(
        "Authorization: Bearer {}\r\nContent-Type: application/json\r\n",
        server.token
    );
    let session = server.open_upload("a.txt", b"hello", json!({})).await;
    let other = server.open_upload("b.txt", b"hello", json!({})).await;
    let (url, token) = (
        session["url"].as_str().unwrap(),
        session["token"].as_str().unwrap(),
    );

    let preflight = format!("{PAGE}Access-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: authorization,content-type\r\n");
    for path in [
        url.to_owned(),
        format!("{url}/parts"),
        format!("{url}/complete"),
        format!("{url}/abort"),
        format!("{url}/parts/1?expires=1&signature=00"),
    ] {
        let allowed = server.send("OPTIONS", &path, &preflight, b"").await;
        assert_eq!(allowed.status, 204, "{path}");
        assert!(allowed.body.is_empty());
        assert_eq!(allowed.header("access-control-allow-origin"), Some("*"));
        assert_eq!(
            allowed.header("access-control-allow-methods"),
            Some("GET, POST, PUT, OPTIONS")
        );
        assert_eq!(
            allowed.header("access-control-allow-headers"),
            Some("authorization, content-type")
        );
    }
    let with_cookie = format!("{preflight}Cookie: a=b\r\n");
    assert_eq!(
        server.send("OPTIONS", url, &with_cookie, b"").await.status,
        403
    );
    let other_host = format!(
        "OPTIONS {} HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n",
        url.strip_prefix(&format!("http://{}", server.address))
            .unwrap()
    );
    let address = server.address;
    let reply = tokio::task::spawn_blocking(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(other_host.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    })
    .await
    .unwrap();
    assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");

    for (method, path, body) in [
        ("GET", "/files", ""),
        ("POST", "/files/uploads", "{}"),
        ("GET", "/files/usage", ""),
        ("POST", "/files/downloads", "{}"),
        ("OPTIONS", "/files", ""),
    ] {
        let refused = server
            .send(method, path, &format!("{service}{PAGE}"), body.as_bytes())
            .await;
        assert_eq!(refused.status, 403, "{method} {path}");
        assert!(refused.json()["error"].is_string());
        assert_eq!(refused.header("access-control-allow-origin"), None);
    }
    let query = json!({"requestId": "page", "afterSequence": 0, "statements": [{"sql": "CREATE TABLE docs"}]}).to_string();
    let refused = server
        .send(
            "POST",
            "/query",
            &format!("{service}{PAGE}"),
            query.as_bytes(),
        )
        .await;
    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["error"]["code"], "RDB_ORIGIN");
    assert_eq!(refused.header("access-control-allow-origin"), None);
    let preflight_query = server.send("OPTIONS", "/query", &preflight, b"").await;
    assert_eq!(preflight_query.status, 403);

    let no_token = server.send("GET", url, PAGE, b"").await;
    assert_eq!(no_token.status, 401);
    assert_eq!(no_token.header("access-control-allow-origin"), Some("*"));
    let (status, wrong) = server
        .with_token(other["token"].as_str().unwrap(), "GET", url, Value::Null)
        .await;
    assert_eq!(
        (status, wrong),
        (
            401,
            json!({"error": "Upload session token is not valid for this upload"})
        )
    );
    for (method, path) in [
        ("GET", "/files"),
        ("POST", "/files/uploads"),
        ("GET", "/files/usage"),
    ] {
        let (status, refused) = server.with_token(token, method, path, json!({})).await;
        assert_eq!(
            (status, refused),
            (401, json!({"error": "Database token is required"})),
            "{method} {path}"
        );
    }
    let (status, _) = server.with_token(token, "DELETE", url, Value::Null).await;
    assert_eq!(status, 405);
    let (status, _) = server.call("GET", "/files/uploads?x=1", Value::Null).await;
    assert_eq!(status, 405);
    let (status, _) = server.call("POST", "/files/uploads?x=1", json!({})).await;
    assert_eq!(status, 400);
    let (status, unknown) = server
        .call("GET", "/files/unknown/route", Value::Null)
        .await;
    assert_eq!(
        (status, unknown),
        (404, json!({"error": "File route does not exist"}))
    );

    let (status, aborted) = server
        .with_token(token, "POST", &format!("{url}/abort"), Value::Null)
        .await;
    assert_eq!(status, 200, "{aborted}");
    assert_eq!(aborted["state"], "aborted");
    assert_eq!(aborted["reason"], "Aborted by the caller");
    assert!(aborted.get("token").is_none());
    let (status, aborted) = server
        .call(
            "POST",
            &format!("{}/abort", other["url"].as_str().unwrap()),
            Value::Null,
        )
        .await;
    assert_eq!((status, &aborted["state"]), (200, &json!("aborted")));

    let file = server.upload("c.txt", b"hello", json!({})).await;
    let file_id = file["id"].as_str().unwrap();
    for (method, path) in [
        ("POST", format!("/files/{file_id}/jobs")),
        ("GET", format!("/files/jobs/{file_id}")),
        ("DELETE", format!("/files/jobs/{file_id}")),
        ("GET", format!("/files/{file_id}/derivatives")),
    ] {
        let reply = server
            .call(
                method,
                &path,
                json!({"kind": "image", "recipe": {"width": 10}}),
            )
            .await;
        assert_eq!(
            reply,
            (503, json!({"error": "File processing is unavailable"})),
            "{method} {path}"
        );
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_keeps_the_files_of_each_database_apart() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("data");
    let mut server = Server::directory(&root).await;
    let organization = server.base.clone();
    let mut databases = Vec::new();
    for name in ["first", "second"] {
        let (status, created) = server
            .call("POST", "/databases", json!({"name": name}))
            .await;
        assert_eq!(status, 201, "{created}");
        databases.push(created["id"].as_str().unwrap().to_owned());
    }
    let (status, usage) = server.call("GET", "/files/usage", Value::Null).await;
    assert_eq!(status, 200, "{usage}");
    assert_eq!((&usage["files"], &usage["bytes"]), (&json!(0), &json!(0)));

    let mut files = Vec::new();
    for (database, bytes) in databases.iter().zip([&b"one"[..], b"second file"]) {
        server.base = format!("{organization}/databases/{database}");
        let session = server.open_upload("a.txt", bytes, json!({})).await;
        assert_eq!(
            session["url"],
            format!(
                "http://{}{}/files/uploads/{}",
                server.address,
                server.base,
                session["id"].as_str().unwrap()
            )
        );
        assert_eq!(session["databaseId"], database.as_str());
        assert_eq!(
            session["organizationId"],
            organization.rsplit('/').next().unwrap()
        );
        let file = server.upload("a.txt", bytes, json!({})).await;
        let (_, listed) = server.call("GET", "/files", Value::Null).await;
        assert_eq!(listed, json!({"files": [file], "next": null}));
        let (_, download) = server
            .call(
                "POST",
                &format!("/files/{}/download", file["id"].as_str().unwrap()),
                Value::Null,
            )
            .await;
        let link = download["url"].as_str().unwrap();
        assert!(link.starts_with(&format!("http://{}{}/files/", server.address, server.base)));
        assert_eq!(server.send("GET", link, "", b"").await.body, bytes);
        assert_eq!(
            objects(&root.join(format!("{database}.files"))),
            [file["id"].as_str().unwrap()]
        );
        let (status, _) = server.call("GET", "/files/usage", Value::Null).await;
        assert_eq!(status, 404);
        files.push((file, link.to_owned()));
    }
    let other = format!("/files/{}", files[0].0["id"].as_str().unwrap());
    assert_eq!(server.call("GET", &other, Value::Null).await.0, 404);
    let crossed = files[0].1.replace(&databases[0], &databases[1]);
    assert_eq!(server.send("GET", &crossed, "", b"").await.status, 403);

    server.base = organization.clone();
    let (status, usage) = server.call("GET", "/files/usage", Value::Null).await;
    assert_eq!(status, 200, "{usage}");
    assert_eq!(
        usage["organizationId"],
        organization.rsplit('/').next().unwrap()
    );
    assert_eq!(usage["plan"], "local");
    assert_eq!(usage["files"], 2);
    assert_eq!(usage["bytes"], 14);
    assert_eq!(usage["openUploads"], 2);
    assert_eq!(usage["reservedBytes"], 14);
    let page = server
        .send(
            "GET",
            "/files/usage",
            &format!("Authorization: Bearer {}\r\n{PAGE}", server.token),
            b"",
        )
        .await;
    assert_eq!(page.status, 403);

    let deleted = server
        .send(
            "DELETE",
            &format!("/databases/{}", databases[0]),
            &format!("Authorization: Bearer {}\r\n", server.token),
            b"",
        )
        .await;
    assert_eq!(deleted.status, 204);
    assert!(!root.join(format!("{}.files", databases[0])).exists());
    assert!(root.join(format!("{}.files", databases[1])).exists());
    let (status, gone) = server
        .call(
            "GET",
            &format!("/databases/{}/files", databases[0]),
            Value::Null,
        )
        .await;
    assert_eq!(
        (status, gone),
        (404, json!({"error": "Database not found"}))
    );
    assert_eq!(server.send("GET", &files[0].1, "", b"").await.status, 404);
    let (_, usage) = server.call("GET", "/files/usage", Value::Null).await;
    assert_eq!((&usage["files"], &usage["bytes"]), (&json!(1), &json!(11)));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_without_a_valid_signature_or_token_opens_no_database() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("data");
    let mut server = Server::directory(&root).await;
    let organization = server.base.clone();
    let (_, created) = server
        .call("POST", "/databases", json!({"name": "app"}))
        .await;
    let database = created["id"].as_str().unwrap().to_owned();
    server.base = format!("{organization}/databases/{database}");
    let file = server.upload("a.txt", b"hello", json!({})).await;
    let file_id = file["id"].as_str().unwrap().to_owned();
    let session = server.open_upload("b.txt", b"hello", json!({})).await;
    let upload = format!("/files/uploads/{}", session["id"].as_str().unwrap());
    server.stop().await;

    let mut server = Server::directory(&root).await;
    server.base = format!("{organization}/databases/{database}");
    std::fs::rename(
        root.join(format!("{database}.romedb")),
        root.join("moved-away"),
    )
    .unwrap();
    let unsigned = server
        .send(
            "GET",
            &format!(
                "/files/{file_id}/content?expires=99999999999999&disposition=inline&signature=00"
            ),
            "",
            b"",
        )
        .await;
    assert_eq!(
        (unsigned.status, unsigned.json()),
        (403, json!({"error": "Download URL signature is not valid"}))
    );
    let unsigned = server
        .send(
            "PUT",
            &format!("{upload}/parts/1?expires=99999999999999&signature=00"),
            "",
            b"hello",
        )
        .await;
    assert_eq!(unsigned.status, 403);
    let (status, refused) = server
        .with_token(
            session["token"].as_str().unwrap(),
            "GET",
            &upload,
            Value::Null,
        )
        .await;
    assert_eq!(
        (status, refused),
        (
            401,
            json!({"error": "Upload session token is not valid for this upload"})
        )
    );
    let (status, _) = server.call("GET", "/files", Value::Null).await;
    assert_eq!(status, 404);
    server.stop().await;
}
