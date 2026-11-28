//! Files of one database for `romedb serve`, with the routes RomeDB Cloud calls
//! managed files. The bytes of a file are in a folder beside the database. The record
//! of a file is its row in the collection `romedb_files`. Upload sessions are kept in
//! memory and end with the process.
use crate::{DatabaseService, Error};
use romedb::{Document, Key, Parameters, Value};
use serde_json::{json, Map, Value as Json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
        Mutex, MutexGuard,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const FILES: &str = "romedb_files";
const ROW: &str = "{name:$name, description:$description, tags:$tags, metadata:$metadata, \
                   size:$size, mediaType:$mediaType, checksum:$checksum, generation:$generation, \
                   status:$status, createdBy:$createdBy, createdAt:$createdAt, \
                   updatedAt:$updatedAt, source:$source}";
const MIB: u64 = 1024 * 1024;
const MAX_PARTS: u64 = 10_000;
const MAX_OBJECT_BYTES: u64 = 5 * 1024 * 1024 * 1024 * 1024 - 5 * 1024 * 1024 * 1024;
const SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const SESSION_SECONDS: i64 = 24 * 3600;
const MAX_SESSION_SECONDS: i64 = 7 * 24 * 3600;
const PART_URL_SECONDS: u64 = 900;
const PARTS_PER_REQUEST: usize = 100;
const OPEN_UPLOADS: usize = 100;
const DOWNLOAD_SECONDS: i64 = 300;
const MAX_URL_SECONDS: i64 = 604_800;
const NAME_BYTES: usize = 255;
const DESCRIPTION_BYTES: usize = 2000;
const TAGS: usize = 32;
const TAG_BYTES: usize = 64;
const METADATA_BYTES: usize = 16 * 1024;
const BINDING_BYTES: usize = 2048;
const SUBJECT_BYTES: usize = 255;
const MEDIA_TYPES: usize = 32;
const LIST_LIMIT: u64 = 100;
const DOWNLOADS_PER_REQUEST: usize = 100;
const CHANGES_PER_ROUND: usize = 100;
const CHANGE_ROUNDS: usize = 5;
const FINISHED_UPLOAD_MS: u64 = 24 * 3_600_000;
const COPY_CHUNK_BYTES: usize = 256 * 1024;
const BLOCK_BYTES: u64 = 100_000_000_000;
const INLINE_TYPES: [&str; 14] = [
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/avif",
    "application/pdf",
    "text/plain",
    "audio/mpeg",
    "audio/ogg",
    "audio/wav",
    "audio/mp4",
    "video/mp4",
    "video/webm",
    "video/ogg",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Invalid,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    TooLarge,
    Limit,
    Unavailable,
}

#[derive(Debug)]
pub struct FileError {
    pub refusal: Refusal,
    pub message: String,
}
pub type FileResult<T> = Result<T, FileError>;

impl FileError {
    pub fn new(refusal: Refusal, message: impl Into<String>) -> Self {
        Self {
            refusal,
            message: message.into(),
        }
    }
}
impl From<Error> for FileError {
    fn from(error: Error) -> Self {
        let refusal = match error.code.as_str() {
            "RDB_NOT_FOUND" => Refusal::NotFound,
            "RDB_CONFLICT" | "RDB_REQUEST_IDENTITY" => Refusal::Conflict,
            "RDB_VALIDATION" => Refusal::Invalid,
            _ => Refusal::Unavailable,
        };
        Self::new(refusal, error.message)
    }
}
impl From<romedb::Error> for FileError {
    fn from(error: romedb::Error) -> Self {
        Error::from(error).into()
    }
}
impl From<io::Error> for FileError {
    fn from(error: io::Error) -> Self {
        Self::new(
            Refusal::Unavailable,
            format!("File storage failed: {error}"),
        )
    }
}

/// Stored and reserved bytes of one database, or of several added together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub bytes: u64,
    pub reserved_bytes: u64,
    pub files: u64,
    pub open_uploads: u64,
    pub peak_bytes: u64,
}

impl Totals {
    pub fn add(&mut self, other: Totals) {
        self.bytes += other.bytes;
        self.reserved_bytes += other.reserved_bytes;
        self.files += other.files;
        self.open_uploads += other.open_uploads;
        self.peak_bytes += other.peak_bytes;
    }

    /// The usage reply. A local server has no plan, no allowance and no charge.
    pub fn report(&self, organization_id: &str) -> Json {
        json!({
            "organizationId": organization_id,
            "plan": "local",
            "bytes": self.bytes,
            "reservedBytes": self.reserved_bytes,
            "files": self.files,
            "openUploads": self.open_uploads,
            "peakBytes": self.peak_bytes,
            "allowanceBytes": SAFE_INTEGER,
            "charge": {"blocks": 0, "blockBytes": BLOCK_BYTES, "usdPerBlockMonth": 0, "billedBytes": 0, "monthlyUsd": 0},
        })
    }
}

/// The bytes of a stored file, open for reading, with what a download reply needs.
pub struct Content {
    pub file: File,
    pub bytes: u64,
    pub media_type: String,
    pub disposition: String,
}

/// What a download URL asks for, once its signature and time were checked.
pub struct SignedDownload {
    disposition: String,
    name: Option<String>,
}

pub struct FileStore {
    root: PathBuf,
    database_id: String,
    organization_id: String,
    key: [u8; 32],
    started: Mutex<bool>,
    has_collection: AtomicBool,
    uploads: Mutex<HashMap<String, Upload>>,
    deletions: Mutex<HashMap<String, u64>>,
    last_file_ms: AtomicU64,
    peak_bytes: AtomicU64,
    clock_change_ms: AtomicI64,
}

#[derive(Clone)]
struct Upload {
    id: String,
    file_id: String,
    identity: String,
    name: String,
    size: u64,
    media_type: String,
    metadata: Json,
    subject: Option<String>,
    part_size: u64,
    parts: u64,
    state: UploadState,
    reason: Option<&'static str>,
    token: String,
    created_at: u64,
    expires_at: u64,
    finished_at: Option<u64>,
    urls_issued: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UploadState {
    Uploading,
    Verifying,
    Ready,
    Aborted,
    Rejected,
    Expired,
}

impl UploadState {
    fn name(self) -> &'static str {
        match self {
            Self::Uploading => "uploading",
            Self::Verifying => "verifying",
            Self::Ready => "ready",
            Self::Aborted => "aborted",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }
}

impl Upload {
    fn part_bytes(&self, number: u64) -> Option<u64> {
        if number < 1 || number > self.parts {
            None
        } else if number == self.parts {
            Some(self.size - (self.parts - 1) * self.part_size)
        } else {
            Some(self.part_size)
        }
    }
}

impl FileStore {
    /// `key` signs the part and download URLs. Make it once for each server start
    /// with [`signing_key`].
    pub fn new(root: PathBuf, database_id: &str, organization_id: &str, key: [u8; 32]) -> Self {
        Self {
            root,
            database_id: database_id.to_owned(),
            organization_id: organization_id.to_owned(),
            key,
            started: Mutex::new(false),
            has_collection: AtomicBool::new(false),
            uploads: Mutex::new(HashMap::new()),
            deletions: Mutex::new(HashMap::new()),
            last_file_ms: AtomicU64::new(0),
            peak_bytes: AtomicU64::new(0),
            clock_change_ms: AtomicI64::new(0),
        }
    }

    /// Removes what an earlier process left behind: the parts of its uploads, the bytes
    /// of rows a customer deleted with SQL, and stored bytes that have no row. Every
    /// operation that reads or writes the database runs it first, and it does its work
    /// once. A database without a files folder has nothing to remove and is not read.
    pub fn start(&self, database: &DatabaseService) -> FileResult<()> {
        let mut started = lock(&self.started);
        if *started {
            return Ok(());
        }
        if self.root.is_dir() {
            remove_folder(&self.root.join("uploads"))?;
            self.drain_changes(database)?;
            self.remove_bytes_without_a_row(database)?;
        }
        *started = true;
        Ok(())
    }

    pub fn create_upload(
        &self,
        database: &DatabaseService,
        input: &Json,
        base: &str,
    ) -> FileResult<Json> {
        self.start(database)?;
        let declared = Declaration::read(input)?;
        let identity = hex(&Sha256::digest(declared.canonical().as_bytes()));
        let now = self.now();
        let mut uploads = lock(&self.uploads);
        for upload in uploads.values_mut() {
            self.end_when_due(upload, now)?;
        }
        uploads.retain(|_, upload| {
            upload
                .finished_at
                .is_none_or(|finished| finished + FINISHED_UPLOAD_MS > now)
        });
        if let Some(existing) = uploads.get(&declared.id) {
            if existing.identity != identity {
                return Err(conflict("This upload id was used with another declaration"));
            }
            return Ok(self.session(existing, base));
        }
        let open = uploads
            .values()
            .filter(|upload| upload.state == UploadState::Uploading && now < upload.expires_at)
            .count();
        if open >= OPEN_UPLOADS {
            return Err(FileError::new(
                Refusal::Limit,
                "Too many uploads are open; finish or abort one",
            ));
        }
        let part_size = part_size(declared.size).ok_or_else(|| invalid("File size"))?;
        let upload = Upload {
            file_id: self.next_file_id(now),
            identity,
            name: declared.name,
            size: declared.size,
            media_type: declared.media_type,
            metadata: declared.metadata,
            subject: declared.subject,
            part_size,
            parts: declared.size.div_ceil(part_size),
            state: UploadState::Uploading,
            reason: None,
            token: format!("rdbu_{}", base64(&random_bytes::<32>()?, URL_SAFE, false)),
            created_at: now,
            expires_at: now + declared.expires_in_seconds * 1000,
            finished_at: None,
            urls_issued: 0,
            id: declared.id,
        };
        private_folder(&self.parts_folder(&upload.id))?;
        let session = self.session(&upload, base);
        uploads.insert(upload.id.clone(), upload);
        Ok(session)
    }

