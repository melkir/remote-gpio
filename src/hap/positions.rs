//! Persisted position cache. Sibling file to `hap.json` so the in-memory
//! the Somfy HAP adapter's position cache survives restarts and the dedupe stays
//! effective across process boundaries.
//!
//! Reload is read-only: we never replay a saved position to GPIO.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::hap::state::{atomic_save_bytes, state_dir};

const POSITIONS_FILE: &str = "positions.json";

pub fn load() -> HashMap<u64, u8> {
    load_from(&state_dir().join(POSITIONS_FILE))
}

pub fn save(positions: &HashMap<u64, u8>) -> Result<()> {
    let dir = state_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating state directory {}", dir.display()))?;
    save_to(&dir.join(POSITIONS_FILE), positions)
}

fn load_from(path: &Path) -> HashMap<u64, u8> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return HashMap::new(),
    };
    let raw: HashMap<String, u8> = match serde_json::from_str(&text) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("ignoring malformed {}: {}", path.display(), e);
            return HashMap::new();
        }
    };
    raw.into_iter()
        .filter_map(|(k, v)| {
            let aid = k.parse::<u64>().ok()?;
            // We only ever persist {0, 100}; clamp anything else so a
            // hand-edited or corrupt file can't push out-of-range values to
            // controllers.
            let snapped = if v >= 50 { 100 } else { 0 };
            Some((aid, snapped))
        })
        .collect()
}

fn save_to(path: &Path, positions: &HashMap<u64, u8>) -> Result<()> {
    let stringified: HashMap<String, u8> =
        positions.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let bytes = serde_json::to_vec_pretty(&stringified)?;
    atomic_save_bytes(path, &bytes, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(POSITIONS_FILE);
        let mut original = HashMap::new();
        original.insert(2u64, 0u8);
        original.insert(3u64, 100u8);
        original.insert(6u64, 0u8);
        save_to(&path, &original).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded, original);
    }

    #[test]
    fn missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(POSITIONS_FILE);
        let loaded = load_from(&path);
        assert!(loaded.is_empty());
    }
}
