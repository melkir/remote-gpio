//! Shared paths and filesystem helpers for install/upgrade/doctor.

use anyhow::{bail, Result};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub const BIN_PATH: &str = "/usr/local/bin/somfy";
pub const BIN_PREV: &str = "/usr/local/bin/somfy.prev";
pub const UNIT_PATH: &str = "/etc/systemd/system/somfy.service";
pub const BIN_DIR: &str = "/usr/local/bin";

pub fn require_root(command: &str) -> Result<()> {
    if !nix::unistd::Uid::current().is_root() {
        bail!("{command} must be run as root (use sudo)");
    }
    Ok(())
}

pub fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("/"));
    let filename = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid path {}", path.display()))?;
    let tmp = parent.join(format!(".{}.tmp", filename.to_string_lossy()));

    let existing_mode = fs::metadata(path).ok().map(|m| m.mode() & 0o777);
    let mode = existing_mode.unwrap_or(0o644);

    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_file_with_mode_0644() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("somfy.service");
        atomic_write(&target, "hello\n").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello\n");
        let mode = fs::metadata(&target).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o644);
        let tmp_sibling = dir.path().join(".somfy.service.tmp");
        assert!(!tmp_sibling.exists());
    }

    #[test]
    fn atomic_write_preserves_existing_mode() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        atomic_write(&target, "first\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o664)).unwrap();
        atomic_write(&target, "second\n").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "second\n");
        let mode = fs::metadata(&target).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o664);
    }
}
