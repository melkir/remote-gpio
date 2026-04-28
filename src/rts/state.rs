use anyhow::{bail, Context, Result};
use rand::rngs::OsRng;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::gpio::Channel;
use crate::hap::state::atomic_save_bytes;
use crate::homekit::config;

pub const STATE_FILE: &str = "rts.json";
pub const SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_RESERVE_SIZE: u16 = 16;

const CHANNELS: [Channel; 5] = [
    Channel::L1,
    Channel::L2,
    Channel::L3,
    Channel::L4,
    Channel::ALL,
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RtsState {
    pub schema_version: u32,
    #[serde(default = "default_selected_channel")]
    pub selected_channel: Channel,
    pub channels: BTreeMap<Channel, RtsChannelState>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RtsChannelState {
    pub remote_id: u32,
    pub reserved_until: u16,
}

#[derive(Debug)]
pub struct RtsStateStore {
    path: PathBuf,
    reserve_size: u16,
    state: RtsState,
    next_on_wire: BTreeMap<Channel, u16>,
}

impl RtsStateStore {
    pub fn load_or_init_default() -> Result<Self> {
        Self::load_or_init(config::state_dir().join(STATE_FILE), DEFAULT_RESERVE_SIZE)
    }

    pub fn load_or_init(path: impl Into<PathBuf>, reserve_size: u16) -> Result<Self> {
        let path = path.into();
        let state = match load_from(&path)? {
            Some(state) => state,
            None => {
                let state = RtsState::generate();
                save_to(&path, &state)?;
                state
            }
        };
        Self::from_state(path, reserve_size, state)
    }

    fn from_state(path: PathBuf, reserve_size: u16, state: RtsState) -> Result<Self> {
        if reserve_size == 0 {
            bail!("RTS rolling-code reserve_size must be greater than zero");
        }
        validate_state(&state)?;
        let next_on_wire = state
            .channels
            .iter()
            .map(|(channel, state)| (*channel, state.reserved_until))
            .collect();
        Ok(Self {
            path,
            reserve_size,
            state,
            next_on_wire,
        })
    }

    pub fn selected_channel(&self) -> Channel {
        self.state.selected_channel
    }

    pub fn set_selected_channel(&mut self, channel: Channel) -> Result<()> {
        self.state.selected_channel = channel;
        save_to(&self.path, &self.state)
    }

    pub fn channel(&self, channel: Channel) -> &RtsChannelState {
        self.state
            .channels
            .get(&channel)
            .expect("validated RTS state has every channel")
    }

    pub fn next_on_wire(&self, channel: Channel) -> u16 {
        *self
            .next_on_wire
            .get(&channel)
            .expect("validated RTS state has every channel")
    }

    pub fn reserve_rolling_code(&mut self, channel: Channel) -> Result<u16> {
        let next = self.next_on_wire(channel);
        let reserved_until = self.channel(channel).reserved_until;
        if next >= reserved_until {
            let new_reserved_until = next.checked_add(self.reserve_size).ok_or_else(|| {
                anyhow::anyhow!("rolling code reserve for {channel} would overflow")
            })?;
            self.state
                .channels
                .get_mut(&channel)
                .expect("validated RTS state has every channel")
                .reserved_until = new_reserved_until;
            save_to(&self.path, &self.state)?;
        }
        Ok(next)
    }

    pub fn commit_rolling_code(&mut self, channel: Channel, code: u16) -> Result<()> {
        let next = self.next_on_wire(channel);
        if code != next {
            bail!("cannot commit rolling code {code} for {channel}; next on wire is {next}");
        }
        let next = next
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("rolling code for {channel} would overflow"))?;
        self.next_on_wire.insert(channel, next);
        Ok(())
    }
}

impl RtsState {
    pub fn generate() -> Self {
        let mut used = BTreeSet::new();
        let channels = CHANNELS
            .into_iter()
            .map(|channel| {
                let remote_id = unique_remote_id(&mut used);
                (
                    channel,
                    RtsChannelState {
                        remote_id,
                        reserved_until: 1,
                    },
                )
            })
            .collect();

        Self {
            schema_version: SCHEMA_VERSION,
            selected_channel: Channel::L1,
            channels,
        }
    }
}

fn default_selected_channel() -> Channel {
    Channel::L1
}

fn unique_remote_id(used: &mut BTreeSet<u32>) -> u32 {
    let mut rng = OsRng;
    loop {
        let id = rng.gen_range(1..=0xFF_FFFF);
        if used.insert(id) {
            return id;
        }
    }
}

fn validate_state(state: &RtsState) -> Result<()> {
    if state.schema_version != SCHEMA_VERSION {
        bail!(
            "unsupported RTS state schema_version {}; expected {SCHEMA_VERSION}",
            state.schema_version
        );
    }

    let mut remote_ids = BTreeSet::new();
    for channel in CHANNELS {
        let state = state
            .channels
            .get(&channel)
            .ok_or_else(|| anyhow::anyhow!("RTS state missing channel {channel}"))?;
        if state.remote_id == 0 || state.remote_id > 0xFF_FFFF {
            bail!("RTS state channel {channel} has invalid remote_id");
        }
        if !remote_ids.insert(state.remote_id) {
            bail!("RTS state reuses remote_id {}", state.remote_id);
        }
    }
    Ok(())
}

fn load_from(path: &Path) -> Result<Option<RtsState>> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let state = serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            Ok(Some(state))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn save_to(path: &Path, state: &RtsState) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("creating state directory {}", parent.display()))?;
    let bytes = serde_json::to_vec_pretty(state)?;
    atomic_save_bytes(path, &bytes, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(STATE_FILE)
    }

    #[test]
    fn missing_file_initializes_all_channels() {
        let dir = tempfile::tempdir().unwrap();
        let store = RtsStateStore::load_or_init(state_path(&dir), DEFAULT_RESERVE_SIZE).unwrap();

        assert_eq!(store.selected_channel(), Channel::L1);
        assert!(state_path(&dir).exists());
        for channel in CHANNELS {
            assert!(store.channel(channel).remote_id > 0);
            assert_eq!(store.channel(channel).reserved_until, 1);
            assert_eq!(store.next_on_wire(channel), 1);
        }
    }

    #[test]
    fn unknown_schema_version_fails_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        fs::write(
            &path,
            r#"{"schema_version":2,"selected_channel":"L1","channels":{}}"#,
        )
        .unwrap();

        let err = RtsStateStore::load_or_init(path, DEFAULT_RESERVE_SIZE).unwrap_err();
        assert!(err
            .to_string()
            .contains("unsupported RTS state schema_version"));
    }

    #[test]
    fn generated_remote_ids_are_independent_per_channel() {
        let state = RtsState::generate();
        let ids: BTreeSet<u32> = state.channels.values().map(|c| c.remote_id).collect();

        assert_eq!(ids.len(), CHANNELS.len());
        assert!(ids.iter().all(|id| (1..=0xFF_FFFF).contains(id)));
    }

    #[test]
    fn selected_channel_defaults_when_missing_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        fs::write(
            &path,
            r#"{
              "schema_version": 1,
              "channels": {
                "L1": {"remote_id": 1, "reserved_until": 1},
                "L2": {"remote_id": 2, "reserved_until": 1},
                "L3": {"remote_id": 3, "reserved_until": 1},
                "L4": {"remote_id": 4, "reserved_until": 1},
                "ALL": {"remote_id": 5, "reserved_until": 1}
              }
            }"#,
        )
        .unwrap();

        let mut store = RtsStateStore::load_or_init(&path, DEFAULT_RESERVE_SIZE).unwrap();
        assert_eq!(store.selected_channel(), Channel::L1);

        store.set_selected_channel(Channel::ALL).unwrap();
        let store = RtsStateStore::load_or_init(&path, DEFAULT_RESERVE_SIZE).unwrap();
        assert_eq!(store.selected_channel(), Channel::ALL);
    }

    #[test]
    fn rolling_codes_are_independent_and_reserve_persists_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let mut store = RtsStateStore::load_or_init(&path, 16).unwrap();

        assert_eq!(store.reserve_rolling_code(Channel::L1).unwrap(), 1);
        assert_eq!(store.channel(Channel::L1).reserved_until, 17);
        assert_eq!(store.channel(Channel::L2).reserved_until, 1);
        assert!(path.exists());

        store.commit_rolling_code(Channel::L1, 1).unwrap();
        assert_eq!(store.next_on_wire(Channel::L1), 2);
        assert_eq!(store.next_on_wire(Channel::L2), 1);
    }

    #[test]
    fn failed_transmit_does_not_advance_in_memory_code_but_restart_skips_reserved_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = state_path(&dir);
        let mut store = RtsStateStore::load_or_init(&path, 16).unwrap();

        let code = store.reserve_rolling_code(Channel::L1).unwrap();
        assert_eq!(code, 1);
        assert_eq!(store.next_on_wire(Channel::L1), 1);

        let restarted = RtsStateStore::load_or_init(&path, 16).unwrap();
        assert_eq!(restarted.next_on_wire(Channel::L1), 17);
    }
}
