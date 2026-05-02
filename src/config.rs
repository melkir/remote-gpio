use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::driver::{DriverConfig, DriverKind, RtsOptions, TelisOptions};
use crate::gpio::MAX_BCM_GPIO;

pub const SYSTEM_CONFIG_PATH: &str = "/etc/somfy/config.toml";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub driver: DriverKind,
    pub homekit: bool,
    pub rts: RtsOptions,
    pub telis: TelisOptions,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            driver: DriverKind::default_for_target(),
            homekit: false,
            rts: RtsOptions::default(),
            telis: TelisOptions::default(),
        }
    }
}

impl AppConfig {
    pub fn driver_config(&self) -> DriverConfig {
        DriverConfig {
            kind: self.driver,
            rts: self.rts.clone(),
            telis: self.telis.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub path: PathBuf,
    pub config: AppConfig,
    pub file_present: bool,
}

pub fn default_path() -> PathBuf {
    PathBuf::from(SYSTEM_CONFIG_PATH)
}

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

pub fn load_or_default(path: &Path) -> Result<AppConfig> {
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn to_toml(config: &AppConfig) -> Result<String> {
    toml::to_string_pretty(config).context("serializing resolved config")
}

pub fn validate(config: &AppConfig) -> Result<()> {
    if config.rts.gdo0_gpio > MAX_BCM_GPIO {
        bail!("rts.gdo0_gpio must be a BCM GPIO in 0..={MAX_BCM_GPIO}");
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
    if let Some(gpio) = config.telis.gpio.prog {
        if gpio > MAX_BCM_GPIO {
            bail!("telis.gpio.prog must be a BCM GPIO in 0..={MAX_BCM_GPIO}");
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
    fn parses_homekit_flag() {
        let config: AppConfig = toml::from_str(
            r#"
driver = "telis"
homekit = false
"#,
        )
        .unwrap();
        assert!(!config.homekit);
    }
}