    /// True when `token` is the session token of this upload.
    pub fn token_matches(&self, upload_id: &str, token: &str) -> bool {
        lock(&self.uploads)
            .get(upload_id)
            .is_some_and(|upload| same_bytes(upload.token.as_bytes(), token.as_bytes()))
    }

    pub fn upload_status(
        &self,
        database: &DatabaseService,
        upload_id: &str,
        base: &str,
    ) -> FileResult<Json> {
        let upload = self.upload(upload_id)?;
        let mut view = self.view(&upload, base);
        let uploaded: Vec<u64> = if upload.state == UploadState::Uploading {
            (1..=upload.parts)
                .filter(|number| self.part_is_stored(&upload, *number))
                .collect()
        } else {
            Vec::new()
        };
        view.insert("uploadedParts".into(), json!(uploaded));
        if upload.state == UploadState::Ready {
            let file = self
                .row(database, &upload.file_id)?
                .map_or(Json::Null, |row| self.file_view(&upload.file_id, &row));
            view.insert("file".into(), file);
        }
        Ok(Json::Object(view))
    }

    pub fn part_urls(&self, upload_id: &str, body: &Json, base: &str) -> FileResult<Json> {
        let asked = match body.as_object() {
            Some(body) if body.len() == 1 => body.get("parts"),
            _ => None,
        }
        .ok_or_else(|| invalid("Ask for part URLs with {parts: [numbers]}"))?;
        let numbers = match asked.as_array() {
            Some(numbers) if (1..=PARTS_PER_REQUEST).contains(&numbers.len()) => numbers
                .iter()
                .map(|number| integer(number).and_then(|number| u64::try_from(number).ok()))
                .collect::<Option<Vec<u64>>>(),
            _ => None,
        }
        .ok_or_else(|| invalid("Part numbers"))?;
        if (1..numbers.len()).any(|at| numbers[..at].contains(&numbers[at])) {
            return Err(invalid("Part numbers"));
        }
        let now = self.now();
        let upload = {
            let mut uploads = lock(&self.uploads);
            let upload = self.current(&mut uploads, upload_id, now)?;
            match upload.state {
                UploadState::Uploading => {}
                UploadState::Expired => return Err(conflict("Upload session expired")),
                _ => return Err(conflict("Upload is not open")),
            }
            if numbers
                .iter()
                .any(|number| upload.part_bytes(*number).is_none())
            {
                return Err(invalid("Part numbers"));
            }
            if upload.urls_issued + numbers.len() as u64 > upload.parts * 20 + 100 {
                return Err(FileError::new(
                    Refusal::Limit,
                    "Too many part URLs asked for this upload",
                ));
            }
            upload.urls_issued += numbers.len() as u64;
            upload.clone()
        };
        let expires = now + PART_URL_SECONDS * 1000;
        let parts: Vec<Json> = numbers
            .into_iter()
            .map(|number| {
                let signature = self.sign(&part_text(&self.database_id, upload_id, number, &expires.to_string()));
                json!({
                    "partNumber": number,
                    "size": upload.part_bytes(number),
                    "url": format!("{base}/files/uploads/{upload_id}/parts/{number}?expires={expires}&signature={signature}"),
                    "expiresAt": expires,
                })
            })
            .collect();
        Ok(json!({ "parts": parts }))
    }

    /// Checks the signed URL of a part and opens a private file for its bytes.
    pub fn begin_part(&self, upload_id: &str, number: u64, query: &str) -> FileResult<PartWrite> {
        let values = query_values(query, &["expires", "signature"])
            .filter(|values| values.len() == 2)
            .ok_or_else(|| forbidden("Part URL is not valid"))?;
        let expires = values["expires"];
        if !same_bytes(
            self.sign(&part_text(&self.database_id, upload_id, number, expires))
                .as_bytes(),
            values["signature"].as_bytes(),
        ) {
            return Err(forbidden("Part URL signature is not valid"));
        }
        let now = self.now();
        if !expires.parse::<u64>().is_ok_and(|expires| now < expires) {
            return Err(forbidden("Part URL expired"));
        }
        let upload = {
            let mut uploads = lock(&self.uploads);
            self.current(&mut uploads, upload_id, now)
                .map_err(|_| forbidden("Upload is not open"))?
                .clone()
        };
        let expected = match (upload.state, upload.part_bytes(number)) {
            (UploadState::Uploading, Some(expected)) => expected,
            _ => return Err(forbidden("Upload is not open")),
        };
        let folder = self.parts_folder(upload_id);
        let staged = folder.join(format!("{number}.{}.tmp", uuid::Uuid::new_v4().simple()));
        let file = private_file(&staged).map_err(|error| match error.kind() {
            ErrorKind::NotFound => forbidden("Upload is not open"),
            _ => error.into(),
        })?;
        Ok(PartWrite {
            file: Some(file),
            staged,
            target: folder.join(number.to_string()),
            number,
            expected,
            written: 0,
        })
    }

    /// Publishes the file when every part is stored at its declared size.
    pub fn complete_upload(
        &self,
        database: &DatabaseService,
        upload_id: &str,
        body: &Json,
    ) -> FileResult<Json> {
        let expected = match body.as_object() {
            Some(body) if body.keys().all(|key| key == "crc64nvme") => {
                match body.get("crc64nvme") {
                    None => None,
                    Some(Json::String(value)) if is_crc64_text(value) => Some(value.clone()),
                    Some(_) => return Err(invalid("Upload completion")),
                }
            }
            _ => return Err(invalid("Upload completion")),
        };
        let now = self.now();
        let upload = {
            let mut uploads = lock(&self.uploads);
            let upload = self.current(&mut uploads, upload_id, now)?;
            match upload.state {
                UploadState::Uploading => upload.state = UploadState::Verifying,
                UploadState::Ready => {}
                UploadState::Verifying => {
                    return Err(conflict("Upload is completing; ask for the status"))
                }
                UploadState::Expired => return Err(conflict("Upload session expired")),
                state => return Err(conflict(&format!("Upload is {}", state.name()))),
            }
            upload.clone()
        };
        if upload.state == UploadState::Ready {
            return Ok(json!({ "file": self.file(database, &upload.file_id)? }));
        }
        match self.publish(database, &upload, expected.as_deref()) {
            Ok(file) => {
                let _ = remove_folder(&self.parts_folder(upload_id));
                self.finish(upload_id, UploadState::Ready, None);
                if let Ok(totals) = self.totals(database) {
                    self.peak_bytes.fetch_max(totals.bytes, Ordering::Relaxed);
                }
                Ok(json!({ "file": file }))
            }
            Err(Published::Rejected(reason, error)) => {
                let _ = remove_folder(&self.parts_folder(upload_id));
                self.finish(upload_id, UploadState::Rejected, Some(reason));
                Err(error)
            }
            Err(Published::Open(error)) => {
                if let Some(upload) = lock(&self.uploads).get_mut(upload_id) {
                    if upload.state == UploadState::Verifying {
                        upload.state = UploadState::Uploading;
                    }
                }
                Err(error)
            }
        }
    }

    /// Ends an open upload. Ending an ended upload changes nothing.
    pub fn abort_upload(&self, upload_id: &str, base: &str) -> FileResult<Json> {
        let now = self.now();
        let mut uploads = lock(&self.uploads);
        let upload = self.current(&mut uploads, upload_id, now)?;
        match upload.state {
            UploadState::Ready => {
                return Err(conflict("Upload is complete; delete the file instead"))
            }
            UploadState::Verifying => return Err(conflict("Upload is completing")),
            UploadState::Uploading => {
                remove_folder(&self.parts_folder(upload_id))?;
                upload.state = UploadState::Aborted;
                upload.reason = Some("Aborted by the caller");
                upload.finished_at = Some(now);
            }
            _ => {}
        }
        Ok(Json::Object(self.view(upload, base)))
    }

    pub fn file(&self, database: &DatabaseService, id: &str) -> FileResult<Json> {
        self.start(database)?;
        let row = self.row(database, id)?.ok_or_else(file_not_found)?;
        Ok(self.file_view(id, &row))
    }

