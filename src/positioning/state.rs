//! In-memory blind positions, `positions.json` persistence, and position event diffs.
//!
//! Reload is read-only: we never replay a saved position to GPIO.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;
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

/// Accessories a channel addresses: every blind for `ALL`, otherwise the single
/// mapped blind (empty when the channel has no accessory).
///
/// Borrows from the compile-time [`BLINDS`] table, so callers that only iterate
/// never allocate.
pub fn blinds_for_channel(channel: Channel) -> &'static [Blind] {
    match channel {
        Channel::All => BLINDS,
        _ => match find_blind_for_channel(channel) {
            Some(blind) => std::slice::from_ref(blind),
            None => &[],
        },
    }
}

pub fn aids_for_channel(channel: Channel) -> impl Iterator<Item = u64> {
    blinds_for_channel(channel).iter().map(|blind| blind.aid)
}

pub fn target_positions(channel: Channel, position: u8) -> Vec<(u64, u8)> {
    aids_for_channel(channel)
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

/// Estimated state of every blind, in [`BLINDS`] order.
///
/// The accessory set is fixed at compile time, so a snapshot is a plain `Copy`
/// array — passing one around never touches the heap.
pub type PositionSnapshot = [BlindPosition; BLINDS.len()];

/// Look one accessory up in a snapshot, falling back to the default estimate.
pub fn position_for_aid(positions: &[BlindPosition], aid: u64) -> BlindPosition {
    positions
        .iter()
        .copied()
        .find(|position| position.aid == aid)
        .unwrap_or_else(|| BlindPosition::default_for_aid(aid))
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
    blinds: PositionSnapshot,
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
    /// Where `positions.json` is written. Resolved once at construction rather
    /// than per save, and `None` disables persistence entirely for tests.
    ///
    /// Held behind an `Arc` so handing the path to the blocking write is a
    /// refcount bump rather than a fresh `PathBuf` per save.
    path: Option<Arc<Path>>,
}

impl PositionCache {
    pub fn new() -> Self {
        let path: Arc<Path> = persist::state_dir().join(POSITIONS_FILE).into();
        Self {
            state: Mutex::new(PositionState::from_saved(&load_positions_from(&path))),
            path: Some(path),
        }
    }

    #[cfg(test)]
    pub fn from_positions(positions: HashMap<u64, u8>) -> Self {
        Self {
            state: Mutex::new(PositionState::from_saved(&positions)),
            path: None,
        }
    }

    #[cfg(test)]
    pub fn persisting_at(path: PathBuf, positions: HashMap<u64, u8>) -> Self {
        Self {
            state: Mutex::new(PositionState::from_saved(&positions)),
            path: Some(path.into()),
        }
    }

    pub async fn snapshot(&self) -> PositionSnapshot {
        self.state.lock().await.blinds
    }

    /// Snap every blind the channel addresses to `position`.
    ///
    /// `ALL` and a single channel differ only in how many blinds
    /// [`blinds_for_channel`] yields, so both go through one path.
    pub async fn apply_for_channel(&self, channel: Channel, position: u8) -> Vec<PositionDelta> {
        self.settle_all(blinds_for_channel(channel), position).await
    }

    pub async fn apply_blind_current(&self, blind: &Blind, position: u8) -> Vec<PositionDelta> {
        self.settle_all(std::slice::from_ref(blind), position).await
    }

    async fn settle_all(&self, blinds: &[Blind], position: u8) -> Vec<PositionDelta> {
        let mut state = self.state.lock().await;
        let deltas: Vec<PositionDelta> = blinds
            .iter()
            .filter_map(|blind| state.settle(blind.aid, position))
            .collect();
        if deltas.is_empty() {
            return Vec::new();
        }
        // `state` is deliberately still held across this await — see
        // [`Self::persist_positions`].
        self.persist_positions(state.blinds).await;
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

    /// Write `positions.json` on the blocking pool.
    ///
    /// The server runs on a current-thread runtime, so writing inline stalled
    /// every other connection — SSE, WebSocket, and the HAP server — for the
    /// duration of the write, the same reason RTS state writes go through
    /// [`tokio::task::spawn_blocking`]. It is a small unsynced write, but it is
    /// still `create_dir_all` + open + write + rename against a Pi's SD card on
    /// the reactor thread.
    ///
    /// Callers hold the state lock across this await on purpose: it serializes
    /// writers, so a slow write can never be overtaken by a newer one and leave
    /// a stale snapshot on disk. Without it the ordering guarantee would rest on
    /// `BlindController::operation_lock`, which this type cannot see.
    async fn persist_positions(&self, snapshot: PositionSnapshot) {
        let Some(path) = self.path.as_ref().map(Arc::clone) else {
            return;
        };
        match tokio::task::spawn_blocking(move || save_positions_to(&path, &snapshot)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("failed to persist positions: {e}"),
            Err(e) => tracing::warn!("position persistence task failed: {e}"),
        }
    }
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
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)
        .with_context(|| format!("creating state directory {}", dir.display()))?;
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

    fn persisting_cache(dir: &tempfile::TempDir) -> (PositionCache, PathBuf) {
        // A nested path also proves the parent directory gets created.
        let path = dir.path().join("state").join(POSITIONS_FILE);
        (
            PositionCache::persisting_at(path.clone(), HashMap::new()),
            path,
        )
    }

    #[tokio::test]
    async fn settling_writes_positions_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let (cache, path) = persisting_cache(&dir);

        cache.apply_for_channel(Channel::All, 0).await;

        assert_eq!(
            load_positions_from(&path),
            HashMap::from([(2u64, 0u8), (3, 0), (4, 0), (5, 0)])
        );
    }

    /// Writers are serialized by the state lock, so the newest snapshot is the
    /// one left on disk — a slow write can never be overtaken by a later one.
    #[tokio::test]
    async fn consecutive_settles_leave_the_last_value_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let (cache, path) = persisting_cache(&dir);

        for position in [10u8, 20, 30] {
            cache.apply_blind_current(&BLINDS[0], position).await;
        }

        assert_eq!(load_positions_from(&path).get(&2), Some(&30));
    }

    /// The write must not run on the reactor. `#[tokio::test]` is a
    /// current-thread runtime, so a ready task can only make progress if the
    /// persist actually yields — which it does only because the write is handed
    /// to `spawn_blocking`. Doing it inline again would fail this.
    #[tokio::test]
    async fn persisting_yields_the_reactor_to_other_tasks() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let (cache, _path) = persisting_cache(&dir);

        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        let task = tokio::spawn(async move { flag.store(true, Ordering::SeqCst) });

        cache.apply_for_channel(Channel::All, 0).await;

        assert!(
            ran.load(Ordering::SeqCst),
            "persist blocked the reactor: the ready task never got to run"
        );
        task.await.unwrap();
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
        assert_eq!(aids_for_channel(Channel::L2).collect::<Vec<_>>(), vec![3]);
        assert_eq!(
            aids_for_channel(Channel::All).collect::<Vec<_>>(),
            vec![2, 3, 4, 5]
        );
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
