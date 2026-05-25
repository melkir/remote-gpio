use anyhow::{bail, Result};

use crate::config::DriverKind;
use crate::config::{self, ResolvedConfig};
use crate::core::Channel;
use crate::deploy::{atomic_write, prepare_driver_prereqs, restart_somfy};

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

    prepare_driver_prereqs(kind)?;

    atomic_write(&resolved.path, &config::to_toml(&next)?)?;
    println!("wrote {} (driver={kind})", resolved.path.display());

    restart_somfy()?;
    println!("somfy restarted");
    Ok(())
}

pub fn set_positioning(
    resolved: &ResolvedConfig,
    channel: Channel,
    open_seconds: f64,
    close_seconds: f64,
) -> Result<()> {
    if matches!(channel, Channel::All) {
        bail!("positioning timing must target one blind: L1, L2, L3, or L4");
    }

    let open_ms = seconds_to_ms("open", open_seconds)?;
    let close_ms = seconds_to_ms("close", close_seconds)?;

    let mut next = resolved.config.clone();
    let timing = match channel {
        Channel::L1 => &mut next.positioning.l1,
        Channel::L2 => &mut next.positioning.l2,
        Channel::L3 => &mut next.positioning.l3,
        Channel::L4 => &mut next.positioning.l4,
        Channel::All => unreachable!("ALL rejected above"),
    };
    timing.open_ms = open_ms;
    timing.close_ms = close_ms;
    config::validate(&next)?;

    atomic_write(&resolved.path, &config::to_toml(&next)?)?;
    println!(
        "wrote {} ({channel}: open_ms={open_ms}, close_ms={close_ms})",
        resolved.path.display()
    );

    restart_somfy()?;
    println!("somfy restarted");
    Ok(())
}

fn seconds_to_ms(name: &str, seconds: f64) -> Result<u64> {
    if !seconds.is_finite() || seconds <= 0.0 {
        bail!("{name} seconds must be greater than 0");
    }
    let millis = seconds * 1000.0;
    if millis > u64::MAX as f64 {
        bail!("{name} seconds is too large");
    }
    Ok(millis.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_to_ms_rounds_to_nearest_millisecond() {
        assert_eq!(seconds_to_ms("open", 27.1).unwrap(), 27_100);
        assert_eq!(seconds_to_ms("close", 25.43).unwrap(), 25_430);
    }

    #[test]
    fn seconds_to_ms_rejects_invalid_values() {
        assert!(seconds_to_ms("open", 0.0).is_err());
        assert!(seconds_to_ms("open", f64::NAN).is_err());
    }
}