    /// One page of files in the order they were made: `?after=ID&limit=N&order=asc|desc`.
    pub fn files(&self, database: &DatabaseService, query: Option<&str>) -> FileResult<Json> {
        let options = || invalid("File list options");
        let values =
            query_values(query.unwrap_or(""), &["after", "limit", "order"]).ok_or_else(options)?;
        let limit = match values.get("limit") {
            None => 50,
            Some(text)
                if (1..=3).contains(&text.len())
                    && !text.starts_with('0')
                    && text.bytes().all(|b| b.is_ascii_digit()) =>
            {
                text.parse::<u64>().map_err(|_| options())?
            }
            Some(_) => return Err(options()),
        };
        let newest_first = match values.get("order") {
            None | Some(&"asc") => false,
            Some(&"desc") => true,
            Some(_) => return Err(options()),
        };
        let after = values.get("after").copied();
        if limit > LIST_LIMIT || after.is_some_and(|after| !is_uuid(after)) {
            return Err(options());
        }
        self.start(database)?;
        self.drain_changes(database)?;
        if !self.collection_exists(database)? {
            return Ok(json!({"files": [], "next": null}));
        }
        let filter = after.map_or(String::new(), |after| {
            format!(
                " WHERE id {} {FILES}:`{after}`",
                if newest_first { "<" } else { ">" }
            )
        });
        let sql = format!(
            "SELECT * FROM {FILES}{filter} ORDER BY id{} LIMIT {}",
            if newest_first { " DESC" } else { "" },
            limit + 1
        );
        let mut rows = self.rows(database, &sql)?;
        let more = rows.len() as u64 > limit;
        rows.truncate(limit as usize);
        let next = match rows.last() {
            Some((id, _)) if more => json!(id),
            _ => Json::Null,
        };
        let files: Vec<Json> = rows
            .iter()
            .map(|(id, row)| self.file_view(id, row))
            .collect();
        Ok(json!({"files": files, "next": next}))
    }

    /// Changes name, description, tags or metadata.
    pub fn update_file(
        &self,
        database: &DatabaseService,
        id: &str,
        patch: &Json,
    ) -> FileResult<Json> {
        let update = || invalid("File update");
        let patch = patch.as_object().ok_or_else(update)?;
        if patch.is_empty()
            || patch
                .keys()
                .any(|key| !["name", "description", "tags", "metadata"].contains(&key.as_str()))
        {
            return Err(update());
        }
        let mut changes = vec![("updatedAt", Value::Integer(self.now() as i64))];
        if let Some(name) = patch.get("name") {
            let name = text(Some(name), NAME_BYTES, "File name", false)?;
            changes.push(("name", Value::String(name.into())));
        }
        if let Some(description) = patch.get("description") {
            let description = text(
                Some(description),
                DESCRIPTION_BYTES,
                "File description",
                true,
            )?;
            changes.push(("description", Value::String(description.into())));
        }
        if let Some(listed) = patch.get("tags") {
            changes.push(("tags", Value::Array(tags(listed)?)));
        }
        if let Some(metadata) = patch.get("metadata") {
            changes.push((
                "metadata",
                stored(&object(metadata, METADATA_BYTES, "File metadata")?),
            ));
        }
        self.start(database)?;
        if !is_uuid(id) || !self.collection_exists(database)? {
            return Err(file_not_found());
        }
        let fields: Vec<String> = changes
            .iter()
            .map(|(field, _)| format!("{field} = ${field}"))
            .collect();
        let results = database.host_query(&[json!({
            "sql": format!("UPDATE {FILES}:`{id}` SET {}", fields.join(", ")),
            "params": parameters(&changes)?,
        })])?;
        if affected(&results, 0) == 0 {
            return Err(file_not_found());
        }
        self.file(database, id)
    }

    /// A URL that downloads the file from this server without a credential until it
    /// expires.
    pub fn download_url(
        &self,
        database: &DatabaseService,
        id: &str,
        body: &Json,
        base: &str,
    ) -> FileResult<Json> {
        let options = body
            .as_object()
            .filter(|body| {
                body.keys().all(|key| {
                    ["expiresInSeconds", "disposition", "fileName"].contains(&key.as_str())
                })
            })
            .ok_or_else(|| invalid("Download options"))?;
        self.start(database)?;
        let row = self.row(database, id)?.ok_or_else(file_not_found)?;
        let (seconds, disposition) = download_options(options)?;
        if disposition == "inline" && !shown_inline(&row) {
            return Err(forbidden("This media type downloads as an attachment only"));
        }
        let name = match options.get("fileName") {
            None => None,
            Some(name) => Some(text(Some(name), NAME_BYTES, "File name", false)?),
        };
        let expires = self.now() + seconds * 1000;
        Ok(json!({
            "url": self.content_url(base, id, expires, disposition, name),
            "expiresAt": expires,
        }))
    }

    /// Download URLs of up to 100 files. A file that is gone is left out, and so is one
    /// that may not be shown inline when `inline` is asked.
    pub fn download_urls(
        &self,
        database: &DatabaseService,
        body: &Json,
        base: &str,
    ) -> FileResult<Json> {
        let asked = || invalid("Ask for download URLs with {files: [ids]}");
        let options = body
            .as_object()
            .filter(|body| {
                body.keys()
                    .all(|key| ["files", "expiresInSeconds", "disposition"].contains(&key.as_str()))
            })
            .ok_or_else(asked)?;
        let ids = options
            .get("files")
            .and_then(Json::as_array)
            .ok_or_else(asked)?;
        let ids = ids
            .iter()
            .map(|id| id.as_str().filter(|id| is_uuid(id)))
            .collect::<Option<Vec<&str>>>()
            .filter(|ids| {
                (1..=DOWNLOADS_PER_REQUEST).contains(&ids.len())
                    && (1..ids.len()).all(|at| !ids[..at].contains(&ids[at]))
            })
            .ok_or_else(|| invalid("Ask for download URLs with 1 to 100 different file ids"))?;
        let (seconds, disposition) = download_options(options)?;
        self.start(database)?;
        let expires = self.now() + seconds * 1000;
        let mut downloads = Vec::new();
        for id in ids {
            let Some(row) = self.row(database, id)? else {
                continue;
            };
            if disposition == "inline" && !shown_inline(&row) {
                continue;
            }
            downloads.push(json!({
                "fileId": id,
                "expiresAt": expires,
                "url": self.content_url(base, id, expires, disposition, None),
            }));
        }
        Ok(json!({ "downloads": downloads }))
    }

    /// Checks the signature and the time of a download URL. It reads nothing from the
    /// database, so a request without a valid URL costs no database work.
    pub fn check_download(&self, id: &str, query: &str) -> FileResult<SignedDownload> {
        let values = query_values(query, &["expires", "disposition", "name", "signature"])
            .ok_or_else(|| forbidden("Download URL is not valid"))?;
        let (Some(expires), Some(disposition), Some(signature)) = (
            values.get("expires"),
            values.get("disposition"),
            values.get("signature"),
        ) else {
            return Err(forbidden("Download URL is not valid"));
        };
        let name = values.get("name").copied();
        let signed = download_text(
            &self.database_id,
            id,
            expires,
            disposition,
            name.unwrap_or(""),
        );
        if !same_bytes(self.sign(&signed).as_bytes(), signature.as_bytes()) {
            return Err(forbidden("Download URL signature is not valid"));
        }
        if !expires
            .parse::<u64>()
            .is_ok_and(|expires| self.now() < expires)
        {
            return Err(forbidden("Download URL expired"));
        }
        Ok(SignedDownload {
            disposition: (*disposition).to_owned(),
            name: match name {
                Some(name) => Some(
                    percent_decoded(name).ok_or_else(|| forbidden("Download URL is not valid"))?,
                ),
                None => None,
            },
        })
    }

    /// Opens the bytes of a file for a download URL that was checked.
    pub fn content(
        &self,
        database: &DatabaseService,
        id: &str,
        download: &SignedDownload,
    ) -> FileResult<Content> {
        self.start(database)?;
        let row = self.row(database, id)?.ok_or_else(file_not_found)?;
        let name = download
            .name
            .as_deref()
            .unwrap_or_else(|| row_text(&row, "name"));
        let file = File::open(self.object(id)).map_err(|error| match error.kind() {
            ErrorKind::NotFound => file_not_found(),
            _ => error.into(),
        })?;
        Ok(Content {
            bytes: file.metadata()?.len(),
            file,
            media_type: row_text(&row, "mediaType").to_owned(),
            disposition: content_disposition(&download.disposition, name),
        })
    }

    /// Removes the row and the bytes. Deleting a deleted file returns its status.
    pub fn delete_file(&self, database: &DatabaseService, id: &str) -> FileResult<Json> {
        self.start(database)?;
        if let Some(deleted) = lock(&self.deletions).get(id) {
            return Ok(self.deletion_view(id, *deleted));
        }
        if !is_uuid(id) || !self.collection_exists(database)? {
            return Err(file_not_found());
        }
        let results = database.host_query(&[json!({"sql": format!("DELETE {FILES}:`{id}`")})])?;
        if affected(&results, 0) == 0 {
            return Err(file_not_found());
        }
        remove_file(&self.object(id))?;
        let now = self.now();
        lock(&self.deletions).insert(id.to_owned(), now);
        Ok(self.deletion_view(id, now))
    }

