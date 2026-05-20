use anyhow::Result;

use crate::deploy::restart_somfy;

pub fn run() -> Result<()> {
    restart_somfy()?;
    println!("somfy restarted");
    Ok(())
}
