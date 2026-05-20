use anyhow::Result;
use std::fs;
use std::path::Path;

use crate::commands::install::POLKIT_RULE_PATH;
use crate::deploy::{require_root, UNIT_PATH};
use crate::systemd;

pub async fn run() -> Result<()> {
    require_root("somfy uninstall")?;

    // Best-effort: disable --now is a no-op if the unit isn't loaded.
    let _ = systemd::systemctl(&["disable", "--now", "somfy"]);

    let unit = Path::new(UNIT_PATH);
    if unit.exists() {
        fs::remove_file(unit)?;
    }

    let polkit_rule = Path::new(POLKIT_RULE_PATH);
    if polkit_rule.exists() {
        fs::remove_file(polkit_rule)?;
    }

    systemd::systemctl(&["daemon-reload"])?;
    println!("somfy uninstalled");
    Ok(())
}
