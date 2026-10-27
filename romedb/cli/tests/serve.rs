#![cfg(unix)]
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(path: &std::path::Path, port: u16, create: bool) -> (Server, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_romedb"));
    command
        .arg("serve")
        .arg(path)
        .arg("--port")
        .arg(port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if create {
        command.arg("--create");
    }
    let mut server = Server(command.spawn().unwrap());
    let mut stdout = BufReader::new(server.0.stdout.take().unwrap());
    let mut url = String::new();
    stdout.read_line(&mut url).unwrap();
    assert!(url.starts_with("Database URL: "), "service failed to start");
    let mut token_line = String::new();
    stdout.read_line(&mut token_line).unwrap();
    let token = std::fs::read_to_string(token_line.trim().strip_prefix("Token file: ").unwrap())
        .unwrap()
        .trim()
        .to_owned();
    assert!(!url.contains(&token));
    assert!(!token_line.contains(&token));
    (server, token)
}

fn stop(server: &mut Server) {
    assert!(Command::new("kill")
        .arg("-TERM")
        .arg(server.0.id().to_string())
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(Instant::now() < deadline, "service did not shut down");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn cli_service_replays_after_restart_and_refuses_a_second_file_owner() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("app.romedb");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let (mut server, token) = start(&path, port, true);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let url = format!("http://127.0.0.1:{port}/v1/database");
    let body = json!({"requestId":"setup","afterSequence":0,"statements":[{"sql":"CREATE TABLE items (id INTEGER PRIMARY KEY)"},{"sql":"INSERT INTO items VALUES(1)"}]}).to_string();
    let reply = client
        .post(format!("{url}/query"))
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(body.clone())
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(
        serde_json::from_str::<Value>(&reply).unwrap()["error"].is_null(),
        "{reply}"
    );
    let mut competitor = Command::new(env!("CARGO_BIN_EXE_romedb-cli"))
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    competitor
        .stdin
        .take()
        .unwrap()
        .write_all(b"SELECT 1;\n")
        .unwrap();
    assert!(!competitor.wait_with_output().unwrap().status.success());
    stop(&mut server);
    drop(server);
    let (mut server, restarted_token) = start(&path, port, false);
    assert_eq!(token, restarted_token);
    let replay = client
        .post(format!("{url}/query"))
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert_eq!(reply, replay);
    stop(&mut server);
}

#[test]
fn public_alias_preserves_embedded_commands_and_creation_is_explicit() {
    for binary in [
        env!("CARGO_BIN_EXE_romedb"),
        env!("CARGO_BIN_EXE_romedb-cli"),
    ] {
        let help = Command::new(binary)
            .args(["serve", "--help"])
            .output()
            .unwrap();
        assert!(help.status.success());
        let mut child = Command::new(binary)
            .arg(":memory:")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"SELECT 1;\n")
            .unwrap();
        assert!(child.wait_with_output().unwrap().status.success());
    }
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.romedb");
    assert!(!Command::new(env!("CARGO_BIN_EXE_romedb"))
        .arg("serve")
        .arg(&missing)
        .output()
        .unwrap()
        .status
        .success());
    assert!(!missing.exists());
}

#[test]
fn acknowledged_http_writes_and_receipts_survive_sigkill() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("crash.romedb");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let (mut server, token) = start(&path, port, true);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let url = format!("http://127.0.0.1:{port}/v1/database");
    let setup = json!({"requestId":"setup","afterSequence":0,"statements":[
        {"sql":"CREATE TABLE docs;"},
        {"sql":"CREATE UNIQUE INDEX docs_n ON docs(n)"},
        {"sql":"CREATE SEARCH INDEX docs_text ON docs(title) USING FULLTEXT"},
        {"sql":"CREATE SEARCH INDEX docs_vector ON docs(v) USING VECTOR WITH (dimensions=2,metric='l2')"},
        {"sql":"CREATE SEARCH INDEX docs_spatial ON docs(location) USING SPATIAL"},
        {"sql":"INSERT INTO docs {id:docs:a,n:1,title:'beforeterm',v:vector32('[1,0]'),location:geo::point(0,0)}"}
    ]}).to_string();
    let send = |body: &str| {
        client
            .post(format!("{url}/query"))
            .bearer_auth(&token)
            .header("content-type", "application/json")
            .body(body.to_owned())
            .send()
            .unwrap()
            .text()
            .unwrap()
    };
    let reply: Value = serde_json::from_str(&send(&setup)).unwrap();
    assert!(reply["error"].is_null(), "{reply}");
    let body = json!({"requestId":"acknowledged","afterSequence":1,"statements":[
        {"sql":"UPDATE docs SET n=2,title='afterterm',v=vector32('[0,1]'),location=geo::point(10,0) WHERE id=docs:a"}
    ]}).to_string();
    let acknowledged = send(&body);
    let reply: Value = serde_json::from_str(&acknowledged).unwrap();
    assert!(reply["error"].is_null(), "{reply}");
    assert_eq!(reply["sequence"], 2);
    server.0.kill().unwrap();
    let killed = server.0.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(killed.signal(), Some(9));
    drop(server);
    {
        let database = romedb::Database::open_for_service(path.to_str().unwrap()).unwrap();
        let connection = database.connect().unwrap();
        assert_eq!(connection.service_catalog().unwrap().sequence, 2);
        assert_eq!(
            connection
                .service_receipt("acknowledged")
                .unwrap()
                .unwrap()
                .response,
            acknowledged
        );
        for sql in [
            "SELECT id FROM docs WHERE n=2",
            "SELECT id FROM search::text('docs_text','afterterm',10)",
            "SELECT id FROM search::vector('docs_vector',vector32('[0,1]'),1)",
            "SELECT id FROM search::near('docs_spatial',geo::point(10,0),1)",
        ] {
            assert_eq!(
                connection
                    .execute(sql, &romedb::Parameters::new())
                    .unwrap()
                    .rows
                    .len(),
                1,
                "{sql}"
            );
        }
        let integrity = connection
            .check_collection_integrity("docs", Default::default())
            .unwrap();
        assert_eq!((integrity.documents, integrity.index_entries), (1, 4));
    }
    let (mut server, restarted_token) = start(&path, port, false);
    assert_eq!(token, restarted_token);
    assert_eq!(send(&body), acknowledged);
    stop(&mut server);
}
