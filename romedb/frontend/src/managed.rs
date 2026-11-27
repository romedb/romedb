//! Collections that the host service owns inside a customer's database.
//!
//! RomeDB Cloud keeps one row per uploaded file in `romedb_files`. The service writes
//! those rows. A customer may read and join them, change the display fields, and
//! delete a row to ask for the file's deletion. Each customer change is recorded in
//! `romedb_file_changes` in the same transaction, so the service can act on it after
//! the commit and a rollback takes the record back with the change.
//!
//! The host turns its own access on with `Connection::set_host_access`. Everything
//! else on the connection is customer access.
use crate::{Collection, Connection, Document, Error, Key, Record, Result, Value};
use std::sync::atomic::Ordering;

/// One row per ready file. Written by the host.
pub const FILES: &str = "romedb_files";
/// One row per file a customer changed or deleted since the host last looked.
pub const FILE_CHANGES: &str = "romedb_file_changes";
const RESERVED_PREFIX: &str = "romedb_";
/// The fields of a file row that belong to the customer.
const EDITABLE: [&str; 4] = ["name", "description", "tags", "metadata"];
const NAME_BYTES: usize = 255;
const DESCRIPTION_BYTES: usize = 2000;
const TAGS: usize = 32;
const TAG_BYTES: usize = 64;
const METADATA_BYTES: usize = 16 * 1024;

/// True for a table or collection name that only the host may create.
pub(crate) fn reserved(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with(RESERVED_PREFIX)
}
fn managed(name: &str) -> bool {
    name == FILES || name == FILE_CHANGES
}
fn refused(what: &str) -> Error {
    Error::Unsupported(format!(
        "{what}: {FILES} and {FILE_CHANGES} are managed by RomeDB Cloud"
    ))
}
fn invalid(field: &str, rule: &str) -> Error {
    Error::Validation(format!("{FILES}.{field} {rule}"))
}
fn plain_text(value: &str, limit: usize, empty: bool) -> bool {
    (empty || !value.is_empty()) && value.len() <= limit && !value.chars().any(char::is_control)
}
fn check_editable(document: &Document) -> Result<()> {
    match document.get("name") {
        Some(Value::String(name)) if plain_text(name, NAME_BYTES, false) => {}
        _ => return Err(invalid("name", "must be text of 1 to 255 bytes")),
    }
    match document.get("description") {
        None => {}
        Some(Value::String(text)) if plain_text(text, DESCRIPTION_BYTES, true) => {}
        _ => return Err(invalid("description", "must be text of at most 2000 bytes")),
    }
    match document.get("tags") {
        None => {}
        Some(Value::Array(tags))
            if tags.len() <= TAGS
                && tags.iter().all(
                    |tag| matches!(tag, Value::String(tag) if plain_text(tag, TAG_BYTES, false)),
                ) => {}
        _ => return Err(invalid("tags", "must be at most 32 texts of 1 to 64 bytes")),
    }
    match document.get("metadata") {
        None => {}
        Some(Value::Object(metadata))
            if crate::value::encode_document(metadata)?.len() <= METADATA_BYTES => {}
        _ => return Err(invalid("metadata", "must be an object of at most 16 KiB")),
    }
    Ok(())
}

impl Connection {
    /// Turns the host's own access on or off for this connection.
    ///
    /// With it on, statements may create and write `romedb_files` and
    /// `romedb_file_changes` freely and record nothing in `romedb_file_changes`.
    /// A host sets it only for requests that it wrote itself, never for customer SQL.
    pub fn set_host_access(&self, host: bool) {
        self.host_access.store(host, Ordering::Relaxed);
    }
    pub fn host_access(&self) -> bool {
        self.host_access.load(Ordering::Relaxed)
    }
    /// Says what the customer credential behind this connection may do to file rows:
    /// change their display fields (`write`) and delete them (`delete`). Both are
    /// allowed until a host says otherwise. Host access is not affected.
    pub fn set_file_access(&self, write: bool, delete: bool) {
        self.file_write_refused.store(!write, Ordering::Relaxed);
        self.file_delete_refused.store(!delete, Ordering::Relaxed);
    }
    /// How many customer changes to file rows this connection has recorded since the
    /// last call. A host reads it before it commits a customer request: above zero
    /// means `romedb_file_changes` has rows to act on after the commit.
    pub fn take_file_changes(&self) -> u64 {
        self.file_changes.swap(0, Ordering::Relaxed)
    }

