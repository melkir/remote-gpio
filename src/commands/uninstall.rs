use anyhow::{bail, Result};
use std::fs;
use std::path::Path;

use crate::commands::doctor::UNIT_PATH;
use crate::systemd;

pub async fn run() -> Result<()> {
    if !nix::unistd::Uid::current().is_root() {
        bail!("somfy uninstall must be run as root (use sudo)");
    }

    // Best-effort: disable --now is a no-op if the unit isn't loaded.
    let _ = systemd::systemctl(&["disable", "--now", "somfy"]);

    let unit = Path::new(UNIT_PATH);
    if unit.exists() {
        fs::remove_file(unit)?;
    }

    systemd::systemctl(&["daemon-reload"])?;
    println!("somfy uninstalled");
    Ok(())
}
