use std::path::PathBuf;

pub const HAP_PORT: u16 = 5010;
pub const MODEL: &str = "Somfy Telis 4";
/// Accessory Category Identifier advertised over mDNS and encoded into setup QR payloads.
pub const HAP_CATEGORY: &str = "2";
pub const MDNS_NAME_PREFIX: &str = "Somfy";
pub const SYSTEM_STATE_DIR: &str = "/var/lib/somfy";

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("STATE_DIRECTORY") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("SOMFY_STATE_DIR") {
        return PathBuf::from(dir);
    }
    default_state_dir()
}

#[cfg(debug_assertions)]
fn default_state_dir() -> PathBuf {
    PathBuf::from("./hap-state")
}

#[cfg(not(debug_assertions))]
fn default_state_dir() -> PathBuf {
    PathBuf::from(SYSTEM_STATE_DIR)
}
