//! TOML configuration loaded from `/etc/somfy/config.toml` (or `--config`).

use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::gpio::{GpioOptions, MAX_BCM_GPIO};

/// Default system configuration path on the Pi.
const SYSTEM_CONFIG_PATH: &str = "/etc/somfy/config.toml";

/// Runtime-selectable blind driver implementation.
#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DriverKind {
    Fake,
    Telis,
    Rts,
}

impl DriverKind {
    pub(crate) fn default_for_target() -> Self {
        if cfg!(all(
            target_os = "linux",
            any(target_arch = "arm", target_arch = "aarch64")
        )) {
            Self::Telis
        } else {
            Self::Fake
        }
    }

    /// Whether `prog` / `prog --long` can be transmitted (RTS RF pairing).
    pub(crate) fn supports_pairing(self) -> bool {
        !matches!(self, Self::Telis)
    }
}

impl fmt::Display for DriverKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fake => write!(f, "fake"),
            Self::Telis => write!(f, "telis"),
            Self::Rts => write!(f, "rts"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RtsOptions {
    pub spi_device: String,
    pub gpio: RtsGpioOptions,
}

impl Default for RtsOptions {
    fn default() -> Self {
        Self {
            spi_device: "/dev/spidev0.0".to_string(),
            gpio: RtsGpioOptions::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RtsGpioOptions {
    pub gdo0: u8,
}

impl Default for RtsGpioOptions {
    fn default() -> Self {
        Self { gdo0: 18 }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TelisOptions {
    pub gpio: TelisGpioOptions,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TelisGpioOptions {
    pub up: u8,
    pub stop: u8,
    pub down: u8,
    pub select: u8,
    pub led1: u8,
    pub led2: u8,
    pub led3: u8,
    pub led4: u8,
}

impl Default for TelisGpioOptions {
    fn default() -> Self {
        Self {
            up: 26,
            stop: 19,
            down: 13,
            select: 6,
            led1: 21,
            led2: 20,
            led3: 16,
            led4: 12,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct BlindTimingOptions {
    pub open_ms: u64,
    pub close_ms: u64,
}

impl Default for BlindTimingOptions {
    fn default() -> Self {
        Self {
            open_ms: 10_000,
            close_ms: 10_000,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PositioningOptions {
    pub l1: BlindTimingOptions,
    pub l2: BlindTimingOptions,
    pub l3: BlindTimingOptions,
    pub l4: BlindTimingOptions,
}

/// Resolved driver settings passed to the driver router.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DriverConfig {
    pub kind: DriverKind,
    pub gpio: GpioOptions,
    pub rts: RtsOptions,
    pub telis: TelisOptions,
}

#[cfg(test)]
impl DriverConfig {
    pub(crate) fn fake() -> Self {
        Self {
            kind: DriverKind::Fake,
            gpio: GpioOptions::default(),
            rts: RtsOptions::default(),
            telis: TelisOptions::default(),
        }
    }
}

/// Top-level application configuration.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub driver: DriverKind,
    pub homekit: bool,
    pub positioning: PositioningOptions,
    pub gpio: GpioOptions,
    pub rts: RtsOptions,
    pub telis: TelisOptions,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            driver: DriverKind::default_for_target(),
            homekit: false,
            positioning: PositioningOptions::default(),
            gpio: GpioOptions::default(),
            rts: RtsOptions::default(),
            telis: TelisOptions::default(),
        }
    }
}

impl AppConfig {
    /// Build the driver configuration snapshot used at startup.
    pub(crate) fn driver_config(&self) -> DriverConfig {
        DriverConfig {
            kind: self.driver,
            gpio: self.gpio.clone(),
            rts: self.rts.clone(),
            telis: self.telis.clone(),
        }
    }
}

/// Configuration file path plus parsed, validated settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub path: PathBuf,
    pub config: AppConfig,
    pub file_present: bool,
}

fn default_path() -> PathBuf {
    PathBuf::from(SYSTEM_CONFIG_PATH)
}

/// Load and validate configuration from `path`, or the system default.
pub fn resolve(path: Option<PathBuf>) -> Result<ResolvedConfig> {
    let path = path.unwrap_or_else(default_path);
    let config = load_or_default(&path)?;
    validate(&config)?;
    Ok(ResolvedConfig {
        file_present: path.exists(),
        path,
        config,
    })
}

fn load_or_default(path: &Path) -> Result<AppConfig> {
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub(crate) fn to_toml(config: &AppConfig) -> Result<String> {
    toml::to_string_pretty(config).context("serializing resolved config")
}

pub(crate) fn validate(config: &AppConfig) -> Result<()> {
    if config.rts.gpio.gdo0 > MAX_BCM_GPIO {
        bail!("rts.gpio.gdo0 must be a BCM GPIO in 0..={MAX_BCM_GPIO}");
    }
    for (name, gpio) in [
        ("telis.gpio.up", config.telis.gpio.up),
        ("telis.gpio.stop", config.telis.gpio.stop),
        ("telis.gpio.down", config.telis.gpio.down),
        ("telis.gpio.select", config.telis.gpio.select),
        ("telis.gpio.led1", config.telis.gpio.led1),
        ("telis.gpio.led2", config.telis.gpio.led2),
        ("telis.gpio.led3", config.telis.gpio.led3),
        ("telis.gpio.led4", config.telis.gpio.led4),
    ] {
        if gpio > MAX_BCM_GPIO {
            bail!("{name} must be a BCM GPIO in 0..={MAX_BCM_GPIO}");
        }
    }
    for (name, timing) in [
        ("positioning.l1", &config.positioning.l1),
        ("positioning.l2", &config.positioning.l2),
        ("positioning.l3", &config.positioning.l3),
        ("positioning.l4", &config.positioning.l4),
    ] {
        if timing.open_ms == 0 {
            bail!("{name}.open_ms must be greater than 0");
        }
        if timing.close_ms == 0 {
            bail!("{name}.close_ms must be greater than 0");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_homekit_defaults_disabled() {
        let config: AppConfig = toml::from_str("driver = \"fake\"\n").unwrap();
        assert!(!config.homekit);
    }

    #[test]
    fn parses_per_blind_positioning() {
        let config: AppConfig = toml::from_str(
            r#"
driver = "fake"

[positioning.l1]
open_ms = 11000
close_ms = 12000

[positioning.l4]
open_ms = 41000
close_ms = 42000
"#,
        )
        .unwrap();

        assert_eq!(config.positioning.l1.open_ms, 11_000);
        assert_eq!(config.positioning.l1.close_ms, 12_000);
        assert_eq!(config.positioning.l2.open_ms, 10_000);
        assert_eq!(config.positioning.l4.open_ms, 41_000);
        assert_eq!(config.positioning.l4.close_ms, 42_000);
    }

    #[test]
    fn rejects_zero_positioning_timing() {
        let config: AppConfig = toml::from_str(
            r#"
driver = "fake"

[positioning.l2]
open_ms = 0
"#,
        )
        .unwrap();

        let err = validate(&config).unwrap_err();
        assert!(err.to_string().contains("positioning.l2.open_ms"));
    }

    #[test]
    fn resolve_accepts_minimal_fake_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "driver = \"fake\"\n").unwrap();

        let resolved = resolve(Some(path)).unwrap();
        assert_eq!(resolved.config.driver, DriverKind::Fake);
    }

    #[test]
    fn resolve_rejects_unknown_driver() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "driver = \"nope\"\n").unwrap();

        let err = resolve(Some(path)).unwrap_err();
        assert!(
            err.to_string().contains("parsing"),
            "expected TOML parse failure: {err}"
        );
    }

    #[test]
    fn parses_nested_rts_gpio_and_global_gpio_chip() {
        let config: AppConfig = toml::from_str(
            r#"
driver = "rts"

[gpio]
chip = "/dev/gpiochip1"

[rts.gpio]
gdo0 = 24
"#,
        )
        .unwrap();

        assert_eq!(config.gpio.chip, "/dev/gpiochip1");
        assert_eq!(config.rts.gpio.gdo0, 24);
    }
}
