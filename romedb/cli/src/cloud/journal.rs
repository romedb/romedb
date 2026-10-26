use super::{Result, Value};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

pub(super) fn load(path: &str, limit: usize) -> Result<Value> {
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err("Journal must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("Journal exceeds its size limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid recovery journal".into())
}
pub(super) fn save(path: &str, value: &Value) -> Result<()> {
    let path = Path::new(path);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Publish a complete immutable identity without replacing an existing one.
    // Sync contents and directory before any request that could execute work.
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(&serde_json::to_vec(value)?)?;
    temp.as_file().sync_all()?;
    let file = temp
        .persist_noclobber(path)
        .map_err(|_| "Cannot create journal; keep existing file and use retry/resume")?;
    file.sync_all()?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
