use anyhow::{bail, Result};
use std::process::Command;

pub fn systemctl(args: &[&str]) -> Result<()> {
    let output = Command::new("systemctl").args(args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("systemctl {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

pub fn is_active(unit: &str) -> Result<String> {
    let output = Command::new("systemctl")
        .args(["is-active", unit])
        .output()?;
    // is-active returns non-zero for inactive/failed units, but stdout still
    // contains the state. We treat that as a value, not an error.
    let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(state)
}

