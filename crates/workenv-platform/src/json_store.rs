//! Private JSON state files with atomic replacement.
use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use fs2::FileExt as _;
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

/// Read one JSON file into a typed value.
///
/// # Errors
/// Returns an error when the file cannot be read or parsed as the requested type.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

/// Atomically write a private JSON file.
///
/// # Errors
/// Returns an error when the parent directory, temporary file, serialization,
/// fsync, or final rename fails.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("JSON path has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = temp_path(parent);
    let result = write_temp_then_rename(path, value, &temp);
    if temp.exists() {
        let _ignored = fs::remove_file(&temp);
    }
    result
}

/// Run a closure while holding an exclusive advisory lock.
///
/// # Errors
/// Returns an error when the lock file cannot be opened, locked, or when the
/// closure returns an error.
pub fn with_exclusive_lock<T>(path: &Path, run: impl FnOnce() -> Result<T>) -> Result<T> {
    let parent = path.parent().context("lock path has no parent")?;
    fs::create_dir_all(parent)?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("open lock {}", path.display()))?;
    lock.lock_exclusive()?;
    let result = run();
    lock.unlock()?;
    result
}

fn temp_path(parent: &Path) -> PathBuf {
    parent.join(format!(".{}.tmp", Uuid::new_v4()))
}

fn write_temp_then_rename<T: Serialize>(path: &Path, value: &T, temp: &Path) -> Result<()> {
    let mut file = private_new_file(temp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temp, path)?;
    fs::File::open(path.parent().context("JSON path has no parent")?)?.sync_all()?;
    Ok(())
}

fn private_new_file(path: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("open temporary {}", path.display()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn writes_private_complete_json() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state/receipt.json");
        write_json_atomic(&path, &json!({"phase":"done"}))?;
        let value: serde_json::Value = read_json(&path)?;
        assert_eq!(value["phase"], "done");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        }
        Ok(())
    }
}
