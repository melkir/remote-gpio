//! Host provisioning for the RTS driver (pigpiod loopback-only).

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;

use super::{atomic_write_if_changed, command_exists, require_root, run_command};
use crate::systemd;

const PIGPIOD_OVERRIDE_PATH: &str = "/etc/systemd/system/pigpiod.service.d/somfy-localhost.conf";
const PIGPIOD_OVERRIDE: &str = "[Service]\nExecStart=\nExecStart=/usr/bin/pigpiod -l\n";

pub(crate) fn prepare() -> Result<()> {
    if rts_prereqs_need_root() {
        require_root("RTS prerequisite setup")?;
    }
    ensure_pigpio_installed()?;
    configure_pigpiod_localhost()?;
    Ok(())
}

fn rts_prereqs_need_root() -> bool {
    !pigpiod_installed() || !pigpiod_override_in_sync()
}

fn pigpiod_installed() -> bool {
    command_exists("pigpiod") || Path::new("/usr/bin/pigpiod").is_file()
}

fn pigpiod_override_in_sync() -> bool {
    fs::read_to_string(PIGPIOD_OVERRIDE_PATH)
        .map(|existing| existing.trim() == PIGPIOD_OVERRIDE.trim())
        .unwrap_or(false)
}

fn ensure_pigpio_installed() -> Result<()> {
    if pigpiod_installed() {
        tracing::info!("pigpiod already installed");
        return Ok(());
    }

    if !command_exists("apt-get") {
        bail!("pigpiod is not installed and apt-get is unavailable; install the `pigpio` package before using the RTS driver");
    }

    run_command("apt-get", &["update"]).context("updating apt package metadata")?;
    run_command("apt-get", &["install", "-y", "pigpio"]).context("installing pigpio")?;
    Ok(())
}

fn pigpiod_override_paths() -> Result<(&'static Path, &'static Path)> {
    let override_path = Path::new(PIGPIOD_OVERRIDE_PATH);
    let override_dir = override_path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| anyhow::anyhow!("invalid pigpiod override path"))?;
    Ok((override_path, override_dir))
}

fn configure_pigpiod_localhost() -> Result<()> {
    let (override_path, override_dir) = pigpiod_override_paths()?;
    if !override_dir.exists() {
        fs::create_dir_all(override_dir)
            .with_context(|| format!("creating {}", override_dir.display()))?;
    }

    if atomic_write_if_changed(override_path, PIGPIOD_OVERRIDE)? {
        systemd::systemctl(&["daemon-reload"])?;
        tracing::info!("wrote {}", PIGPIOD_OVERRIDE_PATH);
    } else {
        tracing::info!("{} already in sync", PIGPIOD_OVERRIDE_PATH);
    }

    systemd::systemctl(&["enable", "--now", "pigpiod"])?;
    systemd::systemctl(&["restart", "pigpiod"])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pigpiod_override_paths_resolve_parent_directory() {
        let (path, dir) = pigpiod_override_paths().unwrap();
        assert_eq!(path, Path::new(PIGPIOD_OVERRIDE_PATH));
        assert_eq!(dir, Path::new("/etc/systemd/system/pigpiod.service.d"));
    }
}
