use anyhow::{bail, Result};
use std::process::Command;

use crate::cli::LogsArgs;

pub fn run(args: LogsArgs) -> Result<()> {
    let mut cmd = Command::new("journalctl");
    cmd.args(["-u", "somfy", "-o", "short-iso"]);
    if args.follow || args.debug {
        cmd.arg("-f");
    }
    if args.debug {
        cmd.args(["-p", "debug"]);
    }
    let status = cmd.status()?;
    if !status.success() {
        bail!("journalctl exited with {status}");
    }
    Ok(())
}
