use crate::{text, Connection, Database, EngineDatabase, EngineValue, Error, Result};
use std::sync::Arc;

const STATE_DDL: &str = "CREATE TABLE IF NOT EXISTS __romedb_service_state (slot INTEGER PRIMARY KEY, version INTEGER NOT NULL, database_id TEXT NOT NULL, sequence INTEGER NOT NULL)";
const RECEIPT_DDL: &str = "CREATE TABLE IF NOT EXISTS __romedb_service_receipts (request_id TEXT PRIMARY KEY, request_hash TEXT NOT NULL, sequence INTEGER NOT NULL UNIQUE, response TEXT NOT NULL)";
const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;

#[derive(Debug)]
pub struct ServiceCatalog {
    pub database_id: String,
    pub sequence: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        let database = Database::open(":memory:").unwrap();
        let connection = database.connect().unwrap();
        connection
            .execute("PRAGMA journal_mode=mvcc", &crate::Parameters::new())
            .unwrap();
        connection
            .initialize_service_catalog("test-database")
            .unwrap();
        connection
    }

    fn publish(connection: &Connection) {
        connection
            .execute("BEGIN CONCURRENT", &crate::Parameters::new())
            .unwrap();
        connection
            .publish_service_receipt(
                0,
                "request",
                &"a".repeat(64),
                r#"{"requestId":"request","sequence":1,"results":[]}"#,
            )
            .unwrap();
        connection
            .execute("COMMIT", &crate::Parameters::new())
            .unwrap();
    }

    #[test]
    fn incomplete_receipts_prevent_service_initialization() {
        let connection = connection();
        publish(&connection);
        connection
            .run("DELETE FROM __romedb_service_receipts", &[])
            .unwrap();
        assert_eq!(
            connection
                .initialize_service_catalog("other")
                .unwrap_err()
                .code(),
            "FDB_STORAGE"
        );
    }

    #[test]
    fn changed_receipt_identity_prevents_service_initialization() {
        let connection = connection();
        publish(&connection);
        connection
            .run("UPDATE __romedb_service_receipts SET response='{}'", &[])
            .unwrap();
        assert_eq!(
            connection
                .initialize_service_catalog("other")
                .unwrap_err()
                .code(),
            "FDB_STORAGE"
        );
    }

    #[test]
    fn modified_receipt_schema_prevents_service_initialization() {
        let connection = connection();
        connection
            .run(
                "CREATE INDEX unexpected_receipt_index ON __romedb_service_receipts(request_hash)",
                &[],
            )
            .unwrap();
        assert_eq!(
            connection
                .initialize_service_catalog("other")
                .unwrap_err()
                .code(),
            "FDB_STORAGE"
        );
    }

    #[test]
    fn publication_requires_a_transaction_and_rolls_back_with_it() {
        let connection = connection();
        let body = r#"{"requestId":"request","sequence":1,"results":[]}"#;
        assert_eq!(
            connection
                .publish_service_receipt(0, "request", &"a".repeat(64), body)
                .unwrap_err()
                .code(),
            "FDB_UNSUPPORTED"
        );
        connection
            .execute("BEGIN CONCURRENT", &crate::Parameters::new())
            .unwrap();
        connection
            .publish_service_receipt(0, "request", &"a".repeat(64), body)
            .unwrap();
        connection
            .execute("ROLLBACK", &crate::Parameters::new())
            .unwrap();
        assert_eq!(connection.service_catalog().unwrap().sequence, 0);
        assert!(connection.service_receipt("request").unwrap().is_none());
    }
}

#[derive(Debug)]
pub struct ServiceReceipt {
    pub request_hash: String,
    pub sequence: u64,
    pub response: String,
}

