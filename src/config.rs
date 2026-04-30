use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::backend::{BackendConfig, BackendKind, RtsOptions, TelisOptions};

pub const SYSTEM_CONFIG_PATH: &str = "/etc/somfy/config.toml";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub backend: BackendKind,
    pub rts: RtsOptions,
    pub telis: TelisOptions,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            backend: BackendKind::Fake,
            rts: RtsOptions::default(),
            telis: TelisOptions::default(),
        }
    }
}

impl AppConfig {
    pub fn backend_config(&self) -> BackendConfig {
        BackendConfig {
            kind: self.backend,
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
    if config.rts.gdo0_gpio > 31 {
        bail!("rts.gdo0_gpio must be a BCM GPIO in 0..=31");
    }
    if config.rts.frame_count == 0 {
        bail!("rts.frame_count must be greater than zero");
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
        if gpio > 31 {
            bail!("{name} must be a BCM GPIO in 0..=31");
        }
    }
    if let Some(gpio) = config.telis.gpio.prog {
        if gpio > 31 {
            bail!("telis.gpio.prog must be a BCM GPIO in 0..=31");
        }
    }
    Ok(())
}
