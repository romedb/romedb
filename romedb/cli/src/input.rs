use rustyline::{config::Behavior, error::ReadlineError, Config, DefaultEditor};
use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub enum Read {
    Line(Vec<u8>),
    Interrupted,
}
pub trait Input {
    fn read(&mut self, label: &str, prompt: &mut dyn Write, limit: usize) -> Result<Read>;
    fn remember(&mut self, _sql: &str) -> Result<()> {
        Ok(())
    }
}
pub struct Plain<'a, R>(pub &'a mut R);
impl<R: BufRead> Input for Plain<'_, R> {
    fn read(&mut self, label: &str, prompt: &mut dyn Write, limit: usize) -> Result<Read> {
        write!(prompt, "{label}")?;
        prompt.flush()?;
        Ok(Read::Line(crate::read_input(self.0, limit, true)?))
    }
}

// PreferTerm avoids mixing terminal escape sequences into JSON stdout. Only
// select it when /dev/tty is available, since Rustyline otherwise uses stdout.
pub fn terminal_available() -> bool {
    #[cfg(unix)]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .is_ok()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

pub struct Terminal {
    editor: DefaultEditor,
    history: Option<PathBuf>,
    database: PathBuf,
}
impl Terminal {
    pub fn new(history: Option<&Path>, database: &Path) -> Result<Self> {
        if let Some(history) = history {
            validate_history_path(history, database)?;
        }
        let config = Config::builder()
            .behavior(Behavior::PreferTerm)
            .max_history_size(100)?
            .history_ignore_space(true)
            .build();
        let mut editor = DefaultEditor::with_config(config)?;
        if let Some(path) = history {
            match std::fs::metadata(path) {
                Ok(metadata) => {
                    if !metadata.is_file() || metadata.len() > 8 * 1024 * 1024 {
                        return Err("history must be a regular file no larger than 8 MiB".into());
                    }
                    editor.load_history(path)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(Self {
            editor,
            history: history.map(Path::to_path_buf),
            database: database.to_path_buf(),
        })
    }
    pub fn save(&mut self) -> Result<()> {
        if let Some(path) = &self.history {
            validate_history_path(path, &self.database)?;
            self.editor.save_history(path)?;
        }
        Ok(())
    }
}
impl Input for Terminal {
    fn read(&mut self, label: &str, _prompt: &mut dyn Write, _limit: usize) -> Result<Read> {
        match self.editor.readline(label) {
            Ok(mut line) => {
                line.push('\n');
                Ok(Read::Line(line.into_bytes()))
            }
            Err(ReadlineError::Interrupted) => Ok(Read::Interrupted),
            Err(ReadlineError::Eof) => Ok(Read::Line(Vec::new())),
            Err(error) => Err(error.into()),
        }
    }
    fn remember(&mut self, sql: &str) -> Result<()> {
        // Keep history bounded independently of the SQL submission limit.
        if sql.len() <= 64 * 1024 {
            self.editor.add_history_entry(sql.trim_end())?;
        }
        Ok(())
    }
}

fn resolved(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let name = path
                .file_name()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "expected file path"))?;
            Ok(parent.canonicalize()?.join(name))
        }
        Err(error) => Err(error),
    }
}
// History is written on exit. Reject collisions before opening the database,
// including sidecars and existing aliases, so saving cannot truncate its data.
fn validate_history_path(history: &Path, database: &Path) -> Result<()> {
    if database == Path::new(":memory:") {
        return Ok(());
    }
    let history_path = resolved(history)?;
    for suffix in ["", "-wal", "-shm"] {
        let mut candidate = database.as_os_str().to_os_string();
        candidate.push(suffix);
        let candidate = PathBuf::from(candidate);
        let mut same = history_path == resolved(&candidate)?;
        #[cfg(unix)]
        if let (Ok(history), Ok(database)) =
            (std::fs::metadata(history), std::fs::metadata(&candidate))
        {
            use std::os::unix::fs::MetadataExt;
            same |= history.dev() == database.dev() && history.ino() == database.ino();
        }
        if same {
            return Err("history path must differ from the database and its sidecars".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_cannot_replace_database_or_sidecars() {
        let root = std::env::temp_dir().join(format!(
            "romedb-history-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let database = root.join("db");
        for name in ["db", "db-wal", "db-shm"] {
            assert!(validate_history_path(&root.join(name), &database).is_err());
        }
        assert!(validate_history_path(&root.join("history"), &database).is_ok());
        std::fs::write(&database, b"database sentinel").unwrap();
        let alias = root.join("alias");
        std::fs::hard_link(&database, &alias).unwrap();
        assert!(validate_history_path(&alias, &database).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&database, root.join("symlink")).unwrap();
            assert!(validate_history_path(&root.join("symlink"), &database).is_err());
        }
        assert_eq!(std::fs::read(&database).unwrap(), b"database sentinel");
        std::fs::remove_dir_all(root).unwrap();
    }
}
