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

#[derive(Clone, Debug, Default)]
pub struct PositionState {
    current: HashMap<u64, u8>,
    target: HashMap<u64, u8>,
    status: HashMap<u64, u8>,
}

#[derive(Debug)]
pub struct PositionCache {
    state: Mutex<PositionState>,
    persist: bool,
}

impl PositionCache {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(PositionState {
                current: load_positions(),
                target: HashMap::new(),
                status: HashMap::new(),
            }),
            persist: true,
        }
    }

    #[cfg(test)]
    pub fn from_positions(positions: HashMap<u64, u8>) -> Self {
        Self {
            state: Mutex::new(PositionState {
                current: positions,
                target: HashMap::new(),
                status: HashMap::new(),
            }),
            persist: false,
        }
    }

    pub async fn snapshot(&self) -> Vec<BlindPosition> {
        let state = self.state.lock().await;
        BLINDS
            .iter()
            .map(|b| BlindPosition {
                aid: b.aid,
                current: effective_current_position(&state, b.aid),
                target: effective_target_position(&state, b.aid),
                status: effective_status(&state, b.aid),
            })
            .collect()
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
        let new_pos = position.min(100);
        if state.current.get(&blind.aid).copied() == Some(new_pos)
            && effective_target_position(&state, blind.aid) == new_pos
        {
            return Vec::new();
        }
        state.current.insert(blind.aid, new_pos);
        state.target.insert(blind.aid, new_pos);
        state.status.insert(blind.aid, STATUS_STOPPED);
        self.finish_current_update(&[(blind.aid, new_pos)], &state.current)
    }

    pub async fn apply_all_current(&self, position: u8) -> Vec<PositionDelta> {
        let mut state = self.state.lock().await;
        let new_pos = position.min(100);
        let mut changes = Vec::new();
        for blind in BLINDS {
            if state.current.get(&blind.aid).copied() != Some(new_pos)
                || effective_target_position(&state, blind.aid) != new_pos
            {
                state.current.insert(blind.aid, new_pos);
                state.target.insert(blind.aid, new_pos);
                state.status.insert(blind.aid, STATUS_STOPPED);
                changes.push((blind.aid, new_pos));
            }
        }
        if changes.is_empty() {
            return Vec::new();
        }
        self.finish_current_update(&changes, &state.current)
    }

    pub async fn apply_target(&self, blind: &Blind, target: u8, status: u8) -> Vec<PositionDelta> {
        let mut state = self.state.lock().await;
        let target = target.min(100);
        if effective_target_position(&state, blind.aid) == target {
            return Vec::new();
        }
        state.target.insert(blind.aid, target);
        state.status.insert(blind.aid, status);
        target_events(blind.aid, target, status)
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
        let mut deltas = Vec::new();

        for aid in aids_for_channel(channel) {
            let current = effective_current_position(&state, aid);
            if effective_target_position(&state, aid) == current
                && effective_status(&state, aid) == STATUS_STOPPED
            {
                continue;
            }

            state.target.insert(aid, current);
            state.status.insert(aid, STATUS_STOPPED);
            deltas.push(PositionDelta {
                aid,
                current: None,
                target: Some(current),
                status: Some(STATUS_STOPPED),
            });
        }

        deltas
    }

    fn finish_current_update(
        &self,
        changes: &[(u64, u8)],
        positions: &HashMap<u64, u8>,
    ) -> Vec<PositionDelta> {
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
            if v <= 100 {
                Some((aid, v))
            } else {
                None
            }
        })
        .collect()
}

fn save_positions_to(path: &Path, positions: &HashMap<u64, u8>) -> Result<()> {
    let stringified: BTreeMap<String, u8> =
        positions.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let bytes = serde_json::to_vec_pretty(&stringified)?;
    atomic_save_bytes(path, &bytes, false)
}

pub fn effective_current_position(state: &PositionState, aid: u64) -> u8 {
    let Some(blind) = find_blind(aid) else {
        return 100;
    };
    state.current.get(&blind.aid).copied().unwrap_or(100)
}

pub fn effective_target_position(state: &PositionState, aid: u64) -> u8 {
    let Some(blind) = find_blind(aid) else {
        return 100;
    };
    state
        .target
        .get(&blind.aid)
        .copied()
        .unwrap_or_else(|| effective_current_position(state, aid))
}

pub fn effective_status(state: &PositionState, aid: u64) -> u8 {
    let Some(blind) = find_blind(aid) else {
        return STATUS_STOPPED;
    };
    state
        .status
        .get(&blind.aid)
        .copied()
        .unwrap_or(STATUS_STOPPED)
}

pub fn position_events(aid: u64, position: u8) -> Vec<PositionDelta> {
    vec![PositionDelta {
        aid,
        current: Some(position),
        target: Some(position),
        status: Some(STATUS_STOPPED),
    }]
}

pub fn target_events(aid: u64, target: u8, status: u8) -> Vec<PositionDelta> {
    vec![PositionDelta {
        aid,
        current: None,
        target: Some(target),
        status: Some(status),
    }]
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

    #[test]
    fn external_position_broadcast_produces_position_delta() {
        let events = position_events(2, 0);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].current, Some(0));
        assert_eq!(events[0].target, Some(0));
        assert_eq!(events[0].status, Some(STATUS_STOPPED));
    }

    #[test]
    fn positions_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(POSITIONS_FILE);
        let mut original = HashMap::new();
        original.insert(2u64, 0u8);
        original.insert(3u64, 100u8);
        original.insert(4u64, 37u8);
        original.insert(6u64, 0u8);
        save_positions_to(&path, &original).unwrap();
        let loaded = load_positions_from(&path);
        assert_eq!(loaded, original);
    }

    #[test]
    fn positions_file_saves_in_stable_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(POSITIONS_FILE);
        let positions = HashMap::from([(4, 37), (6, 50), (2, 0), (3, 101), (5, 100)]);

        save_positions_to(&path, &positions).unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{\n  \"2\": 0,\n  \"3\": 101,\n  \"4\": 37,\n  \"5\": 100,\n  \"6\": 50\n}"
        );
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
