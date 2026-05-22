//! Shared paths and filesystem helpers for install/upgrade/doctor.

use anyhow::{bail, Context, Result};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::systemd;

pub const BIN_PATH: &str = "/usr/local/bin/somfy";
pub const BIN_PREV: &str = "/usr/local/bin/somfy.prev";
pub const UNIT_PATH: &str = "/etc/systemd/system/somfy.service";
pub const STAGED_DOWNLOAD: &str = "/usr/local/bin/.somfy.download";

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

/// Write `contents` when missing or different from on-disk text (trimmed).
pub fn atomic_write_if_changed(path: &Path, contents: &str) -> Result<bool> {
    let on_disk = fs::read_to_string(path).ok();
    let needs_write = match &on_disk {
        Some(existing) => existing.trim() != contents.trim(),
        None => true,
    };
    if needs_write {
        atomic_write(path, contents)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn command_exists(command: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(command).is_file())
}

pub fn run_command(command: &str, args: &[&str]) -> Result<()> {
    use std::process::Command;
    let output = Command::new(command).args(args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{} {} failed: {}", command, args.join(" "), stderr.trim());
    }
    Ok(())
}

/// Snapshot of `somfy.service` state before a binary swap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServiceState {
    state: &'static str,
}

impl ServiceState {
    pub fn capture() -> Self {
        Self::from_state(systemd::is_active("somfy").unwrap_or_default().as_str())
    }

    pub fn from_state(state: &str) -> Self {
        let state = match state {
            "active" => "active",
            "activating" => "activating",
            "reloading" => "reloading",
            "deactivating" => "deactivating",
            "failed" => "failed",
            "inactive" => "inactive",
            _ => "unknown",
        };
        Self { state }
    }

    pub fn state_label(self) -> &'static str {
        self.state
    }

    pub fn was_running(self) -> bool {
        matches!(self.state, "active" | "activating" | "reloading")
    }
}

fn stop_somfy_best_effort() {
    if let Err(e) = systemd::systemctl(&["stop", "somfy"]) {
        tracing::warn!("systemctl stop reported: {e}");
    }
}

fn start_somfy() -> Result<()> {
    systemd::systemctl(&["start", "--no-block", "somfy"]).context("starting somfy")
}

pub fn restart_somfy() -> Result<()> {
    systemd::systemctl(&["restart", "--no-block", "somfy"]).context("restarting somfy")
}

pub fn enable_somfy() -> Result<()> {
    systemd::systemctl(&["enable", "somfy"]).context("enabling somfy")
}

async fn wait_somfy_active(timeout: std::time::Duration) -> Result<()> {
    use std::time::Instant;
    let deadline = Instant::now() + timeout;
    loop {
        let state = systemd::is_active("somfy").unwrap_or_default();
        match state.as_str() {
            "active" => return Ok(()),
            "failed" => bail!("service entered failed state"),
            _ => {}
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for active state (last={state})");
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Move the live binary to `somfy.prev` when present.
fn archive_live_binary() -> Result<()> {
    let bin_path = Path::new(BIN_PATH);
    let prev_path = Path::new(BIN_PREV);
    if bin_path.exists() {
        let _ = fs::remove_file(prev_path);
        fs::rename(bin_path, prev_path).context("moving current binary to .prev")?;
    }
    Ok(())
}

/// Atomically promote a staged binary into `BIN_PATH`.
fn install_staged_binary(staged: &Path) -> Result<()> {
    fs::rename(staged, BIN_PATH).context("moving new binary into place")
}

fn remove_prev_binary() {
    let _ = fs::remove_file(BIN_PREV);
}

/// Restore `somfy.prev` over the live binary when a rollback is needed.
fn restore_prev_binary() -> Result<()> {
    let bin_path = PathBuf::from(BIN_PATH);
    let prev_path = PathBuf::from(BIN_PREV);
    if prev_path.exists() {
        let _ = fs::remove_file(&bin_path);
        fs::rename(&prev_path, &bin_path).context("restoring previous binary")?;
    }
    Ok(())
}

/// Promote `staged` to `BIN_PATH`, optionally restart, then drop `somfy.prev` on success.
pub async fn apply_binary_swap<F, Fut>(
    staged: &Path,
    service_state: ServiceState,
    refresh_unit: impl FnOnce() -> Result<()>,
    post_start_validate: F,
    wait_timeout: std::time::Duration,
) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    archive_live_binary()?;

    if service_state.was_running() {
        stop_somfy_best_effort();
    }

    install_staged_binary(staged)?;
    refresh_unit()?;

    if !service_state.was_running() {
        remove_prev_binary();
        return Ok(());
    }

    start_somfy()?;
    wait_somfy_active(wait_timeout)
        .await
        .context("service did not become active")?;
    post_start_validate().await?;
    remove_prev_binary();
    Ok(())
}

pub fn rollback_binary_swap(service_state: ServiceState) -> Result<()> {
    if service_state.was_running() {
        stop_somfy_best_effort();
    }
    restore_prev_binary()?;
    if service_state.was_running() {
        start_somfy()?;
    }
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
    fn atomic_write_if_changed_skips_identical_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("unit");
        atomic_write(&target, "same\n").unwrap();
        assert!(!atomic_write_if_changed(&target, "same\n").unwrap());
        assert_eq!(fs::read_to_string(&target).unwrap(), "same\n");
        assert!(atomic_write_if_changed(&target, "new\n").unwrap());
        assert_eq!(fs::read_to_string(&target).unwrap(), "new\n");
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