    /// The status of a file deleted since the server started.
    pub fn deletion(&self, id: &str) -> FileResult<Json> {
        lock(&self.deletions)
            .get(id)
            .map(|deleted| self.deletion_view(id, *deleted))
            .ok_or_else(|| FileError::new(Refusal::NotFound, "Deletion not found"))
    }

    pub fn totals(&self, database: &DatabaseService) -> FileResult<Totals> {
        self.start(database)?;
        self.drain_changes(database)?;
        let mut totals = Totals::default();
        if self.collection_exists(database)? {
            let counted = database.host_read(|connection| {
                Ok(connection.execute(
                    &format!("SELECT count(), sum(size) FROM {FILES}"),
                    &Parameters::new(),
                )?)
            })?;
            if let Some([files, bytes]) = counted.rows.first().map(Vec::as_slice) {
                totals.files = whole(files);
                totals.bytes = whole(bytes);
            }
        }
        let now = self.now();
        for upload in lock(&self.uploads).values() {
            if matches!(
                upload.state,
                UploadState::Uploading | UploadState::Verifying
            ) && now < upload.expires_at
            {
                totals.reserved_bytes += upload.size;
                totals.open_uploads += 1;
            }
        }
        totals.peak_bytes = self
            .peak_bytes
            .fetch_max(totals.bytes, Ordering::Relaxed)
            .max(totals.bytes);
        Ok(totals)
    }

    pub fn organization_id(&self) -> &str {
        &self.organization_id
    }

    fn publish(
        &self,
        database: &DatabaseService,
        upload: &Upload,
        expected: Option<&str>,
    ) -> Result<Json, Published> {
        let missing = (1..=upload.parts)
            .filter(|number| !self.part_is_stored(upload, *number))
            .count();
        if missing > 0 {
            return Err(Published::Open(conflict(&format!(
                "Upload is incomplete; {missing} of {} parts are missing",
                upload.parts
            ))));
        }
        let open = |error: io::Error| Published::Open(error.into());
        private_folder(&self.root.join("objects")).map_err(open)?;
        let staged = self
            .root
            .join("objects")
            .join(format!("{}.tmp", upload.file_id));
        let joined = self.join_parts(upload, &staged);
        let (bytes, generation, checksum) = match joined {
            Ok(joined) => joined,
            Err(error) => {
                let _ = fs::remove_file(&staged);
                return Err(open(error));
            }
        };
        if bytes != upload.size {
            let _ = fs::remove_file(&staged);
            return Err(Published::Rejected(
                "The stored object differs from the declared size",
                conflict("Stored file differs from its declaration; the upload was rejected"),
            ));
        }
        if expected.is_some_and(|expected| expected != checksum) {
            let _ = fs::remove_file(&staged);
            return Err(Published::Rejected(
                "The stored bytes differ from the checksum the client computed",
                conflict("File checksum differs; the upload was rejected"),
            ));
        }
        let object = self.object(&upload.file_id);
        fs::rename(&staged, &object).map_err(open)?;
        let now = self.now() as i64;
        let fields = [
            ("name", Value::String(upload.name.clone())),
            ("description", Value::String(String::new())),
            ("tags", Value::Array(Vec::new())),
            ("metadata", stored(&upload.metadata)),
            ("size", Value::Integer(upload.size as i64)),
            ("mediaType", Value::String(upload.media_type.clone())),
            ("checksum", Value::String(checksum)),
            ("generation", Value::String(generation)),
            ("status", Value::String("ready".into())),
            (
                "createdBy",
                upload.subject.clone().map_or(Value::Null, Value::String),
            ),
            ("createdAt", Value::Integer(now)),
            ("updatedAt", Value::Integer(now)),
            ("source", Value::Null),
        ];
        let written = self.write_row(database, &upload.file_id, &fields);
        if let Err(error) = written {
            let _ = fs::remove_file(&object);
            return Err(Published::Open(error));
        }
        let row: Document = fields
            .into_iter()
            .map(|(field, value)| (field.to_owned(), value))
            .collect();
        Ok(self.file_view(&upload.file_id, &row))
    }

    fn join_parts(&self, upload: &Upload, staged: &Path) -> io::Result<(u64, String, String)> {
        let mut object = private_file(staged)?;
        let mut hash = Sha256::new();
        let mut crc = Crc64Nvme::new();
        let mut bytes = 0u64;
        let mut chunk = vec![0u8; COPY_CHUNK_BYTES];
        for number in 1..=upload.parts {
            let mut part = File::open(self.parts_folder(&upload.id).join(number.to_string()))?;
            loop {
                let read = part.read(&mut chunk)?;
                if read == 0 {
                    break;
                }
                hash.update(&chunk[..read]);
                crc.update(&chunk[..read]);
                object.write_all(&chunk[..read])?;
                bytes += read as u64;
            }
        }
        object.sync_all()?;
        Ok((
            bytes,
            hex(&hash.finalize()[..16]),
            base64(&crc.value().to_be_bytes(), STANDARD, true),
        ))
    }

    fn write_row(
        &self,
        database: &DatabaseService,
        id: &str,
        fields: &[(&str, Value)],
    ) -> FileResult<()> {
        let mut statements = Vec::new();
        if !self.collection_exists(database)? {
            statements.push(json!({"sql": format!("CREATE TABLE IF NOT EXISTS {FILES}")}));
        }
        statements.push(json!({
            "sql": format!("UPSERT {FILES}:`{id}` {ROW}"),
            "params": parameters(fields)?,
        }));
        database.host_query(&statements)?;
        self.has_collection.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn finish(&self, upload_id: &str, state: UploadState, reason: Option<&'static str>) {
        if let Some(upload) = lock(&self.uploads).get_mut(upload_id) {
            upload.state = state;
            upload.reason = reason;
            upload.finished_at = Some(self.now());
        }
    }

    /// Acts on what a customer did to file rows with SQL. The row is the record of a
    /// file here, so a changed row needs nothing; a deleted row has bytes to remove.
    fn drain_changes(&self, database: &DatabaseService) -> FileResult<()> {
        if !self.collection_exists(database)? {
            return Ok(());
        }
        for _ in 0..CHANGE_ROUNDS {
            let listed = database.host_read(|connection| {
                Ok(connection.execute(
                    &format!("SELECT file, deleted, version FROM romedb_file_changes LIMIT {CHANGES_PER_ROUND}"),
                    &Parameters::new(),
                )?)
            })?;
            let mut statements = Vec::new();
            for change in &listed.rows {
                let [Value::Record(file), deleted, version] = change.as_slice() else {
                    continue;
                };
                let Key::String(id) = &file.key else {
                    continue;
                };
                if !is_uuid(id) {
                    continue;
                }
                if *deleted == Value::Boolean(true) {
                    remove_file(&self.object(id))?;
                }
                statements.push(json!({
                    "sql": format!("DELETE FROM romedb_file_changes WHERE file = {FILES}:`{id}` AND version = $version"),
                    "params": {"$version": version.to_portable_value()?},
                }));
            }
            if !statements.is_empty() {
                database.host_query(&statements)?;
            }
            if listed.rows.len() < CHANGES_PER_ROUND || statements.is_empty() {
                break;
            }
        }
        Ok(())
    }

    fn remove_bytes_without_a_row(&self, database: &DatabaseService) -> FileResult<()> {
        let listed = match fs::read_dir(self.root.join("objects")) {
            Ok(listed) => listed,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in listed {
            let entry = entry?;
            let name = entry.file_name();
            let has_row = match name.to_str() {
                Some(id) if is_uuid(id) => self.row(database, id)?.is_some(),
                _ => false,
            };
            if !has_row {
                remove_file(&entry.path())?;
            }
        }
        Ok(())
    }

    fn collection_exists(&self, database: &DatabaseService) -> FileResult<bool> {
        if self.has_collection.load(Ordering::Relaxed) {
            return Ok(true);
        }
        let info = database
            .host_read(|connection| Ok(connection.execute("INFO FOR DB", &Parameters::new())?))?;
        let found = matches!(
            info.rows.first().and_then(|row| row.first()),
            Some(Value::Object(info)) if matches!(
                info.get("tables"),
                Some(Value::Array(tables)) if tables.iter().any(|table| matches!(
                    table,
                    Value::Object(table) if table.get("name") == Some(&Value::String(FILES.into()))
                ))
            )
        );
        if found {
            self.has_collection.store(true, Ordering::Relaxed);
        }
        Ok(found)
    }

    fn row(&self, database: &DatabaseService, id: &str) -> FileResult<Option<Document>> {
        if !is_uuid(id) || !self.collection_exists(database)? {
            return Ok(None);
        }
        Ok(self
            .rows(database, &format!("SELECT * FROM {FILES}:`{id}`"))?
            .pop()
            .map(|(_, row)| row))
    }

    fn rows(&self, database: &DatabaseService, sql: &str) -> FileResult<Vec<(String, Document)>> {
        let result =
            database.host_read(|connection| Ok(connection.execute(sql, &Parameters::new())?))?;
        Ok(result
            .rows
            .into_iter()
            .filter_map(|mut row| match row.pop() {
                Some(Value::Object(document)) => match document.get("id") {
                    Some(Value::Record(record)) => match &record.key {
                        Key::String(id) => Some((id.clone(), document)),
                        Key::Integer(_) => None,
                    },
                    _ => None,
                },
                _ => None,
            })
            .collect())
    }

    fn file_view(&self, id: &str, row: &Document) -> Json {
        json!({
            "id": id,
            "databaseId": self.database_id,
            "status": row_text(row, "status"),
            "name": row_text(row, "name"),
            "description": row_text(row, "description"),
            "tags": row.get("tags").map_or_else(|| json!([]), plain),
            "metadata": row.get("metadata").map_or_else(|| json!({}), plain),
            "size": row.get("size").map_or(0, whole),
            "mediaType": row_text(row, "mediaType"),
            "checksum": match row.get("checksum") {
                Some(Value::String(value)) => json!({"algorithm": "crc64nvme", "value": value}),
                _ => Json::Null,
            },
            "generation": row_text(row, "generation"),
            "createdBy": row.get("createdBy").map_or(Json::Null, plain),
            "createdAt": row.get("createdAt").map_or(0, whole),
            "updatedAt": row.get("updatedAt").map_or(0, whole),
        })
    }

    fn deletion_view(&self, id: &str, deleted: u64) -> Json {
        json!({
            "fileId": id,
            "databaseId": self.database_id,
            "state": "done",
            "requestedAt": deleted,
            "finishedAt": deleted,
        })
    }

    fn session(&self, upload: &Upload, base: &str) -> Json {
        let mut view = self.view(upload, base);
        view.insert("token".into(), json!(upload.token));
        Json::Object(view)
    }

    fn view(&self, upload: &Upload, base: &str) -> Map<String, Json> {
        let mut view = Map::new();
        view.insert("id".into(), json!(upload.id));
        view.insert("fileId".into(), json!(upload.file_id));
        view.insert("databaseId".into(), json!(self.database_id));
        view.insert("organizationId".into(), json!(self.organization_id));
        view.insert("state".into(), json!(upload.state.name()));
        view.insert("name".into(), json!(upload.name));
        view.insert("size".into(), json!(upload.size));
        view.insert("mediaType".into(), json!(upload.media_type));
        view.insert("partSize".into(), json!(upload.part_size));
        view.insert("parts".into(), json!(upload.parts));
        view.insert("createdAt".into(), json!(upload.created_at));
        view.insert("expiresAt".into(), json!(upload.expires_at));
        if let Some(reason) = upload.reason {
            view.insert("reason".into(), json!(reason));
        }
        view.insert(
            "url".into(),
            json!(format!("{base}/files/uploads/{}", upload.id)),
        );
        view
    }

    fn upload(&self, upload_id: &str) -> FileResult<Upload> {
        let mut uploads = lock(&self.uploads);
        Ok(self.current(&mut uploads, upload_id, self.now())?.clone())
    }

    /// The session, ended first when its time is over.
    fn current<'a>(
        &self,
        uploads: &'a mut HashMap<String, Upload>,
        upload_id: &str,
        now: u64,
    ) -> FileResult<&'a mut Upload> {
        let upload = uploads
            .get_mut(upload_id)
            .ok_or_else(|| FileError::new(Refusal::NotFound, "Upload not found"))?;
        self.end_when_due(upload, now)?;
        Ok(upload)
    }

