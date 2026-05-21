//! In-memory HomeKit blind positions, `positions.json` persistence, and HAP event diffs.
//!
//! Reload is read-only: we never replay a saved position to GPIO.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tokio::sync::Mutex;

use crate::core::Channel;
use crate::hap::runtime::{CharacteristicEvent, CharacteristicId};
use crate::homekit::accessory_db::{
    IID_CURRENT_POSITION, IID_POSITION_STATE, IID_TARGET_POSITION, POSITION_STATE_STOPPED,
};
use crate::persist::{self, atomic_save_bytes};
use serde_json::json;

const POSITIONS_FILE: &str = "positions.json";

#[derive(Copy, Clone, Debug)]
pub struct Blind {
    pub aid: u64,
    pub name: &'static str,
    pub channel: Channel,
    pub serial: &'static str,
}

pub const BLINDS: &[Blind] = &[
    Blind {
        aid: 2,
        name: "Blind 1",
        channel: Channel::L1,
        serial: "somfy-L1",
    },
    Blind {
        aid: 3,
        name: "Blind 2",
        channel: Channel::L2,
        serial: "somfy-L2",
    },
    Blind {
        aid: 4,
        name: "Blind 3",
        channel: Channel::L3,
        serial: "somfy-L3",
    },
    Blind {
        aid: 5,
        name: "Blind 4",
        channel: Channel::L4,
        serial: "somfy-L4",
    },
];

pub fn find_blind(aid: u64) -> Option<&'static Blind> {
    BLINDS.iter().find(|b| b.aid == aid)
}

/// HomeKit blind position after snapping a 0–100 request to an endpoint.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SnappedPosition {
    Closed = 0,
    Open = 100,
}

impl SnappedPosition {
    pub fn snap(value: u8) -> Self {
        if value >= 50 {
            Self::Open
        } else {
            Self::Closed
        }
    }

    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Closed => 0,
            Self::Open => 100,
        }
    }
}

pub struct PositionCache {
    positions: Mutex<HashMap<u64, u8>>,
    persist: bool,
}

impl PositionCache {
    pub fn new() -> Self {
        Self {
            positions: Mutex::new(load_positions()),
            persist: true,
        }
    }

    #[cfg(test)]
    pub fn from_positions(positions: HashMap<u64, u8>) -> Self {
        Self {
            positions: Mutex::new(positions),
            persist: false,
        }
    }

    pub async fn snapshot(&self) -> Vec<(u64, u8)> {
        let positions = self.positions.lock().await;
        BLINDS
            .iter()
            .map(|b| (b.aid, effective_position(&positions, b.aid)))
            .collect()
    }

    pub async fn with_positions<R>(&self, f: impl FnOnce(&HashMap<u64, u8>) -> R) -> R {
        let guard = self.positions.lock().await;
        f(&guard)
    }

    pub async fn all_at_target(&self, snapped: SnappedPosition) -> bool {
        self.with_positions(|positions| {
            BLINDS
                .iter()
                .all(|blind| positions.get(&blind.aid).copied() == Some(snapped.as_u8()))
        })
        .await
    }

    pub async fn apply_for_channel(&self, channel: Channel, pos: u8) -> Vec<CharacteristicEvent> {
        let snapped = SnappedPosition::snap(pos);
        if matches!(channel, Channel::ALL) {
            return self.apply_all(snapped).await;
        }
        let Some(blind) = BLINDS.iter().find(|b| b.channel == channel) else {
            return Vec::new();
        };
        self.apply_blind(blind, snapped).await
    }

    pub async fn apply_blind(
        &self,
        blind: &Blind,
        snapped: SnappedPosition,
    ) -> Vec<CharacteristicEvent> {
        let mut positions = self.positions.lock().await;
        let new_pos = snapped.as_u8();
        if positions.get(&blind.aid).copied() == Some(new_pos) {
            return Vec::new();
        }
        positions.insert(blind.aid, new_pos);
        self.finish_update(&[(blind.aid, new_pos)], &positions)
    }

