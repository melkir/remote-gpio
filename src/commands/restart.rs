use anyhow::Result;

use crate::systemd;

pub fn run() -> Result<()> {
    systemd::systemctl(&["restart", "somfy"])?;
    println!("somfy restarted");
    Ok(())
}
