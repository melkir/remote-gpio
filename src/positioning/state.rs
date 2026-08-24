//! In-memory blind positions, `positions.json` persistence, and position event diffs.
//!
//! Reload is read-only: we never replay a saved position to GPIO.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
use tokio::sync::Mutex;

use crate::core::Channel;
use crate::persist::{self, atomic_save_bytes};

const POSITIONS_FILE: &str = "positions.json";

pub const STATUS_DECREASING: u8 = 0;
pub const STATUS_INCREASING: u8 = 1;
pub const STATUS_STOPPED: u8 = 2;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
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

pub fn find_blind_for_channel(channel: Channel) -> Option<&'static Blind> {
    BLINDS.iter().find(|b| b.channel == channel)
}

pub fn aids_for_channel(channel: Channel) -> Vec<u64> {
    match channel {
        Channel::All => BLINDS.iter().map(|blind| blind.aid).collect(),
        _ => find_blind_for_channel(channel)
            .map(|blind| vec![blind.aid])
            .unwrap_or_default(),
    }
}

pub fn target_positions(channel: Channel, position: u8) -> Vec<(u64, u8)> {
    aids_for_channel(channel)
        .into_iter()
        .map(|aid| (aid, position))
        .collect()
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BlindPosition {
    pub aid: u64,
    pub current: u8,
    pub target: u8,
    pub status: u8,
}

impl BlindPosition {
    /// Default estimated state for an unknown or missing accessory.
    pub fn default_for_aid(aid: u64) -> Self {
        Self {
            aid,
            current: 100,
            target: 100,
            status: STATUS_STOPPED,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PositionDelta {
    pub aid: u64,
    pub current: Option<u8>,
    pub target: Option<u8>,
    pub status: Option<u8>,
}

impl PositionDelta {
    /// A blind came to rest at a known position: current, target, and status all move.
    pub fn settled(aid: u64, position: u8) -> Self {
        Self {
            aid,
            current: Some(position),
            target: Some(position),
            status: Some(STATUS_STOPPED),
        }
    }

    /// A blind started moving toward `target`; current is still being estimated.
    pub fn retargeted(aid: u64, target: u8, status: u8) -> Self {
        Self {
            aid,
            current: None,
            target: Some(target),
            status: Some(status),
        }
    }
}

/// Estimated state of every blind, in [`BLINDS`] order.
///
/// The accessory set is fixed at compile time, so this is a flat array rather
/// than a map: every lookup is one scan of four `Copy` structs.
#[derive(Clone, Debug)]
pub struct PositionState {
    blinds: [BlindPosition; BLINDS.len()],
}

impl PositionState {
    /// Seed from `positions.json`. Unsaved blinds start fully open, and every
    /// blind starts stationary at its own current position.
    fn from_saved(saved: &HashMap<u64, u8>) -> Self {
        let mut blinds = [BlindPosition::default_for_aid(0); BLINDS.len()];
        for (slot, blind) in blinds.iter_mut().zip(BLINDS) {
            let current = saved.get(&blind.aid).copied().unwrap_or(100).min(100);
            *slot = BlindPosition {
                aid: blind.aid,
                current,
                target: current,
                status: STATUS_STOPPED,
            };
        }
        Self { blinds }
    }

    fn get_mut(&mut self, aid: u64) -> Option<&mut BlindPosition> {
        self.blinds.iter_mut().find(|position| position.aid == aid)
    }

    /// Snap a blind to a resting position, returning the delta if it moved.
    fn settle(&mut self, aid: u64, position: u8) -> Option<PositionDelta> {
        let position = position.min(100);
        let blind = self.get_mut(aid)?;
        if blind.current == position && blind.target == position {
            return None;
        }
        blind.current = position;
        blind.target = position;
        blind.status = STATUS_STOPPED;
        Some(PositionDelta::settled(aid, position))
    }
}

#[derive(Debug)]
pub struct PositionCache {
    state: Mutex<PositionState>,
    persist: bool,
}

impl PositionCache {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(PositionState::from_saved(&load_positions())),
            persist: true,
        }
    }

    #[cfg(test)]
    pub fn from_positions(positions: HashMap<u64, u8>) -> Self {
        Self {
            state: Mutex::new(PositionState::from_saved(&positions)),
            persist: false,
        }
    }

    pub async fn snapshot(&self) -> Vec<BlindPosition> {
        self.state.lock().await.blinds.to_vec()
    }

    pub async fn apply_for_channel(&self, channel: Channel, pos: u8) -> Vec<PositionDelta> {
        if matches!(channel, Channel::All) {
            return self.apply_all_current(pos).await;
        }
        let Some(blind) = find_blind_for_channel(channel) else {
            return Vec::new();
        };
        self.apply_blind_current(blind, pos).await
    }

    pub async fn apply_blind_current(&self, blind: &Blind, position: u8) -> Vec<PositionDelta> {
        let mut state = self.state.lock().await;
        let Some(delta) = state.settle(blind.aid, position) else {
            return Vec::new();
        };
        self.persist_positions(&state);
        vec![delta]
    }

    pub async fn apply_all_current(&self, position: u8) -> Vec<PositionDelta> {
        let mut state = self.state.lock().await;
        let deltas: Vec<PositionDelta> = BLINDS
            .iter()
            .filter_map(|blind| state.settle(blind.aid, position))
            .collect();
        if deltas.is_empty() {
            return Vec::new();
        }
        self.persist_positions(&state);
        deltas
    }

    pub async fn apply_target(&self, blind: &Blind, target: u8, status: u8) -> Vec<PositionDelta> {
        let mut state = self.state.lock().await;
        let target = target.min(100);
        let Some(position) = state.get_mut(blind.aid) else {
            return Vec::new();
        };
        if position.target == target {
            return Vec::new();
        }
        position.target = target;
        position.status = status;
        vec![PositionDelta::retargeted(blind.aid, target, status)]
    }

    /// Mark a manually stopped channel as stationary at its last known position.
    ///
    /// Position estimation only advances when a timed motion completes, so an
    /// early stop cannot infer a more precise intermediate position. Resetting
    /// the target to the last known current value keeps the state internally
    /// consistent and prevents HomeKit from reporting a movement that is no
    /// longer running.
    pub async fn stop_channel(&self, channel: Channel) -> Vec<PositionDelta> {
        let mut state = self.state.lock().await;
        aids_for_channel(channel)
            .into_iter()
            .filter_map(|aid| {
                let position = state.get_mut(aid)?;
                if position.target == position.current && position.status == STATUS_STOPPED {
                    return None;
                }
                position.target = position.current;
                position.status = STATUS_STOPPED;
                Some(PositionDelta::retargeted(
                    aid,
                    position.current,
                    STATUS_STOPPED,
                ))
            })
            .collect()
    }

    fn persist_positions(&self, state: &PositionState) {
        if !self.persist {
            return;
        }
        if let Err(e) = save_positions(&state.blinds) {
            tracing::warn!("failed to persist positions: {e}");
        }
    }
}

fn load_positions() -> HashMap<u64, u8> {
    load_positions_from(&persist::state_dir().join(POSITIONS_FILE))
}

fn save_positions(positions: &[BlindPosition]) -> Result<()> {
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
            if v <= 100 {
                Some((aid, v))
            } else {
                None
            }
        })
        .collect()
}

