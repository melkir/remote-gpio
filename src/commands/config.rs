use anyhow::Result;

use crate::commands::install;
use crate::config::{self, ResolvedConfig};
use crate::driver::DriverKind;
use crate::systemd;

pub fn path(resolved: &ResolvedConfig) {
    println!("{}", resolved.path.display());
}

pub fn show(resolved: &ResolvedConfig) -> Result<()> {
    print!("{}", config::to_toml(&resolved.config)?);
    Ok(())
}

pub fn set_driver(resolved: &ResolvedConfig, kind: DriverKind) -> Result<()> {
    if resolved.config.driver == kind {
        println!("driver already set to {kind}");
        return Ok(());
    }

    let mut next = resolved.config.clone();
    next.driver = kind;
    config::validate(&next)?;

    install::atomic_write(&resolved.path, &config::to_toml(&next)?)?;
    println!("wrote {} (driver={kind})", resolved.path.display());

    if kind == DriverKind::Rts {
        install::prepare_rts_prereqs()?;
    }

    systemd::systemctl(&["restart", "somfy"])?;
    println!("somfy restarted");
    Ok(())
}
