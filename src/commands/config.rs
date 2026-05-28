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
    slack_seconds: Option<f64>,
    no_restart: bool,
) -> Result<()> {
    if matches!(channel, Channel::All) {
        bail!("positioning timing must target one blind: L1, L2, L3, or L4");
    }

    let open_ms = seconds_to_positive_ms("open", open_seconds)?;
    let close_ms = seconds_to_positive_ms("close", close_seconds)?;
    let slack_ms = slack_seconds
        .map(|seconds| seconds_to_nonnegative_ms("slack", seconds))
        .transpose()?;

    let mut next = resolved.config.clone();
    let Some(timing) = next.positioning.timing_mut(channel) else {
        bail!("positioning timing must target one blind: L1, L2, L3, or L4");
    };
    timing.open_ms = open_ms;
    timing.close_ms = close_ms;
    if let Some(slack_ms) = slack_ms {
        timing.slack_ms = slack_ms;
    }
    config::validate(&next)?;

    let slack_message = slack_ms
        .map(|slack_ms| format!(", slack_ms={slack_ms}"))
        .unwrap_or_default();
    atomic_write(&resolved.path, &config::to_toml(&next)?)?;
    println!(
        "wrote {} ({channel}: open_ms={open_ms}, close_ms={close_ms}{slack_message})",
        resolved.path.display()
    );

    if no_restart {
        println!("somfy not restarted (--no-restart); restart once after the final config change");
    } else {
        restart_somfy()?;
        println!("somfy restarted");
    }
    Ok(())
}

fn seconds_to_positive_ms(name: &str, seconds: f64) -> Result<u64> {
    if seconds <= 0.0 {
        bail!("{name} seconds must be greater than 0");
    }
    seconds_to_nonnegative_ms(name, seconds)
}

fn seconds_to_nonnegative_ms(name: &str, seconds: f64) -> Result<u64> {
    if !seconds.is_finite() || seconds < 0.0 {
        bail!("{name} seconds must be 0 or greater");
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
    fn seconds_to_positive_ms_rounds_to_nearest_millisecond() {
        assert_eq!(seconds_to_positive_ms("open", 27.1).unwrap(), 27_100);
        assert_eq!(seconds_to_positive_ms("close", 25.43).unwrap(), 25_430);
    }

    #[test]
    fn seconds_to_positive_ms_rejects_invalid_values() {
        assert!(seconds_to_positive_ms("open", 0.0).is_err());
        assert!(seconds_to_positive_ms("open", -1.0).is_err());
        assert!(seconds_to_positive_ms("open", f64::NAN).is_err());
    }

    #[test]
    fn seconds_to_nonnegative_ms_allows_zero() {
        assert_eq!(seconds_to_nonnegative_ms("delay", 0.0).unwrap(), 0);
    }
}
