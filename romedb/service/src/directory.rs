use crate::{DatabaseService, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{DirBuilder, File, OpenOptions},
    io::{ErrorKind, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::SystemTime,
};

const MANIFEST: &str = "romedb-directory.json";
const LOCK: &str = "romedb-directory.lock";
const EXTENSION: &str = "romedb";
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;

pub struct DirectoryService {
    root: PathBuf,
    organization_id: String,
    max_open: usize,
    state: Mutex<State>,
    _lock: File,
}

struct State {
    manifest: Manifest,
    open: HashMap<String, OpenDatabase>,
    clock: u64,
    closing: bool,
}

struct OpenDatabase {
    service: Arc<DatabaseService>,
    used: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    version: u32,
    organization_id: String,
    databases: Vec<Entry>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Entry {
    id: String,
    name: String,
    created_at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    name: String,
}

impl DirectoryService {
    pub fn open(root: &Path, create: bool, max_open: usize) -> Result<Self> {
        if !(1..=1024).contains(&max_open) {
            return Err(Error::new(
                "RDB_VALIDATION",
                "the open database limit must be 1..1024",
            ));
        }
        if create {
            match DirBuilder::new().mode(0o700).create(root) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        if !root.is_dir() {
            return Err(Error::new(
                "RDB_NOT_FOUND",
                "database directory is missing; use --create to create it",
            ));
        }
        let root = root.canonicalize()?;
        let lock = lock_directory(&root)?;
        let mut manifest = match read_manifest(&root)? {
            Some(manifest) => manifest,
            None => Manifest {
                version: 1,
                organization_id: uuid::Uuid::new_v4().to_string(),
                databases: Vec::new(),
            },
        };
        let files = database_files(&root)?;
        manifest
            .databases
            .retain(|entry| files.iter().any(|(id, _)| id == &entry.id));
        for (id, created) in files {
            if !manifest.databases.iter().any(|entry| entry.id == id) {
                manifest.databases.push(Entry {
                    name: format!("db-{id}"),
                    id,
                    created_at: created,
                });
            }
        }
        write_manifest(&root, &manifest)?;
        Ok(Self {
            root,
            organization_id: manifest.organization_id.clone(),
            max_open,
            state: Mutex::new(State {
                manifest,
                open: HashMap::new(),
                clock: 0,
                closing: false,
            }),
            _lock: lock,
        })
    }

    pub fn organization_id(&self) -> &str {
        &self.organization_id
    }

    pub fn list(&self, origin: &str) -> Result<Value> {
        let state = self.state()?;
        let mut databases: Vec<&Entry> = state.manifest.databases.iter().collect();
        databases.sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
        Ok(json!({"databases": databases
            .into_iter()
            .map(|entry| self.describe(entry, origin))
            .collect::<Vec<_>>()}))
    }

    pub fn create(&self, body: &str, origin: &str) -> Result<Value> {
        let request: CreateRequest = serde_json::from_str(body).map_err(|_| {
            Error::new("RDB_VALIDATION", "database creation takes {\"name\": NAME}")
        })?;
        if !valid_name(&request.name) {
            return Err(Error::new("RDB_VALIDATION", "Invalid database name"));
        }
        let mut state = self.state()?;
        if state.closing {
            return Err(Error::new("RDB_CLOSED", "service is stopping"));
        }
        if let Some(entry) = state
            .manifest
            .databases
            .iter()
            .find(|entry| entry.name == request.name)
        {
            return Ok(self.describe(entry, origin));
        }
        if state.open.len() >= self.max_open {
            self.close_least_recent(&mut state)?;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let service = Arc::new(DatabaseService::create_with_id(&self.path(&id), &id)?);
        let entry = Entry {
            id: id.clone(),
            name: request.name,
            created_at: timestamp(SystemTime::now()),
        };
        state.clock += 1;
        let used = state.clock;
        state.open.insert(id, OpenDatabase { service, used });
        state.manifest.databases.push(entry.clone());
        write_manifest(&self.root, &state.manifest)?;
        Ok(self.describe(&entry, origin))
    }

    pub fn details(&self, id: &str, origin: &str) -> Result<Value> {
        let (service, entry) = self.open_database(id)?;
        let mut details = self.describe(&entry, origin);
        if let (Value::Object(details), Value::Object(info)) = (&mut details, service.info()?) {
            details.extend(info);
        }
        Ok(details)
    }

    pub fn database(&self, id: &str) -> Result<Arc<DatabaseService>> {
        Ok(self.open_database(id)?.0)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let mut state = self.state()?;
        let position = state
            .manifest
            .databases
            .iter()
            .position(|entry| entry.id == id)
            .ok_or_else(not_found)?;
        if let Some(open) = state.open.get(id) {
            if Arc::strong_count(&open.service) > 1 {
                return Err(Error::new(
                    "RDB_BUSY",
                    "database has requests in progress; retry the deletion",
                ));
            }
            open.service.begin_shutdown();
        }
        state.open.remove(id);
        let path = self.path(id);
        std::fs::remove_file(&path)?;
        for sidecar in sidecars(&path) {
            match std::fs::remove_file(sidecar) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        File::open(&self.root)?.sync_all()?;
        state.manifest.databases.remove(position);
        write_manifest(&self.root, &state.manifest)
    }

    pub fn begin_shutdown(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closing = true;
            for open in state.open.values() {
                open.service.begin_shutdown();
            }
        }
    }

    pub fn checkpoint_for_shutdown(&self) {
        if let Ok(state) = self.state.lock() {
            for (id, open) in &state.open {
                if let Err(error) = open.service.checkpoint_for_shutdown() {
                    eprintln!("romedb-service: {id}: {}: {}", error.code, error.message);
                }
            }
        }
    }

    fn open_database(&self, id: &str) -> Result<(Arc<DatabaseService>, Entry)> {
        let mut state = self.state()?;
        let entry = state
            .manifest
            .databases
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or_else(not_found)?;
        state.clock += 1;
        let used = state.clock;
        if let Some(open) = state.open.get_mut(id) {
            open.used = used;
            return Ok((open.service.clone(), entry));
        }
        if state.closing {
            return Err(Error::new("RDB_CLOSED", "service is stopping"));
        }
        if state.open.len() >= self.max_open {
            self.close_least_recent(&mut state)?;
        }
        let service = Arc::new(DatabaseService::open_with_id(&self.path(id), id)?);
        state.open.insert(
            id.to_owned(),
            OpenDatabase {
                service: service.clone(),
                used,
            },
        );
        Ok((service, entry))
    }

    fn close_least_recent(&self, state: &mut State) -> Result<()> {
        let id = state
            .open
            .iter()
            .filter(|(_, open)| Arc::strong_count(&open.service) == 1)
            .min_by_key(|(_, open)| open.used)
            .map(|(id, _)| id.clone())
            .ok_or_else(|| {
                Error::new(
                    "RDB_BUSY",
                    "every open database has requests in progress; retry or raise --max-open",
                )
            })?;
        let open = state.open.remove(&id).expect("selected open database");
        open.service.begin_shutdown();
        if let Err(error) = open.service.checkpoint_for_shutdown() {
            eprintln!("romedb-service: {id}: {}: {}", error.code, error.message);
        }
        Ok(())
    }

    fn describe(&self, entry: &Entry, origin: &str) -> Value {
        json!({
            "id": entry.id,
            "name": entry.name,
            "organizationId": self.organization_id,
            "status": "active",
            "createdAt": entry.created_at,
            "updatedAt": entry.created_at,
            "nameRevision": 0,
            "url": format!("{origin}/v1/organizations/{}/databases/{}/query", self.organization_id, entry.id),
        })
    }

    fn path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.{EXTENSION}"))
    }

    fn state(&self) -> Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| Error::new("RDB_STORAGE", "directory state failed"))
    }
}

pub fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id)
        .is_ok_and(|parsed| parsed.get_version_num() == 4 && parsed.to_string() == id)
}

fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=48).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn not_found() -> Error {
    Error::new("RDB_NOT_FOUND", "Database not found")
}

fn lock_directory(root: &Path) -> Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(LOCK))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::new(
            "RDB_BUSY",
            "another romedb serve owns this directory",
        ));
    }
    Ok(lock)
}

fn read_manifest(root: &Path) -> Result<Option<Manifest>> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(MANIFEST))
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut text = String::new();
    Read::by_ref(&mut file)
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_string(&mut text)?;
    let invalid = || Error::new("RDB_STORAGE", "directory manifest is invalid");
    if text.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(invalid());
    }
    let manifest: Manifest = serde_json::from_str(&text).map_err(|_| invalid())?;
    let mut ids = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    if manifest.version != 1
        || !valid_id(&manifest.organization_id)
        || !manifest.databases.iter().all(|entry| {
            valid_id(&entry.id)
                && valid_name(&entry.name)
                && ids.insert(&entry.id)
                && names.insert(&entry.name)
        })
    {
        return Err(invalid());
    }
    Ok(Some(manifest))
}

fn write_manifest(root: &Path, manifest: &Manifest) -> Result<()> {
    let staged = root.join(format!("{MANIFEST}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&staged)?;
    file.write_all(&serde_json::to_vec_pretty(manifest)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(&staged, root.join(MANIFEST))?;
    File::open(root)?.sync_all()?;
    Ok(())
}

fn database_files(root: &Path) -> Result<Vec<(String, String)>> {
    let mut files = Vec::new();
    for item in std::fs::read_dir(root)? {
        let item = item?;
        let path = item.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some(EXTENSION) {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let metadata = item.metadata()?;
        if valid_id(id) && metadata.is_file() {
            files.push((id.to_owned(), timestamp(metadata.modified()?)));
        }
    }
    Ok(files)
}

fn sidecars(path: &Path) -> Vec<PathBuf> {
    let mut sidecars: Vec<PathBuf> = ["-wal", "-shm", "-log", "-journal", "-tshm"]
        .into_iter()
        .map(|suffix| {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            PathBuf::from(name)
        })
        .collect();
    sidecars.push(path.with_extension("db-log"));
    sidecars
}

fn timestamp(time: SystemTime) -> String {
    let millis = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0);
    let days = millis.div_euclid(86_400_000);
    let of_day = millis.rem_euclid(86_400_000);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3_600_000,
        of_day / 60_000 % 60,
        of_day / 1000 % 60,
        of_day % 1000
    )
}