    pub async fn apply_all(&self, snapped: SnappedPosition) -> Vec<CharacteristicEvent> {
        let mut positions = self.positions.lock().await;
        let new_pos = snapped.as_u8();
        let mut changes = Vec::new();
        for blind in BLINDS {
            if positions.get(&blind.aid).copied() != Some(new_pos) {
                positions.insert(blind.aid, new_pos);
                changes.push((blind.aid, new_pos));
            }
        }
        if changes.is_empty() {
            return Vec::new();
        }
        self.finish_update(&changes, &positions)
    }

    pub async fn get(&self, aid: u64) -> Option<u8> {
        self.positions.lock().await.get(&aid).copied()
    }

    fn finish_update(
        &self,
        changes: &[(u64, u8)],
        positions: &HashMap<u64, u8>,
    ) -> Vec<CharacteristicEvent> {
        if self.persist {
            if let Err(e) = save_positions(positions) {
                tracing::warn!("failed to persist positions: {e}");
            }
        }
        changes
            .iter()
            .flat_map(|(aid, pos)| position_events(*aid, *pos))
            .collect()
    }
}

fn load_positions() -> HashMap<u64, u8> {
    load_positions_from(&persist::state_dir().join(POSITIONS_FILE))
}

fn save_positions(positions: &HashMap<u64, u8>) -> Result<()> {
    let dir = persist::state_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating state directory {}", dir.display()))?;
    save_positions_to(&dir.join(POSITIONS_FILE), positions)
}

fn load_positions_from(path: &Path) -> HashMap<u64, u8> {
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
            let snapped = if v >= 50 { 100 } else { 0 };
            Some((aid, snapped))
        })
        .collect()
}

fn save_positions_to(path: &Path, positions: &HashMap<u64, u8>) -> Result<()> {
    let stringified: HashMap<String, u8> =
        positions.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let bytes = serde_json::to_vec_pretty(&stringified)?;
    atomic_save_bytes(path, &bytes, false)
}

pub fn effective_position(positions: &HashMap<u64, u8>, aid: u64) -> u8 {
    let Some(blind) = find_blind(aid) else {
        return 100;
    };
    positions.get(&blind.aid).copied().unwrap_or(100)
}

pub fn position_events(aid: u64, position: u8) -> Vec<CharacteristicEvent> {
    [
        (IID_CURRENT_POSITION, json!(position)),
        (IID_TARGET_POSITION, json!(position)),
        (IID_POSITION_STATE, json!(POSITION_STATE_STOPPED)),
    ]
    .into_iter()
    .map(|(iid, value)| CharacteristicEvent {
        id: CharacteristicId::new(aid, iid),
        value,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn all_at_target_checks_individual_blinds_only() {
        let mut positions = HashMap::new();
        positions.insert(2, 0);
        positions.insert(3, 0);
        positions.insert(4, 0);
        positions.insert(5, 0);

        let cache = PositionCache::from_positions(positions);

        assert!(cache.all_at_target(SnappedPosition::Closed).await);
        assert!(!cache.all_at_target(SnappedPosition::Open).await);
    }

    #[test]
    fn external_position_broadcast_produces_hap_events() {
        let events = position_events(2, 0);

        assert_eq!(events.len(), 3);
        assert!(events
            .iter()
            .any(|e| e.id == CharacteristicId::new(2, IID_CURRENT_POSITION)));
    }

    #[test]
    fn positions_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(POSITIONS_FILE);
        let mut original = HashMap::new();
        original.insert(2u64, 0u8);
        original.insert(3u64, 100u8);
        original.insert(6u64, 0u8);
        save_positions_to(&path, &original).unwrap();
        let loaded = load_positions_from(&path);
        assert_eq!(loaded, original);
    }

    #[test]
    fn missing_positions_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(POSITIONS_FILE);
        let loaded = load_positions_from(&path);
        assert!(loaded.is_empty());
    }
}