    fn end_when_due(&self, upload: &mut Upload, now: u64) -> io::Result<()> {
        if upload.state == UploadState::Uploading && now >= upload.expires_at {
            remove_folder(&self.parts_folder(&upload.id))?;
            upload.state = UploadState::Expired;
            upload.reason = Some("The session expired before the upload was completed");
            upload.finished_at = Some(now);
        }
        Ok(())
    }

    fn part_is_stored(&self, upload: &Upload, number: u64) -> bool {
        fs::metadata(self.parts_folder(&upload.id).join(number.to_string()))
            .is_ok_and(|stored| Some(stored.len()) == upload.part_bytes(number))
    }

    fn content_url(
        &self,
        base: &str,
        id: &str,
        expires: u64,
        disposition: &str,
        name: Option<&str>,
    ) -> String {
        let name = name.map(|name| percent_encoded(name, ""));
        let signature = self.sign(&download_text(
            &self.database_id,
            id,
            &expires.to_string(),
            disposition,
            name.as_deref().unwrap_or(""),
        ));
        let name = name.map_or(String::new(), |name| format!("&name={name}"));
        format!("{base}/files/{id}/content?expires={expires}&disposition={disposition}{name}&signature={signature}")
    }

    fn sign(&self, text: &str) -> String {
        hex(&hmac_sha256(&self.key, text.as_bytes()))
    }

    /// An identifier shaped like UUID version 7. Later files of this store get later
    /// identifiers, so they list in the order they were made.
    fn next_file_id(&self, now: u64) -> String {
        let previous = self
            .last_file_ms
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
                Some(now.max(last + 1))
            })
            .unwrap_or(0);
        uuid_v7(now.max(previous + 1), &uuid::Uuid::new_v4())
    }

    fn parts_folder(&self, upload_id: &str) -> PathBuf {
        self.root.join("uploads").join(upload_id)
    }

    fn object(&self, id: &str) -> PathBuf {
        self.root.join("objects").join(id)
    }

    fn now(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as i64);
        (now + self.clock_change_ms.load(Ordering::Relaxed)).max(0) as u64
    }

    #[cfg(test)]
    fn move_clock(&self, milliseconds: i64) {
        self.clock_change_ms
            .fetch_add(milliseconds, Ordering::Relaxed);
    }
}

enum Published {
    /// The upload stays open and can be completed again.
    Open(FileError),
    /// The upload ends with this reason.
    Rejected(&'static str, FileError),
}

/// The bytes of one part while they arrive. The part appears under its number only
/// when exactly the declared bytes were written.
pub struct PartWrite {
    file: Option<File>,
    staged: PathBuf,
    target: PathBuf,
    number: u64,
    expected: u64,
    written: u64,
}

impl PartWrite {
    pub fn expected(&self) -> u64 {
        self.expected
    }

    pub fn write(&mut self, bytes: &[u8]) -> FileResult<()> {
        self.written += bytes.len() as u64;
        if self.written > self.expected {
            return Err(self.wrong_size());
        }
        match self.file.as_mut() {
            Some(file) => Ok(file.write_all(bytes)?),
            None => Err(self.wrong_size()),
        }
    }

    pub fn finish(mut self) -> FileResult<()> {
        if self.written != self.expected {
            return Err(self.wrong_size());
        }
        drop(self.file.take());
        fs::rename(&self.staged, &self.target).map_err(|error| match error.kind() {
            ErrorKind::NotFound => forbidden("Upload is not open"),
            _ => error.into(),
        })
    }

    pub fn wrong_size(&self) -> FileError {
        invalid(&format!(
            "Part {} must be exactly {} bytes",
            self.number, self.expected
        ))
    }
}

impl Drop for PartWrite {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = fs::remove_file(&self.staged);
        }
    }
}

struct Declaration {
    id: String,
    name: String,
    size: u64,
    media_type: String,
    metadata: Json,
    subject: Option<String>,
    binding: Json,
    expires_in_seconds: u64,
}

impl Declaration {
    fn read(input: &Json) -> FileResult<Self> {
        let declaration = || invalid("Upload declaration");
        let input = input.as_object().ok_or_else(declaration)?;
        const ALLOWED: [&str; 9] = [
            "id",
            "name",
            "size",
            "mediaType",
            "metadata",
            "subject",
            "binding",
            "allowedMediaTypes",
            "expiresInSeconds",
        ];
        let id = input.get("id").and_then(Json::as_str).unwrap_or("");
        if input.keys().any(|key| !ALLOWED.contains(&key.as_str())) || !is_uuid(id) {
            return Err(declaration());
        }
        let media_type = match input.get("mediaType") {
            None | Some(Json::Null) => "application/octet-stream",
            Some(Json::String(media_type)) if is_media_type(media_type) => media_type,
            Some(_) => return Err(invalid("File media type")),
        };
        if let Some(allowed) = input.get("allowedMediaTypes") {
            let allowed = allowed
                .as_array()
                .filter(|list| (1..=MEDIA_TYPES).contains(&list.len()))
                .and_then(|list| {
                    list.iter()
                        .map(|entry| entry.as_str().filter(|entry| is_media_type(entry)))
                        .collect::<Option<Vec<&str>>>()
                })
                .ok_or_else(|| invalid("Allowed media types"))?;
            if !allowed.contains(&media_type) {
                return Err(forbidden("This media type is not allowed for the upload"));
            }
        }
        let expires_in_seconds = match input.get("expiresInSeconds") {
            None | Some(Json::Null) => SESSION_SECONDS,
            Some(seconds) => integer(seconds)
                .filter(|seconds| (60..=MAX_SESSION_SECONDS).contains(seconds))
                .ok_or_else(|| invalid("Upload session lifetime"))?,
        };
        let size = input
            .get("size")
            .and_then(integer)
            .and_then(|size| u64::try_from(size).ok())
            .filter(|size| part_size(*size).is_some())
            .ok_or_else(|| invalid("File size"))?;
        Ok(Self {
            id: id.to_owned(),
            name: text(input.get("name"), NAME_BYTES, "File name", false)?.to_owned(),
            size,
            media_type: media_type.to_owned(),
            metadata: match input.get("metadata") {
                None => json!({}),
                Some(metadata) => object(metadata, METADATA_BYTES, "File metadata")?,
            },
            subject: match input.get("subject") {
                None => None,
                Some(subject) => {
                    Some(text(Some(subject), SUBJECT_BYTES, "Upload subject", false)?.to_owned())
                }
            },
            binding: match input.get("binding") {
                None => Json::Null,
                Some(binding) => object(binding, BINDING_BYTES, "Upload binding")?,
            },
            expires_in_seconds: expires_in_seconds as u64,
        })
    }

