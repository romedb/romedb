use romedb::{Database, Parameters, Value};
use std::fs;

fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}

#[test]
fn manual_wal_retains_incremental_commits_and_restores_without_local_files() {
    let source = tempfile::tempdir().unwrap();
    let path = source.path().join("source.db");
    let path_str = path.to_str().unwrap();
    let wal_path = format!("{path_str}-wal");
    let db = Database::open_with_manual_wal(path_str).unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE items");
    c.create_index("items", "items_email", vec!["email".into()], true)
        .unwrap();
    q(&c, "CREATE TABLE events (n INTEGER)");
    q(&c, "PRAGMA wal_checkpoint(TRUNCATE)");
    let checkpoint = fs::read(&path).unwrap();
    let mut archived_wal = Vec::new();
    let mut previous = 0usize;
    let mut header = None;

    // Exceed the pinned engine's 1000-frame automatic checkpoint threshold.
    // Each captured segment contains only newly committed physical WAL bytes.
    for n in 0..1100 {
        q(&c, &format!("INSERT INTO events VALUES ({n})"));
        let position = c.wal_replication_position().unwrap();
        let end = usize::try_from(position.byte_len().unwrap()).unwrap();
        assert!(end > previous);
        let bytes = fs::read(&wal_path).unwrap();
        if let Some(header) = &header {
            assert_eq!(&bytes[..32], header);
        } else {
            header = Some(bytes[..32].to_vec());
        }
        archived_wal.extend_from_slice(&bytes[previous..end]);
        previous = end;
    }
    q(&c, "INSERT INTO items {id: items:first, email: 'a@example.test', nested: {enabled: true}} RETURNING *");
    let committed = c.wal_replication_position().unwrap();
    q(&c, "BEGIN");
    q(&c, "UPDATE items:first {email: 'uncommitted@example.test'}");
    assert!(c.wal_replication_position().is_err());
    q(&c, "ROLLBACK");
    assert_eq!(c.wal_replication_position().unwrap(), committed);
    assert!(c
        .execute(
            "INSERT INTO items {id: items:duplicate, email: 'a@example.test'}",
            &Parameters::new()
        )
        .is_err());
    // Failed document statements may commit internal catalog bookkeeping.
    // Replicate the actual committed prefix, not an assumed unchanged offset.
    let after_failure = c.wal_replication_position().unwrap();
    assert!(after_failure.committed_frames >= committed.committed_frames);
    assert_eq!(
        q(&c, "SELECT count(*) FROM items").rows,
        vec![vec![Value::Integer(1)]]
    );
    let committed = c.wal_replication_position().unwrap();
    let end = usize::try_from(committed.byte_len().unwrap()).unwrap();
    let bytes = fs::read(&wal_path).unwrap();
    archived_wal.extend_from_slice(&bytes[previous..end]);
    assert_eq!(
        fs::read(&path).unwrap(),
        checkpoint,
        "automatic checkpoint changed the base file"
    );

    drop(c);
    drop(db);
    assert_eq!(
        fs::read(&path).unwrap(),
        checkpoint,
        "close checkpointed manual WAL"
    );
    assert_eq!(&fs::read(&wal_path).unwrap()[..end], &archived_wal);

    // Reopen must not reset or checkpoint the log during catalog initialization.
    {
        let db = Database::open_with_manual_wal(path_str).unwrap();
        let c = db.connect().unwrap();
        assert_eq!(c.wal_replication_position().unwrap(), committed);
        assert_eq!(fs::read(&path).unwrap(), checkpoint);
    }

    // Remove all source data before recovery; only archived checkpoint + segments survive.
    source.close().unwrap();
    let target = tempfile::tempdir().unwrap();
    let restored = target.path().join("restored.db");
    fs::write(&restored, &checkpoint).unwrap();
    fs::write(format!("{}-wal", restored.display()), &archived_wal).unwrap();
    let db = Database::open_with_manual_wal(restored.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(
        q(&c, "SELECT count(*) FROM events").rows,
        vec![vec![Value::Integer(1100)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT i.nested.enabled FROM items i WHERE i.id = items:first"
        )
        .rows,
        vec![vec![Value::Boolean(true)]]
    );
    assert_eq!(
        c.lookup_index(
            "items",
            "items_email",
            &Value::String("a@example.test".into())
        )
        .unwrap()
        .len(),
        1
    );
    assert!(c
        .lookup_index(
            "items",
            "items_email",
            &Value::String("uncommitted@example.test".into())
        )
        .unwrap()
        .is_empty());
    q(&c, "INSERT INTO events VALUES (1100)");
    assert!(c.wal_replication_position().unwrap().committed_frames > committed.committed_frames);
}

#[test]
fn ordinary_connections_reject_replication_position() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("ordinary.db").to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert!(c
        .wal_replication_position()
        .unwrap_err()
        .to_string()
        .contains("manual WAL"));
}
