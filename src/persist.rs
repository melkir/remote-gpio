//! Host state directory and atomic file persistence.

use anyhow::{Context, Result};
use std::fs;
use std::io;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Default runtime state directory on the Pi (systemd `StateDirectory`).
pub const SYSTEM_STATE_DIR: &str = "/var/lib/somfy";

/// Resolve the directory for `rts.json`, `hap.json`, `positions.json`, etc.
pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("STATE_DIRECTORY") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("SOMFY_STATE_DIR") {
        return PathBuf::from(dir);
    }
    default_state_dir()
}

#[cfg(debug_assertions)]
fn default_state_dir() -> PathBuf {
    PathBuf::from("./hap-state")
}

#[cfg(not(debug_assertions))]
fn default_state_dir() -> PathBuf {
    PathBuf::from(SYSTEM_STATE_DIR)
}

/// Write `bytes` via a temp file and atomic rename. Mode `0600`.
pub fn atomic_save_bytes(path: &Path, bytes: &[u8], durable: bool) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let filename = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid state path"))?;
    let tmp = parent.join(format!(".{}.tmp", filename.to_string_lossy()));
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        if durable {
            f.sync_all()?;
        }
    }
    fs::rename(&tmp, path)?;
    if durable {
        sync_parent_dir(parent)?;
    }
    Ok(())
}

fn sync_parent_dir(parent: &Path) -> Result<()> {
    match fs::File::open(parent).and_then(|dir| dir.sync_all()) {
        Ok(()) => Ok(()),
        Err(e) if directory_sync_unsupported(&e) => Ok(()),
        Err(e) => Err(e).with_context(|| format!("syncing state directory {}", parent.display())),
    }
}

fn directory_sync_unsupported(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn atomic_save_bytes_creates_private_file_and_removes_temp() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state.json");

        atomic_save_bytes(&target, br#"{"ok":true}"#, false).unwrap();

        assert_eq!(fs::read(&target).unwrap(), br#"{"ok":true}"#);
        let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!dir.path().join(".state.json.tmp").exists());
    }

    #[test]
    fn atomic_save_bytes_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state.json");

        atomic_save_bytes(&target, b"first", false).unwrap();
        atomic_save_bytes(&target, b"second", false).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"second");
        assert!(!dir.path().join(".state.json.tmp").exists());
    }

    #[test]
    fn atomic_save_bytes_supports_durable_writes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("state.json");

        atomic_save_bytes(&target, b"durable", true).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"durable");
    }
}