    /// One text for one declaration, so that a second request with the same id can be
    /// compared with the first.
    fn canonical(&self) -> String {
        json!([
            self.id,
            self.name,
            self.size,
            self.media_type,
            self.metadata,
            self.subject,
            self.binding,
            self.expires_in_seconds
        ])
        .to_string()
    }
}

fn download_options(options: &Map<String, Json>) -> FileResult<(u64, &'static str)> {
    let seconds = match options.get("expiresInSeconds") {
        None => DOWNLOAD_SECONDS,
        Some(seconds) => integer(seconds)
            .filter(|seconds| (1..=MAX_URL_SECONDS).contains(seconds))
            .ok_or_else(|| invalid("Download URL lifetime"))?,
    };
    let disposition = match options.get("disposition").map(Json::as_str) {
        None | Some(Some("attachment")) => "attachment",
        Some(Some("inline")) => "inline",
        Some(_) => return Err(invalid("Download disposition")),
    };
    Ok((seconds as u64, disposition))
}

fn shown_inline(row: &Document) -> bool {
    INLINE_TYPES.contains(&row_text(row, "mediaType"))
}

fn tags(listed: &Json) -> FileResult<Vec<Value>> {
    let listed = listed
        .as_array()
        .filter(|listed| listed.len() <= TAGS)
        .ok_or_else(|| invalid("File tags"))?;
    let mut tags: Vec<Value> = Vec::new();
    for tag in listed {
        let tag = Value::String(text(Some(tag), TAG_BYTES, "File tags", false)?.to_owned());
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    Ok(tags)
}

fn text<'a>(value: Option<&'a Json>, limit: usize, what: &str, empty: bool) -> FileResult<&'a str> {
    match value.and_then(Json::as_str) {
        Some(text)
            if (empty || !text.is_empty())
                && text.len() <= limit
                && !text.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}') =>
        {
            Ok(text)
        }
        _ => Err(invalid(what)),
    }
}

fn object(value: &Json, limit: usize, what: &str) -> FileResult<Json> {
    if !value.is_object() {
        return Err(invalid(what));
    }
    if value.to_string().len() > limit {
        return Err(FileError::new(
            Refusal::Limit,
            format!("{what} is too large"),
        ));
    }
    Ok(value.clone())
}

/// A JSON number that JavaScript holds as a whole number without loss.
fn integer(value: &Json) -> Option<i64> {
    let number = value.as_number()?;
    number
        .as_i64()
        .or_else(|| {
            number
                .as_f64()
                .filter(|float| float.fract() == 0.0 && float.abs() <= SAFE_INTEGER as f64)
                .map(|float| float as i64)
        })
        .filter(|integer| integer.unsigned_abs() <= SAFE_INTEGER)
}

fn stored(value: &Json) -> Value {
    match value {
        Json::Null => Value::Null,
        Json::Bool(value) => Value::Boolean(*value),
        Json::Number(number) => integer(value).map_or_else(
            || Value::Number(number.as_f64().unwrap_or_default()),
            Value::Integer,
        ),
        Json::String(value) => Value::String(value.clone()),
        Json::Array(values) => Value::Array(values.iter().map(stored).collect()),
        Json::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(field, value)| (field.clone(), stored(value)))
                .collect(),
        ),
    }
}

fn plain(value: &Value) -> Json {
    match value {
        Value::Null | Value::Vector(_) => Json::Null,
        Value::Boolean(value) => json!(value),
        Value::Integer(value) => json!(value),
        Value::Number(value) => json!(value),
        Value::String(value) => json!(value),
        Value::Binary(bytes) => json!(bytes),
        Value::Record(record) => match &record.key {
            Key::String(key) => json!(format!("{}:{key}", record.table)),
            Key::Integer(key) => json!(format!("{}:{key}", record.table)),
        },
        Value::Array(values) => Json::Array(values.iter().map(plain).collect()),
        Value::Object(fields) => Json::Object(
            fields
                .iter()
                .map(|(field, value)| (field.clone(), plain(value)))
                .collect(),
        ),
    }
}

fn parameters(fields: &[(&str, Value)]) -> FileResult<Json> {
    let mut parameters = Map::new();
    for (field, value) in fields {
        parameters.insert(format!("${field}"), value.to_portable_value()?);
    }
    Ok(Json::Object(parameters))
}

fn affected(results: &[Json], statement: usize) -> u64 {
    results
        .get(statement)
        .and_then(|result| result["affected"].as_str())
        .and_then(|affected| affected.parse().ok())
        .unwrap_or(0)
}

fn row_text<'a>(row: &'a Document, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::String(text)) => text,
        _ => "",
    }
}

fn whole(value: &Value) -> u64 {
    match value {
        Value::Integer(value) => u64::try_from(*value).unwrap_or(0),
        _ => 0,
    }
}

fn part_text(database_id: &str, upload_id: &str, number: u64, expires: &str) -> String {
    format!("part\n{database_id}\n{upload_id}\n{number}\n{expires}")
}

fn download_text(
    database_id: &str,
    file_id: &str,
    expires: &str,
    disposition: &str,
    name: &str,
) -> String {
    format!("download\n{database_id}\n{file_id}\n{expires}\n{disposition}\n{name}")
}

/// The values of a URL query as they are written, without decoding. None when a name
/// is repeated or is not one of `allowed`.
fn query_values<'a>(query: &'a str, allowed: &[&str]) -> Option<HashMap<&'a str, &'a str>> {
    let mut values = HashMap::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=')?;
        if !allowed.contains(&name) || values.insert(name, value).is_some() {
            return None;
        }
    }
    Some(values)
}

/// A download header that keeps the file's name whatever characters it has.
pub fn content_disposition(kind: &str, name: &str) -> String {
    let ascii: String = name
        .chars()
        .flat_map(|c| {
            let kept = matches!(c, ' '..='~') && c != '"' && c != '\\';
            std::iter::repeat_n(if kept { c } else { '_' }, c.len_utf16())
        })
        .collect();
    format!(
        "{kind}; filename=\"{ascii}\"; filename*=UTF-8''{}",
        percent_encoded(name, "!~")
    )
}

/// Percent-encodes every byte except ASCII letters, digits, `-`, `_`, `.` and `kept`.
fn percent_encoded(text: &str, kept: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.".contains(&byte) || kept.as_bytes().contains(&byte)
        {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn percent_decoded(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        rest = tail;
        if byte != b'%' {
            bytes.push(byte);
            continue;
        }
        let digits = std::str::from_utf8(rest.get(..2)?).ok()?;
        bytes.push(u8::from_str_radix(digits, 16).ok()?);
        rest = &rest[2..];
    }
    String::from_utf8(bytes).ok()
}

/// The part size for a file: 8 MiB, or more when the file would need over 10,000 parts.
fn part_size(size: u64) -> Option<u64> {
    (1..=MAX_OBJECT_BYTES)
        .contains(&size)
        .then(|| (8 * MIB).max(size.div_ceil(MAX_PARTS).div_ceil(MIB) * MIB))
}

/// Lowercase hexadecimal in the 8-4-4-4-12 shape with the variant of RFC 9562, any
/// version, as the client checks it.
pub fn is_uuid(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(at, byte)| match at {
            8 | 13 | 18 | 23 => *byte == b'-',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
        })
}

fn is_media_type(text: &str) -> bool {
    let part = |part: &str| {
        let bytes = part.as_bytes();
        (1..=63).contains(&bytes.len())
            && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
            && bytes.iter().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"!#$&^_.+-".contains(byte)
            })
    };
    text.split_once('/')
        .is_some_and(|(kind, subtype)| part(kind) && part(subtype))
}

fn is_crc64_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 12
        && bytes[11] == b'='
        && bytes[..11]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
}

fn uuid_v7(milliseconds: u64, random: &uuid::Uuid) -> String {
    let time = format!("{:012x}", milliseconds & 0xffff_ffff_ffff);
    let random: Vec<char> = random
        .simple()
        .to_string()
        .chars()
        .enumerate()
        .filter(|(at, _)| *at != 12 && *at != 16)
        .map(|(_, digit)| digit)
        .collect();
    let digits = |from: usize, to: usize| random[from..to].iter().collect::<String>();
    let variant = 8 + (random[3].to_digit(16).unwrap_or(0) & 3);
    format!(
        "{}-{}-7{}-{variant:x}{}-{}",
        &time[..8],
        &time[8..],
        digits(0, 3),
        digits(4, 7),
        digits(7, 19)
    )
}

