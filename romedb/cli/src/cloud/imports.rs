//! Generic organization import protocol; no provider or storage credentials.
use super::{Cloud, Method, Result, Value};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::ExitCode;

const PART: usize = 8 * 1024 * 1024;
pub(super) const HELP: &str = "romedb cloud import start ORGANIZATION_UUID NAME SNAPSHOT JOURNAL\nromedb cloud import resume JOURNAL SNAPSHOT\nromedb cloud import status ORGANIZATION_UUID IMPORT_UUID\nromedb cloud import cancel ORGANIZATION_UUID IMPORT_UUID --confirm\nUse a consistent SQLite backup/export, not a live file missing its WAL.\nKeep the journal to resume uncertain requests. It contains no API key.\nOnly state=ready confirms publication. Status/cancel work with starts disabled.";

fn uuid(value: &str) -> Result<String> {
    let id = uuid::Uuid::parse_str(value).map_err(|_| "Invalid import/organization UUID")?;
    if id.get_version_num() != 4 || id.to_string() != value {
        return Err("Expected a canonical lowercase version 4 UUID".into());
    }
    Ok(id.to_string())
}
fn path(org: &str, id: &str) -> Result<String> {
    Ok(format!(
        "/v1/organizations/{}/imports/{}",
        uuid(org)?,
        uuid(id)?
    ))
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("Missing import {key}").into())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_size(size: u64) -> bool {
    (4096..=10_000_000_000).contains(&size) && size % 4096 == 0
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
struct Snapshot {
    file: File,
    size: u64,
    hash: String,
    parts: Vec<String>,
}
impl Snapshot {
    fn open(source: &str) -> Result<Self> {
        let mut file = File::open(source)?;
        let meta = file.metadata()?;
        let size = meta.len();
        if !meta.is_file() || !valid_size(size) {
            return Err(
                "Choose a regular SQLite snapshot, 4 KiB to 10 GB, aligned to 4 KiB".into(),
            );
        }
        let mut header = [0; 16];
        file.read_exact(&mut header)?;
        if &header != b"SQLite format 3\0" {
            return Err("Invalid SQLite snapshot header".into());
        }
        file.seek(SeekFrom::Start(0))?;
        let mut full = Sha256::new();
        let mut parts = Vec::new();
        let mut bytes = vec![0; PART];
        let mut at = 0;
        while at < size {
            let n = (size - at).min(PART as u64) as usize;
            file.read_exact(&mut bytes[..n])?;
            full.update(&bytes[..n]);
            parts.push(digest(&bytes[..n]));
            at += n as u64;
        }
        if file.metadata()?.len() != size {
            return Err("Snapshot changed while hashing".into());
        }
        Ok(Self {
            file,
            size,
            hash: format!("{:x}", full.finalize()),
            parts,
        })
    }
    fn part(&mut self, index: usize) -> Result<Vec<u8>> {
        if self.file.metadata()?.len() != self.size {
            return Err("Snapshot changed during upload".into());
        }
        self.file
            .seek(SeekFrom::Start(index as u64 * PART as u64))?;
        let mut bytes = vec![0; (self.size - index as u64 * PART as u64).min(PART as u64) as usize];
        self.file.read_exact(&mut bytes)?;
        if digest(&bytes) != self.parts[index] {
            return Err("Snapshot changed during upload; keep the journal".into());
        }
        Ok(bytes)
    }
}
fn load(journal: &str, cloud: &Cloud) -> Result<Value> {
    let value = super::journal::load(journal, 4096)?;
    if value["version"] != 1 || text(&value, "origin")? != cloud.origin.as_str() {
        return Err(
            "Journal version or ROMEDB_CLOUD_URL does not match; do not create a replacement job"
                .into(),
        );
    }
    uuid(text(&value, "id")?)?;
    uuid(text(&value, "organizationId")?)?;
    let declaration = &value["declaration"];
    uuid(text(declaration, "databaseId")?)?;
    let hash = text(declaration, "sha256")?;
    if !valid_name(text(declaration, "name")?)
        || !declaration["size"].as_u64().is_some_and(valid_size)
        || hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || declaration.as_object().map(|o| o.len()) != Some(4)
    {
        return Err("Invalid import declaration in journal".into());
    }
    Ok(value)
}
fn verify<'a>(job: &'a Value, journal: &Value) -> Result<&'a str> {
    if job["id"] != journal["id"] || job["organizationId"] != journal["organizationId"] {
        return Err("Import response identity differs from journal".into());
    }
    for key in ["databaseId", "name", "size", "sha256"] {
        if job[key] != journal["declaration"][key] {
            return Err("Import response declaration differs from journal".into());
        }
    }
    let state = text(job, "state")?;
    if ![
        "creating",
        "uploading",
        "completing",
        "processing",
        "publishing",
        "canceling",
        "ready",
        "canceled",
    ]
    .contains(&state)
    {
        return Err("Unknown import state; keep the journal".into());
    }
    if state == "ready"
        && (job["result"]["databaseId"] != journal["declaration"]["databaseId"]
            || job["result"]["logicalRows"].as_u64().is_none()
            || job["result"]["bytes"].as_u64().is_none())
    {
        return Err("Invalid published import result".into());
    }
    Ok(state)
}
fn status(cloud: &Cloud, path: &str) -> Result<Value> {
    cloud
        .request(Method::GET, path, None)
        .map_err(|e| e.message.into())
}
fn upload(
    cloud: &Cloud,
    journal: &Value,
    source: &str,
    mut snapshot: Option<Snapshot>,
) -> Result<ExitCode> {
    let path = path(text(journal, "organizationId")?, text(journal, "id")?)?;
    eprintln!(
        "Import {}. Keep the journal; resume this identity after any interruption.",
        text(journal, "id")?
    );
    let mut job = match cloud.request(Method::GET, &path, None) {
        Ok(job) => job,
        Err(error) if error.status == Some(404) => {
            // A missing job is replayed only with its already-synced declaration.
            if snapshot.is_none() {
                snapshot = Some(Snapshot::open(source)?);
            }
            match_source(snapshot.as_ref().unwrap(), journal)?;
            cloud
                .request(Method::PUT, &path, Some(&journal["declaration"]))
                .map_err(|e| e.message)?
        }
        Err(error) => return Err(error.message.into()),
    };
    // Capacity reservation is asynchronous. Bounded polling never changes the ID.
    for _ in 0..30 {
        if verify(&job, journal)? != "creating" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
        job = status(cloud, &path)?;
    }
    if verify(&job, journal)? != "uploading" {
        cloud.output(&job)?;
        return Ok(ExitCode::SUCCESS);
    }
    if snapshot.is_none() {
        snapshot = Some(Snapshot::open(source)?);
    }
    let snapshot = snapshot.as_mut().unwrap();
    match_source(snapshot, journal)?;
    // Creation replies have no receipt page; GET is authoritative for resume.
    job = status(cloud, &path)?;
    let mut received = vec![false; snapshot.parts.len()];
    let mut after = 0;
    loop {
        if verify(&job, journal)? != "uploading" {
            cloud.output(&job)?;
            return Ok(ExitCode::SUCCESS);
        }
        let receipts = job["partReceipts"]
            .as_array()
            .ok_or("Missing part receipts")?;
        if receipts.len() > 100 {
            return Err("Import receipt page exceeds 100 parts".into());
        }
        let mut last = after;
        for receipt in receipts {
            let n = receipt["number"].as_u64().ok_or("Invalid receipt number")? as usize;
            if n <= last
                || n > snapshot.parts.len()
                || receipt["sha256"] != snapshot.parts[n - 1]
                || receipt["size"].as_u64()
                    != Some((snapshot.size - (n - 1) as u64 * PART as u64).min(PART as u64))
            {
                return Err("Import receipt does not match snapshot".into());
            }
            let state = text(receipt, "state")?;
            if !["uploaded", "uploading"].contains(&state) {
                return Err("Invalid part receipt state".into());
            }
            received[n - 1] = state == "uploaded";
            last = n;
        }
        if job["nextPart"].is_null() {
            break;
        }
        let next = job["nextPart"].as_u64().ok_or("Invalid receipt cursor")? as usize;
        if next <= after || next != last {
            return Err("Invalid receipt cursor".into());
        }
        after = next;
        job = status(cloud, &format!("{path}?after={after}"))?;
    }
    for (i, confirmed) in received.iter().enumerate() {
        if !confirmed {
            let bytes = snapshot.part(i)?;
            let size = bytes.len();
            let request = cloud
                .http
                .put(cloud.origin.join(&format!("{path}/parts/{}", i + 1))?)
                .bearer_auth(&cloud.key)
                .header("content-type", "application/octet-stream")
                .header("x-romedb-part-sha256", &snapshot.parts[i])
                .body(bytes);
            let receipt = cloud.response(request).map_err(|e| e.message)?;
            if receipt["number"].as_u64() != Some((i + 1) as u64)
                || receipt["size"].as_u64() != Some(size as u64)
                || receipt["sha256"] != snapshot.parts[i]
            {
                return Err("Part receipt unconfirmed; resume the same journal".into());
            }
        }
        eprintln!(
            "{} of {} bytes confirmed",
            ((i + 1) as u64 * PART as u64).min(snapshot.size),
            snapshot.size
        );
    }
    let job = cloud
        .request(Method::POST, &format!("{path}/complete"), None)
        .map_err(|e| e.message)?;
    verify(&job, journal)?;
    cloud.output(&job)?;
    eprintln!("Check import status until ready or canceled. Only ready confirms publication.");
    Ok(ExitCode::SUCCESS)
}
fn match_source(snapshot: &Snapshot, journal: &Value) -> Result<()> {
    if journal["declaration"]["size"].as_u64() != Some(snapshot.size)
        || journal["declaration"]["sha256"] != snapshot.hash
    {
        return Err("Select the exact snapshot recorded in the journal".into());
    }
    Ok(())
}
pub(super) fn run(cloud: &Cloud, args: &[&str]) -> Result<ExitCode> {
    match args {
        ["start", org, name, source, journal] => {
            uuid(org)?;
            if !valid_name(name) {
                return Err("Invalid destination name".into());
            }
            if Path::new(journal).try_exists()? {
                return Err("Journal exists; use resume with the original snapshot".into());
            }
            let snapshot = Snapshot::open(source)?;
            let value = json!({"version":1,"origin":cloud.origin.as_str(),"organizationId":org,
                "id":uuid::Uuid::new_v4().to_string(),"declaration":{"databaseId":uuid::Uuid::new_v4().to_string(),
                "name":name,"size":snapshot.size,"sha256":snapshot.hash}});
            super::journal::save(journal, &value)?;
            upload(cloud, &value, source, Some(snapshot))
        }
        ["resume", journal, source] => upload(cloud, &load(journal, cloud)?, source, None),
        ["status", org, id] | ["cancel", org, id, "--confirm"] => {
            let base = path(org, id)?;
            let result = if args[0] == "cancel" {
                cloud
                    .request(Method::POST, &format!("{base}/cancel"), None)
                    .map_err(|e| e.message)?
            } else {
                status(cloud, &base)?
            };
            if result["id"] != *id || result["organizationId"] != *org {
                return Err("Import response identity differs".into());
            }
            cloud.output(&result)?;
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(HELP.into()),
    }
}