/// Persist only the estimated current positions, keyed by aid.
fn save_positions_to(path: &Path, positions: &[BlindPosition]) -> Result<()> {
    let stringified: BTreeMap<String, u8> = positions
        .iter()
        .map(|position| (position.aid.to_string(), position.current))
        .collect();
    let bytes = serde_json::to_vec_pretty(&stringified)?;
    atomic_save_bytes(path, &bytes, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn snapshot_reports_current_and_target() {
        let mut positions = HashMap::new();
        positions.insert(2, 25);

        let cache = PositionCache::from_positions(positions);

        let snapshot = cache.snapshot().await;
        let blind = snapshot.iter().find(|p| p.aid == 2).unwrap();
        assert_eq!(blind.current, 25);
        assert_eq!(blind.target, 25);
        assert_eq!(blind.status, STATUS_STOPPED);
    }

    #[tokio::test]
    async fn stop_channel_resets_pending_target_to_last_known_position() {
        let cache = PositionCache::from_positions(HashMap::from([(2, 75)]));
        cache.apply_target(&BLINDS[0], 25, STATUS_DECREASING).await;

        let deltas = cache.stop_channel(Channel::L1).await;

        assert_eq!(
            deltas,
            vec![PositionDelta {
                aid: 2,
                current: None,
                target: Some(75),
                status: Some(STATUS_STOPPED),
            }]
        );
        assert_eq!(
            cache.snapshot().await[0],
            BlindPosition {
                aid: 2,
                current: 75,
                target: 75,
                status: STATUS_STOPPED,
            }
        );
    }

    /// Only `aid` and `current` reach the file; target/status are not persisted.
    fn saved(entries: &[(u64, u8)]) -> Vec<BlindPosition> {
        entries
            .iter()
            .map(|(aid, current)| BlindPosition {
                aid: *aid,
                current: *current,
                target: 100,
                status: STATUS_INCREASING,
            })
            .collect()
    }

    #[test]
    fn external_position_broadcast_produces_position_delta() {
        let delta = PositionDelta::settled(2, 0);

        assert_eq!(delta.current, Some(0));
        assert_eq!(delta.target, Some(0));
        assert_eq!(delta.status, Some(STATUS_STOPPED));
    }

    #[test]
    fn positions_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(POSITIONS_FILE);
        let original = saved(&[(2, 0), (3, 100), (4, 37), (6, 0)]);

        save_positions_to(&path, &original).unwrap();

        let loaded = load_positions_from(&path);
        assert_eq!(
            loaded,
            HashMap::from([(2u64, 0u8), (3, 100), (4, 37), (6, 0)])
        );
    }

    #[test]
    fn positions_file_saves_in_stable_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(POSITIONS_FILE);
        let positions = saved(&[(4, 37), (6, 50), (2, 0), (3, 101), (5, 100)]);

        save_positions_to(&path, &positions).unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{\n  \"2\": 0,\n  \"3\": 101,\n  \"4\": 37,\n  \"5\": 100,\n  \"6\": 50\n}"
        );
    }

    #[test]
    fn out_of_range_saved_position_is_ignored_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(POSITIONS_FILE);
        save_positions_to(&path, &saved(&[(2, 101), (3, 40)])).unwrap();

        let state = PositionState::from_saved(&load_positions_from(&path));

        assert_eq!(state.blinds[0].current, 100);
        assert_eq!(state.blinds[1].current, 40);
    }

    #[test]
    fn missing_positions_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(POSITIONS_FILE);
        let loaded = load_positions_from(&path);
        assert!(loaded.is_empty());
    }

    #[test]
    fn aids_for_channel_maps_channel_and_all() {
        assert_eq!(aids_for_channel(Channel::L2), vec![3]);
        assert_eq!(aids_for_channel(Channel::All), vec![2, 3, 4, 5]);
    }

    #[test]
    fn target_positions_pairs_aids_with_position() {
        assert_eq!(target_positions(Channel::L2, 25), vec![(3, 25)]);
        assert_eq!(
            target_positions(Channel::All, 10),
            vec![(2, 10), (3, 10), (4, 10), (5, 10)]
        );
    }
}