impl Database {
    pub fn open_for_service(path: &str) -> Result<Self> {
        if !std::path::Path::new(path).is_file() {
            return Err(Error::NotFound(
                "service requires an existing database file".into(),
            ));
        }
        let io = Arc::new(turso_core::io::PlatformIO::new()?);
        let engine = EngineDatabase::open_file_with_flags(
            io,
            path,
            turso_core::OpenFlags::default(),
            turso_core::DatabaseOpts::new().with_index_method(true),
            None,
            Arc::new(turso_core::SqliteDialect),
        )?;
        let conn = engine.connect()?;
        let mut statement = conn.prepare("PRAGMA journal_mode")?;
        let rows = crate::collect_rows(&mut statement)?;
        if !matches!(rows.as_slice(), [row] if matches!(row.as_slice(), [EngineValue::Text(mode)] if mode.as_str().eq_ignore_ascii_case("mvcc")))
        {
            return Err(Error::Unsupported(
                "service requires MVCC; back up and explicitly migrate this WAL database first"
                    .into(),
            ));
        }
        drop(statement);
        drop(conn);
        Self::initialize(engine, false)
    }
}

impl Connection {
    #[doc(hidden)]
    pub fn initialize_service_catalog(&self, database_id: &str) -> Result<ServiceCatalog> {
        if database_id.is_empty()
            || database_id.len() > 80
            || !database_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Error::Validation(
                "invalid service database identity".into(),
            ));
        }
        self.atomic(|| {
            for (name, ddl) in [
                ("__romedb_service_state", STATE_DDL),
                ("__romedb_service_receipts", RECEIPT_DDL),
            ] {
                self.run(ddl, &[])?;
                let rows = self.run(
                    "SELECT sql FROM main.sqlite_schema WHERE name=?1 AND type='table'",
                    &[text(name)],
                )?;
                let [row] = rows.as_slice() else {
                    return Err(Error::Storage("missing service metadata schema".into()));
                };
                let [EngineValue::Text(actual)] = row.as_slice() else {
                    return Err(Error::Storage("invalid service metadata schema".into()));
                };
                if crate::catalog::schema_tokens(actual.as_str())?
                    != crate::catalog::schema_tokens(ddl)?
                {
                    return Err(Error::Storage("modified service metadata schema".into()));
                }
                self.managed_dependencies(name, None)?;
            }
            let rows = self.run("SELECT slot FROM __romedb_service_state", &[])?;
            if rows.is_empty() {
                if !self
                    .run(
                        "SELECT request_id FROM __romedb_service_receipts LIMIT 1",
                        &[],
                    )?
                    .is_empty()
                {
                    return Err(Error::Storage(
                        "service receipts have no database identity".into(),
                    ));
                }
                self.run(
                    "INSERT INTO __romedb_service_state VALUES(1,1,?1,0)",
                    &[text(database_id)],
                )?;
            }
            let catalog = self.service_catalog()?;
            self.validate_service_receipts(catalog.sequence)?;
            Ok(catalog)
        })
    }

    #[doc(hidden)]
    pub fn service_catalog(&self) -> Result<ServiceCatalog> {
        let rows = self.run(
            "SELECT slot,version,database_id,sequence FROM __romedb_service_state",
            &[],
        )?;
        let [row] = rows.as_slice() else {
            return Err(Error::Storage("invalid service state count".into()));
        };
        let [slot, version, EngineValue::Text(id), sequence] = row.as_slice() else {
            return Err(Error::Storage(
                "unsupported or invalid service state".into(),
            ));
        };
        let Some(sequence) = sequence.as_int() else {
            return Err(Error::Storage("invalid service sequence type".into()));
        };
        if slot.as_int() != Some(1)
            || version.as_int() != Some(1)
            || id.as_str().is_empty()
            || id.as_str().len() > 80
            || sequence < 0
            || sequence as u64 > MAX_SEQUENCE
        {
            return Err(Error::Storage(
                "invalid service identity or sequence".into(),
            ));
        }
        Ok(ServiceCatalog {
            database_id: id.as_str().to_owned(),
            sequence: sequence as u64,
        })
    }

    #[doc(hidden)]
    pub fn service_receipt(&self, request_id: &str) -> Result<Option<ServiceReceipt>> {
        let rows = self.run("SELECT request_hash,sequence,response FROM __romedb_service_receipts WHERE request_id=?1", &[text(request_id)])?;
        if rows.is_empty() {
            return Ok(None);
        }
        let [row] = rows.as_slice() else {
            return Err(Error::Storage("duplicate service receipt".into()));
        };
        let [EngineValue::Text(hash), sequence, EngineValue::Text(response)] = row.as_slice()
        else {
            return Err(Error::Storage("invalid service receipt".into()));
        };
        let Some(sequence) = sequence.as_int() else {
            return Err(Error::Storage(
                "invalid service receipt sequence type".into(),
            ));
        };
        if sequence <= 0
            || sequence as u64 > MAX_SEQUENCE
            || hash.as_str().len() != 64
            || !hash.as_str().bytes().all(|b| b.is_ascii_hexdigit())
            || response.as_str().len() > 524_288
        {
            return Err(Error::Storage("invalid service receipt contents".into()));
        }
        let reply: serde_json::Value = crate::decode_wire_json(response.as_str())?;
        if reply["requestId"].as_str() != Some(request_id)
            || reply["sequence"].as_u64() != Some(sequence as u64)
        {
            return Err(Error::Storage("service receipt identity differs".into()));
        }
        Ok(Some(ServiceReceipt {
            request_hash: hash.as_str().to_owned(),
            sequence: sequence as u64,
            response: response.as_str().to_owned(),
        }))
    }

    fn validate_service_receipts(&self, sequence: u64) -> Result<()> {
        let rows = self.run(
            "SELECT request_id,sequence FROM __romedb_service_receipts ORDER BY sequence LIMIT 65",
            &[],
        )?;
        let count = sequence.min(64) as usize;
        if rows.len() != count {
            return Err(Error::Storage("incomplete service receipt history".into()));
        }
        for (position, row) in rows.into_iter().enumerate() {
            let [EngineValue::Text(id), stored_sequence] = row.as_slice() else {
                return Err(Error::Storage("invalid service receipt history".into()));
            };
            let expected = sequence - count as u64 + position as u64 + 1;
            if stored_sequence.as_int() != Some(expected as i64)
                || id.as_str().is_empty()
                || id.as_str().len() > 80
                || !id
                    .as_str()
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                || self.service_receipt(id.as_str())?.is_none()
            {
                return Err(Error::Storage(
                    "inconsistent service receipt history".into(),
                ));
            }
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn publish_service_receipt(
        &self,
        expected_sequence: u64,
        request_id: &str,
        request_hash: &str,
        response: &str,
    ) -> Result<()> {
        if self.transaction_state() != crate::TransactionState::Active {
            return Err(Error::Unsupported(
                "service publication requires an active transaction".into(),
            ));
        }
        if expected_sequence >= MAX_SEQUENCE
            || request_id.is_empty()
            || request_id.len() > 80
            || !request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            || request_hash.len() != 64
            || !request_hash.bytes().all(|b| b.is_ascii_hexdigit())
            || response.len() > 524_288
        {
            return Err(Error::Validation("invalid service publication".into()));
        }
        let next = expected_sequence + 1;
        let reply: serde_json::Value = crate::decode_wire_json(response)?;
        if reply["requestId"].as_str() != Some(request_id)
            || reply["sequence"].as_u64() != Some(next)
        {
            return Err(Error::Validation(
                "service publication identity differs".into(),
            ));
        }
        if self.service_catalog()?.sequence != expected_sequence {
            return Err(Error::Engine(turso_core::LimboError::BusySnapshot));
        }
        self.run(
            "INSERT INTO __romedb_service_receipts VALUES(?1,?2,?3,?4)",
            &[
                text(request_id),
                text(request_hash),
                EngineValue::from_i64(next as i64),
                text(response),
            ],
        )?;
        self.run(
            "UPDATE __romedb_service_state SET sequence=?1 WHERE slot=1 AND sequence=?2",
            &[
                EngineValue::from_i64(next as i64),
                EngineValue::from_i64(expected_sequence as i64),
            ],
        )?;
        let changed = self.run("SELECT changes()", &[])?;
        if !matches!(changed.as_slice(), [row] if matches!(row.as_slice(), [cell] if cell.as_int() == Some(1)))
        {
            return Err(Error::Storage("service sequence publication failed".into()));
        }
        self.run(
            "DELETE FROM __romedb_service_receipts WHERE sequence<=?1",
            &[EngineValue::from_i64(next.saturating_sub(64) as i64)],
        )?;
        Ok(())
    }
}
