//! Process-kill recovery probes. These simulate process failure, not power loss.
use romedb::{Database, Document, Key, Parameters, Record, Value};
use std::io::{BufRead, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const WIDTH: i64 = 12;
fn query(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}
fn document(batch: i64, slot: i64) -> Document {
    let name = format!("b{batch}s{slot}");
    Document::from([
        (
            "id".into(),
            Value::Record(Record {
                table: "items".into(),
                key: Key::String(name.clone()),
            }),
        ),
        ("name".into(), Value::String(name)),
        ("batch".into(), Value::Integer(batch)),
        ("payload".into(), Value::Binary(vec![slot as u8; 2048])),
    ])
}
fn archived(batch: i64, slot: i64) -> Document {
    let mut value = document(batch, slot);
    value.insert(
        "name".into(),
        Value::String(format!("archived_b{batch}s{slot}")),
    );
    value.insert(
        "payload".into(),
        Value::Binary(vec![255 - slot as u8; 2048]),
    );
    value
}
fn marker(phase: &str, batch: i64) {
    println!("ROMEDB_PHASE {phase} {batch}");
    std::io::stdout().flush().unwrap();
}
#[test]
fn crash_writer_child() {
    let Ok(path) = std::env::var("ROMEDB_KILL_STRESS_PATH") else {
        return;
    };
    let db = Database::open(&path).unwrap();
    let c = db.connect().unwrap();
    query(&c, "CREATE TABLE items");
    query(&c, "CREATE UNIQUE INDEX item_name ON items(name)");
    query(
        &c,
        "CREATE TABLE audit(batch INTEGER,slot INTEGER,PRIMARY KEY(batch,slot))",
    );
    query(
        &c,
        "CREATE TABLE live(name TEXT PRIMARY KEY,batch INTEGER,slot INTEGER)",
    );
    query(&c, "CREATE TABLE state(version INTEGER)");
    query(&c, "INSERT INTO state VALUES (0)");
    for batch in 1..=1000 {
        query(&c, "BEGIN");
        if batch > 1 {
            let previous = batch - 1;
            for slot in 0..WIDTH {
                let before = document(previous, slot);
                let Value::Record(id) = &before["id"] else {
                    unreachable!();
                };
                if slot % 2 == 0 {
                    let mut patch = archived(previous, slot);
                    patch.remove("id");
                    if slot % 4 == 0 {
                        let params = Parameters::from([
                            ("$id".into(), Value::Record(id.clone())),
                            ("$name".into(), patch["name"].clone()),
                            ("$payload".into(), patch["payload"].clone()),
                        ]);
                        let sql = if slot % 8 == 0 {
                            marker("joined", batch);
                            "WITH target AS (SELECT $id AS id,$name AS name,$payload AS payload), copies AS (SELECT * FROM target UNION ALL SELECT * FROM target) UPDATE items AS target SET (name,payload)=(s.name,s.payload) FROM copies s WHERE target.id=s.id"
                        } else {
                            "UPDATE items SET (name,payload)=(VALUES($name,$payload)) WHERE id=$id"
                        };
                        assert_eq!(c.execute(sql, &params,).unwrap().affected, 1);
                    } else {
                        assert!(c.patch(id, patch).unwrap().is_some());
                    }
                    query(&c,&format!("UPDATE live SET name='archived_b{previous}s{slot}' WHERE batch={previous} AND slot={slot}"));
                } else {
                    assert!(c.delete(id).unwrap().is_some());
                    query(
                        &c,
                        &format!("DELETE FROM live WHERE batch={previous} AND slot={slot}"),
                    );
                }
            }
        }
        marker("rewrite", batch);
        for slot in 0..WIDTH {
            c.insert("items", document(batch, slot)).unwrap();
            query(
                &c,
                &format!("INSERT INTO live VALUES ('b{batch}s{slot}',{batch},{slot})"),
            );
            query(&c, &format!("INSERT INTO audit VALUES ({batch},{slot})"));
        }
        query(&c, &format!("UPDATE state SET version={batch}"));
        marker("commit", batch);
        query(&c, "COMMIT");
        marker("ack", batch);
        marker("checkpoint", batch);
        query(&c, "PRAGMA wal_checkpoint(TRUNCATE)");
    }
    panic!("parent did not stop crash writer");
}
struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn killed_commit_and_checkpoint_loops_recover_atomic_batches() {
    for phase in ["joined", "rewrite", "commit", "checkpoint"] {
        for delay_ms in [0, 1, 5] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("kill.db");
            let mut child = KillOnDrop(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "crash_writer_child", "--nocapture"])
                    .env("ROMEDB_KILL_STRESS_PATH", &path)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap(),
            );
            let output = child.0.stdout.take().unwrap();
            let (send, receive) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || {
                for line in std::io::BufReader::new(output).lines() {
                    if send.send(line.unwrap()).is_err() {
                        break;
                    }
                }
            });
            let mut acknowledged = 0;
            loop {
                let line = receive
                    .recv_timeout(Duration::from_secs(30))
                    .expect("writer phase timeout");
                let words = line.split_whitespace().collect::<Vec<_>>();
                if words.len() != 3 || words[0] != "ROMEDB_PHASE" {
                    continue;
                }
                let batch = words[2].parse::<i64>().unwrap();
                if words[1] == "ack" {
                    acknowledged = acknowledged.max(batch);
                }
                if words[1] == phase && batch >= 3 {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(delay_ms));
            child.0.kill().unwrap();
            assert!(!child.0.wait().unwrap().success());
            reader.join().unwrap();
            for line in receive.try_iter() {
                if let Some(batch) = line.strip_prefix("ROMEDB_PHASE ack ") {
                    acknowledged = acknowledged.max(batch.parse().unwrap());
                }
            }
            // A commit can become durable before its acknowledgement reaches
            // stdout. Exactly one unacknowledged transaction may survive.
            for reopen in 0..2 {
                let db = Database::open(path.to_str().unwrap()).unwrap();
                let c = db.connect().unwrap();
                assert_eq!(
                    query(&c, "PRAGMA integrity_check").rows,
                    vec![vec![Value::String("ok".into())]]
                );
                let version = query(&c, "SELECT version FROM state").rows[0][0].clone();
                let Value::Integer(version) = version else {
                    panic!("state version type");
                };
                assert!(
                    (acknowledged..=acknowledged + 1).contains(&version),
                    "{phase}/{delay_ms}/{reopen}: acknowledged {acknowledged}, recovered {version}"
                );
                assert_eq!(
                    query(&c, "SELECT count(*) FROM audit").rows,
                    vec![vec![Value::Integer(version * WIDTH)]]
                );
                assert_eq!(
                    query(&c, "SELECT count(*) FROM items").rows,
                    vec![vec![Value::Integer((version - 1) * (WIDTH / 2) + WIDTH)]]
                );
                assert_eq!(
                    query(&c, "SELECT count(*) FROM live").rows,
                    vec![vec![Value::Integer((version - 1) * (WIDTH / 2) + WIDTH)]]
                );
                for batch in 1..=version {
                    for slot in 0..WIDTH {
                        let original = document(batch, slot);
                        let Value::Record(id) = &original["id"] else {
                            unreachable!();
                        };
                        let expected = if batch == version {
                            Some(original.clone())
                        } else if slot % 2 == 0 {
                            Some(archived(batch, slot))
                        } else {
                            None
                        };
                        assert_eq!(c.get(id).unwrap(), expected);
                        if let Some(expected) = expected {
                            assert_eq!(
                                c.lookup_index("items", "item_name", &expected["name"])
                                    .unwrap(),
                                vec![expected.clone()]
                            );
                            assert_eq!(
                                query(
                                    &c,
                                    &format!(
                                        "SELECT name FROM live WHERE batch={batch} AND slot={slot}"
                                    )
                                )
                                .rows,
                                vec![vec![expected["name"].clone()]]
                            );
                        } else {
                            assert!(query(
                                &c,
                                &format!(
                                    "SELECT name FROM live WHERE batch={batch} AND slot={slot}"
                                )
                            )
                            .rows
                            .is_empty());
                        }
                        if batch < version {
                            assert!(c
                                .lookup_index("items", "item_name", &original["name"])
                                .unwrap()
                                .is_empty());
                        }
                        if batch == version || slot % 2 != 0 {
                            assert!(c
                                .lookup_index("items", "item_name", &archived(batch, slot)["name"])
                                .unwrap()
                                .is_empty());
                        }
                        assert_eq!(query(&c,&format!("SELECT count(*) FROM audit WHERE batch={batch} AND slot={slot}")).rows,vec![vec![Value::Integer(1)]]);
                    }
                }
                for slot in 0..WIDTH {
                    let absent = document(version + 1, slot);
                    let Value::Record(id) = &absent["id"] else {
                        unreachable!();
                    };
                    assert!(c.get(id).unwrap().is_none());
                    assert!(c
                        .lookup_index("items", "item_name", &absent["name"])
                        .unwrap()
                        .is_empty());
                }
            }
        }
    }
}