/// 32 bytes from the operating system, for the key that signs part and download URLs.
pub fn signing_key() -> io::Result<[u8; 32]> {
    random_bytes()
}

fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > block.len() {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let inner = Sha256::new()
        .chain_update(block.map(|byte| byte ^ 0x36))
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(block.map(|byte| byte ^ 0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}

/// Compares without stopping at the first difference.
fn same_bytes(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64(bytes: &[u8], alphabet: &[u8; 64], padded: bool) -> String {
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let bits = group.iter().enumerate().fold(0u32, |bits, (at, byte)| {
            bits | ((*byte as u32) << (16 - 8 * at))
        });
        for at in 0..4 {
            if at <= group.len() {
                text.push(alphabet[((bits >> (18 - 6 * at)) & 63) as usize] as char);
            } else if padded {
                text.push('=');
            }
        }
    }
    text
}

const CRC64_NVME_TABLE: [u64; 256] = {
    let mut table = [0u64; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u64;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                (value >> 1) ^ 0x9a6c_9329_ac4b_c9b5
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
};

/// CRC-64/NVME: the reflected polynomial 0x9a6c9329ac4bc9b5, started and finished
/// with all bits set.
struct Crc64Nvme(u64);

impl Crc64Nvme {
    fn new() -> Self {
        Self(!0)
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = CRC64_NVME_TABLE[((self.0 ^ *byte as u64) & 0xff) as usize] ^ (self.0 >> 8);
        }
    }

    fn value(&self) -> u64 {
        !self.0
    }
}

fn private_folder(path: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)
}

fn private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

fn remove_folder(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn invalid(message: &str) -> FileError {
    FileError::new(Refusal::Invalid, message)
}

fn forbidden(message: &str) -> FileError {
    FileError::new(Refusal::Forbidden, message)
}

fn conflict(message: &str) -> FileError {
    FileError::new(Refusal::Conflict, message)
}

fn file_not_found() -> FileError {
    FileError::new(Refusal::NotFound, "File not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "http://127.0.0.1:4321/v1/database";

    fn store(folder: &Path) -> (DatabaseService, FileStore) {
        let path = folder.join("app.romedb");
        let database = if path.exists() {
            DatabaseService::open(&path).unwrap()
        } else {
            DatabaseService::create(&path).unwrap()
        };
        let id = database.database_id.clone();
        let files = FileStore::new(database.files_folder(), &id, &id, signing_key().unwrap());
        (database, files)
    }

    fn declaration(id: u8, size: u64) -> Json {
        json!({"id": format!("0193a000-0000-4000-8000-0000000000{id:02x}"), "name": "notes.txt", "size": size, "mediaType": "text/plain"})
    }

    fn upload(database: &DatabaseService, files: &FileStore, id: u8, bytes: &[u8]) -> Json {
        let session = files
            .create_upload(database, &declaration(id, bytes.len() as u64), BASE)
            .unwrap();
        let upload_id = session["id"].as_str().unwrap();
        put_part(files, upload_id, 1, bytes).unwrap();
        files
            .complete_upload(database, upload_id, &json!({}))
            .unwrap()["file"]
            .take()
    }

    fn put_part(files: &FileStore, upload_id: &str, number: u64, bytes: &[u8]) -> FileResult<()> {
        let urls = files.part_urls(upload_id, &json!({"parts": [number]}), BASE)?;
        let url = urls["parts"][0]["url"].as_str().unwrap();
        let mut part = files.begin_part(upload_id, number, url.split_once('?').unwrap().1)?;
        part.write(bytes)?;
        part.finish()
    }

    fn refused<T>(result: FileResult<T>) -> (Refusal, String) {
        match result {
            Ok(_) => panic!("the request must be refused"),
            Err(error) => (error.refusal, error.message),
        }
    }

    #[test]
    fn hmac_sha256_gives_the_values_of_rfc_4231() {
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn crc64_nvme_gives_the_check_value_of_its_definition() {
        let mut crc = Crc64Nvme::new();
        crc.update(b"1234");
        crc.update(b"56789");
        assert_eq!(crc.value(), 0xae8b_1486_0a79_9888);
        assert_eq!(
            base64(&crc.value().to_be_bytes(), STANDARD, true),
            "rosUhgp5mIg="
        );
        assert!(is_crc64_text("rosUhgp5mIg="));
        assert!(!is_crc64_text("rosUhgp5mIg"));
        assert!(!is_crc64_text("rosUhgp5mI-="));
        assert_eq!(Crc64Nvme::new().value(), 0);
    }

    #[test]
    fn base64_gives_the_values_of_rfc_4648_in_both_alphabets() {
        for (bytes, text) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(bytes.as_bytes(), STANDARD, true), text);
            assert_eq!(
                base64(bytes.as_bytes(), URL_SAFE, false),
                text.trim_end_matches('=')
            );
        }
        assert_eq!(base64(&[0xfb, 0xff], STANDARD, true), "+/8=");
        assert_eq!(base64(&[0xfb, 0xff], URL_SAFE, false), "-_8");
        assert_eq!(base64(&[0u8; 32], URL_SAFE, false).len(), 43);
    }

    #[test]
    fn a_part_is_8_mib_until_the_file_needs_more_than_10000_parts() {
        assert_eq!(part_size(0), None);
        assert_eq!(part_size(1), Some(8 * MIB));
        assert_eq!(part_size(80_000 * MIB), Some(8 * MIB));
        assert_eq!(part_size(80_000 * MIB + 1), Some(9 * MIB));
        assert_eq!(
            part_size(MAX_OBJECT_BYTES),
            Some(MAX_OBJECT_BYTES.div_ceil(MAX_PARTS).div_ceil(MIB) * MIB)
        );
        assert_eq!(part_size(MAX_OBJECT_BYTES + 1), None);
    }

    #[test]
    fn percent_encoding_and_the_download_header_match_cloud() {
        assert_eq!(percent_encoded("a b/c?é", ""), "a%20b%2Fc%3F%C3%A9");
        assert_eq!(
            percent_decoded("a%20b%2Fc%3F%C3%A9").as_deref(),
            Some("a b/c?é")
        );
        assert_eq!(percent_decoded("%4"), None);
        assert_eq!(percent_decoded("%ff"), None);
        assert_eq!(
            content_disposition("attachment", "résumé \"1\".pdf"),
            "attachment; filename=\"r_sum_ _1_.pdf\"; filename*=UTF-8''r%C3%A9sum%C3%A9%20%221%22.pdf"
        );
        assert_eq!(
            content_disposition("inline", "a'(b)*!~\\😀.txt"),
            "inline; filename=\"a'(b)*!~___.txt\"; filename*=UTF-8''a%27%28b%29%2A!~%5C%F0%9F%98%80.txt"
        );
    }

    #[test]
    fn file_ids_have_the_shape_of_uuid_version_7_and_rise() {
        let id = uuid_v7(0x0193_a000_0001, &uuid::Uuid::new_v4());
        assert!(is_uuid(&id), "{id}");
        assert!(id.starts_with("0193a000-0001-7"), "{id}");
        assert!(is_uuid("0193a000-0000-7000-8000-000000000001"));
        assert!(!is_uuid("0193a000-0000-7000-c000-000000000001"));
        assert!(!is_uuid("0193A000-0000-7000-8000-000000000001"));
        assert!(!is_uuid("../../../../../../../../../etc/passwd"));
        let folder = tempfile::tempdir().unwrap();
        let (_, files) = store(folder.path());
        let ids: Vec<String> = (0..50).map(|_| files.next_file_id(1_000)).collect();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
    }

    #[test]
    fn media_types_and_query_values_are_read_strictly() {
        assert!(is_media_type("image/svg+xml"));
        assert!(is_media_type("application/vnd.romedb.database"));
        assert!(!is_media_type("Image/png"));
        assert!(!is_media_type("image/"));
        assert!(!is_media_type("image/png; charset=utf-8"));
        let values = query_values("expires=5&signature=ab", &["expires", "signature"]).unwrap();
        assert_eq!(values["expires"], "5");
        assert!(query_values("expires=5&expires=6", &["expires"]).is_none());
        assert!(query_values("other=5", &["expires"]).is_none());
        assert!(query_values("expires", &["expires"]).is_none());
    }

    #[test]
    fn an_upload_declaration_follows_the_rules_of_cloud() {
        let folder = tempfile::tempdir().unwrap();
        let (database, files) = store(folder.path());
        let create = |change: Json| {
            let mut input = declaration(1, 5);
            for (key, value) in change.as_object().unwrap() {
                input[key] = value.clone();
            }
            files.create_upload(&database, &input, BASE)
        };
        for (change, refusal, message) in [
            (
                json!({"id": "nope"}),
                Refusal::Invalid,
                "Upload declaration",
            ),
            (json!({"other": 1}), Refusal::Invalid, "Upload declaration"),
            (
                json!({"mediaType": "TEXT"}),
                Refusal::Invalid,
                "File media type",
            ),
            (
                json!({"allowedMediaTypes": []}),
                Refusal::Invalid,
                "Allowed media types",
            ),
            (
                json!({"allowedMediaTypes": ["image/png"]}),
                Refusal::Forbidden,
                "This media type is not allowed for the upload",
            ),
            (
                json!({"expiresInSeconds": 59}),
                Refusal::Invalid,
                "Upload session lifetime",
            ),
            (json!({"size": 0}), Refusal::Invalid, "File size"),
            (json!({"size": 1.5}), Refusal::Invalid, "File size"),
            (json!({"name": ""}), Refusal::Invalid, "File name"),
            (json!({"name": "a\nb"}), Refusal::Invalid, "File name"),
            (
                json!({"name": "x".repeat(256)}),
                Refusal::Invalid,
                "File name",
            ),
            (json!({"metadata": []}), Refusal::Invalid, "File metadata"),
            (
                json!({"metadata": {"text": "x".repeat(16 * 1024)}}),
                Refusal::Limit,
                "File metadata is too large",
            ),
            (json!({"subject": 5}), Refusal::Invalid, "Upload subject"),
            (json!({"binding": "x"}), Refusal::Invalid, "Upload binding"),
        ] {
            assert_eq!(
                refused(create(change.clone())),
                (refusal, message.to_owned()),
                "{change}"
            );
        }
        let session =
            create(json!({"allowedMediaTypes": ["text/plain"], "subject": "user-1"})).unwrap();
        assert_eq!(session["state"], "uploading");
        assert_eq!(session["partSize"], 8 * MIB);
        assert_eq!(session["parts"], 1);
        let again =
            create(json!({"allowedMediaTypes": ["text/plain"], "subject": "user-1"})).unwrap();
        assert_eq!(again, session);
        assert_eq!(
            refused(create(json!({"subject": "user-2"}))),
            (
                Refusal::Conflict,
                "This upload id was used with another declaration".to_owned()
            )
        );
    }

    #[test]
    fn a_part_url_ends_after_15_minutes_and_a_session_at_its_time() {
        let folder = tempfile::tempdir().unwrap();
        let (database, files) = store(folder.path());
        let session = files
            .create_upload(&database, &declaration(1, 5), BASE)
            .unwrap();
        let upload_id = session["id"].as_str().unwrap();
        let urls = files
            .part_urls(upload_id, &json!({"parts": [1]}), BASE)
            .unwrap();
        assert_eq!(urls["parts"][0]["size"], 5);
        let url = urls["parts"][0]["url"].as_str().unwrap();
        let (path, query) = url.split_once('?').unwrap();
        assert_eq!(path, format!("{BASE}/files/uploads/{upload_id}/parts/1"));
        drop(files.begin_part(upload_id, 1, query).unwrap());
        assert_eq!(
            refused(files.begin_part(upload_id, 2, query)).0,
            Refusal::Forbidden
        );
        let later = query.replacen("expires=", "expires=9", 1);
        assert_eq!(
            refused(files.begin_part(upload_id, 1, &later)),
            (
                Refusal::Forbidden,
                "Part URL signature is not valid".to_owned()
            )
        );
        files.move_clock(PART_URL_SECONDS as i64 * 1000);
        assert_eq!(
            refused(files.begin_part(upload_id, 1, query)),
            (Refusal::Forbidden, "Part URL expired".to_owned())
        );
        put_part(&files, upload_id, 1, b"hello").unwrap();
        files.move_clock(SESSION_SECONDS * 1000);
        assert_eq!(
            refused(files.part_urls(upload_id, &json!({"parts": [1]}), BASE)),
            (Refusal::Conflict, "Upload session expired".to_owned())
        );
        let status = files.upload_status(&database, upload_id, BASE).unwrap();
        assert_eq!(status["state"], "expired");
        assert_eq!(status["uploadedParts"], json!([]));
        assert!(!files.parts_folder(upload_id).exists());
        assert_eq!(files.totals(&database).unwrap(), Totals::default());
    }

    #[test]
    fn a_part_is_kept_only_at_its_exact_size() {
        let folder = tempfile::tempdir().unwrap();
        let (database, files) = store(folder.path());
        let session = files
            .create_upload(&database, &declaration(1, 5), BASE)
            .unwrap();
        let upload_id = session["id"].as_str().unwrap();
        for bytes in [&b"hell"[..], b"hello!"] {
            assert_eq!(
                refused(put_part(&files, upload_id, 1, bytes)),
                (
                    Refusal::Invalid,
                    "Part 1 must be exactly 5 bytes".to_owned()
                )
            );
        }
        assert_eq!(
            fs::read_dir(files.parts_folder(upload_id)).unwrap().count(),
            0
        );
        assert_eq!(
            refused(files.complete_upload(&database, upload_id, &json!({}))),
            (
                Refusal::Conflict,
                "Upload is incomplete; 1 of 1 parts are missing".to_owned()
            )
        );
        put_part(&files, upload_id, 1, b"hello").unwrap();
        let status = files.upload_status(&database, upload_id, BASE).unwrap();
        assert_eq!(status["uploadedParts"], json!([1]));
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&files.root), 0o700);
        assert_eq!(mode(&files.parts_folder(upload_id)), 0o700);
        assert_eq!(mode(&files.parts_folder(upload_id).join("1")), 0o600);
        let file = files
            .complete_upload(&database, upload_id, &json!({}))
            .unwrap()["file"]
            .take();
        assert_eq!(mode(&files.object(file["id"].as_str().unwrap())), 0o600);
        assert_eq!(file["size"], 5);
        assert_eq!(file["generation"], hex(&Sha256::digest(b"hello")[..16]));
    }

    #[test]
    fn a_start_without_a_files_folder_does_not_read_the_database() {
        let folder = tempfile::tempdir().unwrap();
        let (database, files) = store(folder.path());
        database.fence();
        files.start(&database).unwrap();
        assert!(!files.root.exists());
        assert_eq!(
            refused(files.files(&database, None)).0,
            Refusal::Unavailable
        );
    }

    #[test]
    fn a_new_start_removes_open_uploads_and_bytes_without_a_row() {
        let folder = tempfile::tempdir().unwrap();
        let (database, files) = store(folder.path());
        let kept = upload(&database, &files, 1, b"kept");
        let gone = upload(&database, &files, 2, b"gone");
        let open = files
            .create_upload(&database, &declaration(3, 5), BASE)
            .unwrap();
        put_part(&files, open["id"].as_str().unwrap(), 1, b"hello").unwrap();
        database
            .query(
                &json!({"requestId": "customer", "afterSequence": database.info().unwrap()["sequence"], "statements": [{"sql": format!("DELETE romedb_files:`{}`", gone["id"].as_str().unwrap())}]})
                    .to_string(),
            )
            .unwrap();
        let stray = files
            .root
            .join("objects/0193a000-0000-7000-8000-00000000ffff");
        fs::write(&stray, b"stray").unwrap();
        let root = files.root.clone();
        drop(files);
        drop(database);

        let (database, files) = store(folder.path());
        files.start(&database).unwrap();
        assert!(!root.join("uploads").exists());
        assert!(!stray.exists());
        assert!(!files.object(gone["id"].as_str().unwrap()).exists());
        assert!(files.object(kept["id"].as_str().unwrap()).exists());
        let listed = files.files(&database, None).unwrap();
        assert_eq!(listed["files"].as_array().unwrap().len(), 1);
        assert_eq!(listed["files"][0], kept);
        let changes = database
            .host_read(|connection| {
                Ok(connection
                    .execute("SELECT file FROM romedb_file_changes", &Parameters::new())?)
            })
            .unwrap();
        assert!(changes.rows.is_empty());
        assert_eq!(
            refused(files.upload_status(&database, open["id"].as_str().unwrap(), BASE)).0,
            Refusal::NotFound
        );
    }

    #[test]
    fn host_statements_are_one_more_request_of_the_database() {
        let folder = tempfile::tempdir().unwrap();
        let (database, files) = store(folder.path());
        assert_eq!(database.info().unwrap()["sequence"], 0);
        let customer = json!({"requestId": "customer", "afterSequence": 0, "statements": [{"sql": "CREATE TABLE IF NOT EXISTS romedb_files"}]});
        let reply: Json =
            serde_json::from_str(&database.query(&customer.to_string()).unwrap()).unwrap();
        assert_eq!(reply["error"]["code"], "RDB_VALIDATION", "{reply}");
        let file = upload(&database, &files, 1, b"hello");
        assert_eq!(database.info().unwrap()["sequence"], 2);
        let insert = json!({"requestId": "customer-2", "afterSequence": 2, "statements": [{"sql": "UPSERT romedb_files:`0193a000-0000-7000-8000-000000000009` {name:'x'}"}]});
        let reply: Json =
            serde_json::from_str(&database.query(&insert.to_string()).unwrap()).unwrap();
        assert_eq!(reply["error"]["code"], "RDB_UNSUPPORTED", "{reply}");
        assert_eq!(
            files.files(&database, None).unwrap()["files"],
            json!([file])
        );
    }
}