    /// Refuses a customer's new table or collection under a name kept for the host.
    pub(crate) fn managed_create(&self, name: &str) -> Result<()> {
        if !reserved(name) || (self.host_access() && managed(name)) {
            return Ok(());
        }
        Err(Error::Validation(format!(
            "names that start with {RESERVED_PREFIX} are reserved"
        )))
    }
    /// Refuses a customer's change to the definition of a managed collection.
    pub(crate) fn managed_schema(&self, name: &str) -> Result<()> {
        if managed(name) && !self.host_access() {
            return Err(refused("the definition cannot change"));
        }
        Ok(())
    }
    pub(crate) fn managed_insert(&self, collection: &Collection) -> Result<()> {
        if managed(&collection.name) && !self.host_access() {
            return Err(refused("rows cannot be inserted"));
        }
        Ok(())
    }
    /// A customer may change the display fields of a file row, and nothing else.
    pub(crate) fn managed_update(&self, collection: &Collection, after: &Document) -> Result<()> {
        if !managed(&collection.name) || self.host_access() {
            return Ok(());
        }
        if collection.name != FILES {
            return Err(refused("rows cannot be updated"));
        }
        if self.file_write_refused.load(Ordering::Relaxed) {
            return Err(Error::Unsupported(format!(
                "this credential may not change rows of {FILES}; it needs the files:write scope"
            )));
        }
        let Some(Value::Record(id)) = after.get("id") else {
            return Err(Error::Storage("file row without an id".into()));
        };
        let Some(before) = self.get_in(collection, id)? else {
            return Ok(());
        };
        for field in before.keys().chain(after.keys()) {
            if !EDITABLE.contains(&field.as_str()) && before.get(field) != after.get(field) {
                return Err(invalid(
                    field,
                    "is set by the service; only name, description, tags and metadata can change",
                ));
            }
        }
        check_editable(after)?;
        if EDITABLE
            .iter()
            .any(|field| before.get(*field) != after.get(*field))
        {
            self.record_file_change(&id.key, false)?;
        }
        Ok(())
    }
    /// A customer's delete of a file row is the request to delete the file.
    pub(crate) fn managed_delete(
        &self,
        collection: &Collection,
        document: &Document,
    ) -> Result<()> {
        if !managed(&collection.name) || self.host_access() {
            return Ok(());
        }
        if collection.name != FILES {
            return Err(refused("rows cannot be deleted"));
        }
        if self.file_delete_refused.load(Ordering::Relaxed) {
            return Err(Error::Unsupported(format!(
                "this credential may not delete rows of {FILES}; it needs the files:delete scope"
            )));
        }
        let Some(Value::Record(id)) = document.get("id") else {
            return Err(Error::Storage("file row without an id".into()));
        };
        self.record_file_change(&id.key, true)
    }
    /// Creates the change collection beside the file collection, so that a customer
    /// write never has to create one.
    pub(crate) fn managed_created(&self, name: &str) -> Result<()> {
        if name == FILES {
            self.create_collection(FILE_CHANGES, true)?;
        }
        Ok(())
    }
    // The engine writes the change record itself, whoever asked for the change.
    fn record_file_change(&self, key: &Key, deleted: bool) -> Result<()> {
        let was_host = self.host_access.swap(true, Ordering::Relaxed);
        let recorded = self.write_file_change(key, deleted);
        self.host_access.store(was_host, Ordering::Relaxed);
        if recorded.is_ok() {
            self.file_changes.fetch_add(1, Ordering::Relaxed);
        }
        recorded
    }
    // One row per file: a later change raises its version, and a delete stays a delete.
    fn write_file_change(&self, key: &Key, deleted: bool) -> Result<()> {
        let changes = self.catalog(FILE_CHANGES)?;
        let id = Record {
            table: FILE_CHANGES.into(),
            key: key.clone(),
        };
        let previous = self.get_in(&changes, &id)?;
        let version = match previous.as_ref().and_then(|row| row.get("version")) {
            Some(Value::Integer(version)) => version + 1,
            _ => 1,
        };
        let deleted = deleted
            || matches!(
                previous.as_ref().and_then(|row| row.get("deleted")),
                Some(Value::Boolean(true))
            );
        self.upsert(
            FILE_CHANGES,
            Document::from([
                ("id".into(), Value::Record(id)),
                (
                    "file".into(),
                    Value::Record(Record {
                        table: FILES.into(),
                        key: key.clone(),
                    }),
                ),
                ("deleted".into(), Value::Boolean(deleted)),
                ("version".into(), Value::Integer(version)),
            ]),
        )?;
        Ok(())
    }
}
