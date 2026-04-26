use anyhow::{bail, Result};

use crate::systemd;

pub fn run() -> Result<()> {
    if !nix::unistd::Uid::current().is_root() {
        bail!("somfy restart must be run as root (use sudo)");
    }

    systemd::systemctl(&["restart", "somfy"])?;
    println!("somfy restarted");
    Ok(())
}
