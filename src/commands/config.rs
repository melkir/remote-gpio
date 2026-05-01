use anyhow::Result;

use crate::config::{self, ResolvedConfig};

pub fn path(resolved: &ResolvedConfig) {
    println!("{}", resolved.path.display());
}

pub fn show(resolved: &ResolvedConfig) -> Result<()> {
    print!("{}", config::to_toml(&resolved.config)?);
    Ok(())
}

pub fn validate(resolved: &ResolvedConfig) -> Result<()> {
    config::validate(&resolved.config)?;
    println!(
        "ok: {} ({})",
        resolved.path.display(),
        if resolved.file_present {
            "file"
        } else {
            "built-in defaults"
        }
    );
    Ok(())
}
