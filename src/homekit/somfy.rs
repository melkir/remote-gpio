//! Somfy HomeKit accessory adapter.
//!
//! This module owns the project-specific accessory shape, persisted blind
//! position cache, and mapping from HomeKit `TargetPosition` writes to
//! remote-control commands. The HAP protocol server calls it only through
//! `HapAccessoryApp`.

use anyhow::anyhow;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

use crate::gpio::Channel;
use crate::hap::runtime::{
    CharacteristicEvent, CharacteristicId, CharacteristicRead, CharacteristicWrite,
    CharacteristicWriteOutcome, CharacteristicWriteStatus, HapAccessoryApp, HapFuture, HapStatus,
    Subscriptions,
};
use crate::homekit::positions;
use crate::remote::{Command, PositionUpdate, RemoteControl};

#[derive(Copy, Clone, Debug)]
struct Blind {
    aid: u64,
    name: &'static str,
    channel: Channel,
    serial: &'static str,
}

const BRIDGE_AID: u64 = 1;

const BLINDS: &[Blind] = &[
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
    Blind {
        aid: 6,
        name: "All Blinds",
        channel: Channel::ALL,
        serial: "somfy-ALL",
    },
];

const IID_AINFO_SERVICE: u64 = 1;
const IID_IDENTIFY: u64 = 2;
const IID_MANUFACTURER: u64 = 3;
const IID_MODEL: u64 = 4;
const IID_NAME: u64 = 5;
const IID_SERIAL: u64 = 6;
const IID_FIRMWARE: u64 = 7;
const IID_WC_SERVICE: u64 = 8;
const IID_CURRENT_POSITION: u64 = 9;
const IID_TARGET_POSITION: u64 = 10;
const IID_POSITION_STATE: u64 = 11;
const IID_BRIDGE_PROTO_SERVICE: u64 = 8;
const IID_BRIDGE_VERSION: u64 = 9;

const POSITION_STATE_STOPPED: u8 = 2;

pub struct SomfyHapApp {
    remote_control: Arc<RemoteControl>,
    /// aid -> cached position (0 or 100). Updated on any successful write or
    /// external position broadcast.
    positions: Mutex<HashMap<u64, u8>>,
}

impl SomfyHapApp {
    pub fn new(remote_control: Arc<RemoteControl>) -> Self {
        Self {
            remote_control,
            positions: Mutex::new(positions::load()),
        }
    }

    /// Mirror non-HAP command outcomes (REST, WS) into the HAP position cache.
    /// HAP-originated writes already update the cache before the broadcast
    /// lands here, so duplicates return no changes and emit no events.
    pub async fn run_position_listener(
        self: Arc<Self>,
        event_tx: broadcast::Sender<Vec<CharacteristicEvent>>,
        mut rx: broadcast::Receiver<PositionUpdate>,
    ) {
        loop {
            match rx.recv().await {
                Ok(update) => {
                    let changes = self
                        .apply_position_for_channel(update.channel, update.position)
                        .await;
                    if !changes.is_empty() {
                        let _ = event_tx.send(changes);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("position listener lagged by {n}");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    async fn snapshot_positions(&self) -> Vec<(u64, u8)> {
        let positions = self.positions.lock().await;
        BLINDS
            .iter()
            .map(|b| (b.aid, effective_position(&positions, b.aid)))
            .collect()
    }

    async fn apply_position_for_channel(
        &self,
        channel: Channel,
        pos: u8,
    ) -> Vec<CharacteristicEvent> {
        let Some(blind) = BLINDS.iter().find(|b| b.channel == channel) else {
            return Vec::new();
        };
        self.apply_position_change(blind, snap_position(pos)).await
    }

    /// Update the cached position for `blind`, propagate the ALL aggregate,
    /// and persist the snapshot. Returns the characteristic events changed vs.
    /// the prior cache.
    async fn apply_position_change(&self, blind: &Blind, snapped: u8) -> Vec<CharacteristicEvent> {
        let mut positions = self.positions.lock().await;
        if positions.get(&blind.aid).copied() == Some(snapped) {
            let needs_propagate = matches!(blind.channel, Channel::ALL)
                && BLINDS
                    .iter()
                    .filter(|b| !matches!(b.channel, Channel::ALL))
                    .any(|b| positions.get(&b.aid).copied() != Some(snapped));
            if !needs_propagate {
                return Vec::new();
            }
        }
        let before = positions.clone();
        positions.insert(blind.aid, snapped);
        propagate_positions(&mut positions, blind, snapped);
        let snapshot = positions.clone();
        if let Err(e) = positions::save(&snapshot) {
            tracing::warn!("failed to persist positions: {e}");
        }
        drop(positions);

        snapshot
            .iter()
            .filter(|(aid, pos)| before.get(aid) != Some(pos))
            .flat_map(|(aid, pos)| position_events(*aid, *pos))
            .collect()
    }
}

impl HapAccessoryApp for SomfyHapApp {
    fn accessories(&self) -> HapFuture<'_, Value> {
        Box::pin(async move {
            let positions = self.snapshot_positions().await;
            Ok(build_accessories(&positions))
        })
    }

    fn read_characteristics<'a>(
        &'a self,
        ids: &'a [CharacteristicId],
    ) -> HapFuture<'a, Vec<CharacteristicRead>> {
        Box::pin(async move {
            let positions = self.positions.lock().await;
            let values = ids
                .iter()
                .map(|id| read_characteristic(&positions, *id))
                .collect();
            Ok(values)
        })
    }

    fn write_characteristics<'a>(
        &'a self,
        writes: Vec<CharacteristicWrite>,
        subscriptions: &'a mut Subscriptions,
    ) -> HapFuture<'a, CharacteristicWriteOutcome> {
        Box::pin(async move {
            let mut outcome = CharacteristicWriteOutcome::default();
            for write in writes {
                if let Some(ev) = write.ev {
                    let status = handle_subscription(write.id, ev, subscriptions);
                    outcome.statuses.push(status);
                    continue;
                }

                if write.id.iid.0 == IID_IDENTIFY && is_known_characteristic(write.id) {
                    outcome
                        .statuses
                        .push(CharacteristicWriteStatus::success(write.id));
                    continue;
                }

                if write.id.iid.0 != IID_TARGET_POSITION || find_blind(write.id.aid.0).is_none() {
                    outcome.statuses.push(CharacteristicWriteStatus::error(
                        write.id,
                        write_error_status(write.id),
                    ));
                    continue;
                }

                let value = match write.value.and_then(|v| v.as_u64()) {
                    Some(v) if v <= 100 => v as u8,
                    _ => {
                        outcome.statuses.push(CharacteristicWriteStatus::error(
                            write.id,
                            HapStatus::InvalidValue,
                        ));
                        continue;
                    }
                };
                let blind = find_blind(write.id.aid.0).expect("blind checked above");
                let snapped = snap_position(value);

                let positions = self.positions.lock().await;
                if already_at_target(&positions, blind, snapped) {
                    tracing::debug!(
                        "PUT TargetPosition aid={} value={snapped}: cache hit, no-op",
                        write.id.aid.0
                    );
                    outcome
                        .statuses
                        .push(CharacteristicWriteStatus::success(write.id));
                    continue;
                }
                drop(positions);

                let command = if snapped == 100 {
                    Command::Up
                } else {
                    Command::Down
                };
                self.remote_control
                    .execute_on(blind.channel, command)
                    .await
                    .map_err(|e| anyhow!(e))?;

                outcome
                    .events
                    .extend(self.apply_position_change(blind, snapped).await);
                outcome
                    .statuses
                    .push(CharacteristicWriteStatus::success(write.id));
            }
            Ok(outcome)
        })
    }
}

fn handle_subscription(
    id: CharacteristicId,
    enabled: bool,
    subscriptions: &mut Subscriptions,
) -> CharacteristicWriteStatus {
    if !is_known_characteristic(id) {
        return CharacteristicWriteStatus::error(id, HapStatus::ResourceDoesNotExist);
    }
    if !supports_events(id) {
        return CharacteristicWriteStatus::error(id, HapStatus::NotificationNotSupported);
    }
    if enabled {
        subscriptions.insert(id);
    } else {
        subscriptions.remove(&id);
    }
    CharacteristicWriteStatus::success(id)
}

fn read_characteristic(positions: &HashMap<u64, u8>, id: CharacteristicId) -> CharacteristicRead {
    let aid = id.aid.0;
    let iid = id.iid.0;
    let value = match (aid, iid) {
        (BRIDGE_AID, IID_MANUFACTURER) => json!("Somfy"),
        (BRIDGE_AID, IID_MODEL) => json!("Telis 4 Bridge"),
        (BRIDGE_AID, IID_NAME) => json!("Somfy Bridge"),
        (BRIDGE_AID, IID_SERIAL) => json!("somfy-bridge"),
        (BRIDGE_AID, IID_FIRMWARE) => json!(env!("CARGO_PKG_VERSION")),
        (BRIDGE_AID, IID_BRIDGE_VERSION) => json!("1.1.0"),
        (_, IID_IDENTIFY) if is_known_characteristic(id) => {
            return CharacteristicRead::error(id, HapStatus::WriteOnly);
        }
        (a, IID_MANUFACTURER) if find_blind(a).is_some() => json!("Somfy"),
        (a, IID_MODEL) if find_blind(a).is_some() => json!("Telis 4"),
        (a, IID_NAME) => match find_blind(a) {
            Some(blind) => json!(blind.name),
            None => return CharacteristicRead::error(id, HapStatus::ResourceDoesNotExist),
        },
        (a, IID_SERIAL) => match find_blind(a) {
            Some(blind) => json!(blind.serial),
            None => return CharacteristicRead::error(id, HapStatus::ResourceDoesNotExist),
        },
        (a, IID_FIRMWARE) if find_blind(a).is_some() => json!(env!("CARGO_PKG_VERSION")),
        (a, IID_CURRENT_POSITION) if find_blind(a).is_some() => {
            json!(effective_position(positions, a))
        }
        (a, IID_TARGET_POSITION) if find_blind(a).is_some() => {
            json!(effective_position(positions, a))
        }
        (a, IID_POSITION_STATE) if find_blind(a).is_some() => json!(POSITION_STATE_STOPPED),
        _ => return CharacteristicRead::error(id, HapStatus::ResourceDoesNotExist),
    };
    CharacteristicRead::success(id, value)
}

fn write_error_status(id: CharacteristicId) -> HapStatus {
    if is_known_characteristic(id) {
        HapStatus::ReadOnly
    } else {
        HapStatus::ResourceDoesNotExist
    }
}

fn is_known_characteristic(id: CharacteristicId) -> bool {
    let aid = id.aid.0;
    let iid = id.iid.0;
    match aid {
        BRIDGE_AID => matches!(
            iid,
            IID_IDENTIFY
                | IID_MANUFACTURER
                | IID_MODEL
                | IID_NAME
                | IID_SERIAL
                | IID_FIRMWARE
                | IID_BRIDGE_VERSION
        ),
        _ if find_blind(aid).is_some() => matches!(
            iid,
            IID_IDENTIFY
                | IID_MANUFACTURER
                | IID_MODEL
                | IID_NAME
                | IID_SERIAL
                | IID_FIRMWARE
                | IID_CURRENT_POSITION
                | IID_TARGET_POSITION
                | IID_POSITION_STATE
        ),
        _ => false,
    }
}

fn supports_events(id: CharacteristicId) -> bool {
    find_blind(id.aid.0).is_some()
        && matches!(
            id.iid.0,
            IID_CURRENT_POSITION | IID_TARGET_POSITION | IID_POSITION_STATE
        )
}

fn position_events(aid: u64, position: u8) -> Vec<CharacteristicEvent> {
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

fn snap_position(value: u8) -> u8 {
    if value >= 50 {
        100
    } else {
        0
    }
}

fn already_at_target(positions: &HashMap<u64, u8>, blind: &Blind, snapped: u8) -> bool {
    if matches!(blind.channel, Channel::ALL) {
        BLINDS
            .iter()
            .filter(|b| !matches!(b.channel, Channel::ALL))
            .all(|b| positions.get(&b.aid).copied() == Some(snapped))
    } else {
        positions.get(&blind.aid).copied() == Some(snapped)
    }
}

fn propagate_positions(positions: &mut HashMap<u64, u8>, changed: &Blind, snapped: u8) {
    if matches!(changed.channel, Channel::ALL) {
        for b in BLINDS.iter().filter(|b| !matches!(b.channel, Channel::ALL)) {
            positions.insert(b.aid, snapped);
        }
        return;
    }
    let individuals: Vec<&Blind> = BLINDS
        .iter()
        .filter(|b| !matches!(b.channel, Channel::ALL))
        .collect();
    let all_match = individuals
        .iter()
        .all(|b| positions.get(&b.aid).copied() == Some(snapped));
    let all_blind = BLINDS.iter().find(|b| matches!(b.channel, Channel::ALL));
    if let Some(all_blind) = all_blind {
        if all_match {
            positions.insert(all_blind.aid, snapped);
        }
    }
}

fn effective_position(positions: &HashMap<u64, u8>, aid: u64) -> u8 {
    let Some(blind) = find_blind(aid) else {
        return 100;
    };
    if !matches!(blind.channel, Channel::ALL) {
        return positions.get(&aid).copied().unwrap_or(100);
    }

    let mut individual_positions = BLINDS
        .iter()
        .filter(|b| !matches!(b.channel, Channel::ALL))
        .map(|b| positions.get(&b.aid).copied());
    let Some(Some(first)) = individual_positions.next() else {
        return positions.get(&aid).copied().unwrap_or(100);
    };
    if individual_positions.all(|pos| pos == Some(first)) {
        first
    } else {
        positions.get(&aid).copied().unwrap_or(100)
    }
}

fn find_blind(aid: u64) -> Option<&'static Blind> {
    BLINDS.iter().find(|b| b.aid == aid)
}

fn build_accessories(positions: &[(u64, u8)]) -> Value {
    let mut accessories = vec![bridge_accessory()];
    for blind in BLINDS {
        let pos = positions
            .iter()
            .find(|(a, _)| *a == blind.aid)
            .map(|(_, p)| *p)
            .unwrap_or(100);
        accessories.push(blind_accessory(blind, pos));
    }
    json!({ "accessories": accessories })
}

fn bridge_accessory() -> Value {
    let firmware = env!("CARGO_PKG_VERSION");
    json!({
        "aid": BRIDGE_AID,
        "services": [
            {
                "iid": IID_AINFO_SERVICE,
                "type": "3E",
                "characteristics": [
                    char_string(IID_MANUFACTURER, "20", "Somfy", &["pr"]),
                    char_string(IID_MODEL, "21", "Telis 4 Bridge", &["pr"]),
                    char_string(IID_NAME, "23", "Somfy Bridge", &["pr"]),
                    char_string(IID_SERIAL, "30", "somfy-bridge", &["pr"]),
                    char_string(IID_FIRMWARE, "52", firmware, &["pr"]),
                    char_bool_pw(IID_IDENTIFY, "14"),
                ],
            },
            {
                "iid": IID_BRIDGE_PROTO_SERVICE,
                "type": "A2",
                "characteristics": [
                    char_string(IID_BRIDGE_VERSION, "37", "1.1.0", &["pr"]),
                ],
            }
        ]
    })
}

fn blind_accessory(blind: &Blind, position: u8) -> Value {
    let firmware = env!("CARGO_PKG_VERSION");
    json!({
        "aid": blind.aid,
        "services": [
            {
                "iid": IID_AINFO_SERVICE,
                "type": "3E",
                "characteristics": [
                    char_string(IID_MANUFACTURER, "20", "Somfy", &["pr"]),
                    char_string(IID_MODEL, "21", "Telis 4", &["pr"]),
                    char_string(IID_NAME, "23", blind.name, &["pr"]),
                    char_string(IID_SERIAL, "30", blind.serial, &["pr"]),
                    char_string(IID_FIRMWARE, "52", firmware, &["pr"]),
                    char_bool_pw(IID_IDENTIFY, "14"),
                ],
            },
            {
                "iid": IID_WC_SERVICE,
                "type": "8C",
                "characteristics": [
                    char_uint8(IID_CURRENT_POSITION, "6D", position, &["pr", "ev"], 100),
                    char_uint8(IID_TARGET_POSITION, "7C", position, &["pr", "pw", "ev"], 100),
                    char_uint8(IID_POSITION_STATE, "72", POSITION_STATE_STOPPED, &["pr", "ev"], 2),
                ],
            }
        ]
    })
}

fn char_string(iid: u64, type_: &str, value: &str, perms: &[&str]) -> Value {
    json!({
        "iid": iid,
        "type": type_,
        "perms": perms,
        "format": "string",
        "value": value,
    })
}

fn char_uint8(iid: u64, type_: &str, value: u8, perms: &[&str], max_value: u8) -> Value {
    json!({
        "iid": iid,
        "type": type_,
        "perms": perms,
        "format": "uint8",
        "value": value,
        "minValue": 0,
        "maxValue": max_value,
        "minStep": 1,
    })
}

fn char_bool_pw(iid: u64, type_: &str) -> Value {
    json!({
        "iid": iid,
        "type": type_,
        "perms": ["pw"],
        "format": "bool",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_state_metadata_is_hap_enum_range() {
        let body = build_accessories(&[(2, 100)]);
        let chars = &body["accessories"][1]["services"][1]["characteristics"];
        let state = chars
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["iid"] == IID_POSITION_STATE)
            .unwrap();

        assert_eq!(state["value"], POSITION_STATE_STOPPED);
        assert_eq!(state["maxValue"], 2);
    }

    #[test]
    fn unknown_read_returns_resource_missing_status() {
        let positions = HashMap::new();
        let read = read_characteristic(&positions, CharacteristicId::new(99, 99));

        assert_eq!(read.status, HapStatus::ResourceDoesNotExist);
        assert!(read.value.is_none());
    }

    #[test]
    fn read_position_returns_cached_value() {
        let mut positions = HashMap::new();
        positions.insert(2, 0);

        let read = read_characteristic(&positions, CharacteristicId::new(2, IID_CURRENT_POSITION));

        assert_eq!(read.status, HapStatus::Success);
        assert_eq!(read.value, Some(json!(0)));
    }

    #[test]
    fn unsupported_write_reports_protocol_status() {
        assert_eq!(
            write_error_status(CharacteristicId::new(2, IID_CURRENT_POSITION)),
            HapStatus::ReadOnly
        );
        assert_eq!(
            write_error_status(CharacteristicId::new(99, 99)),
            HapStatus::ResourceDoesNotExist
        );
    }

    #[test]
    fn all_blinds_position_uses_matching_individual_positions() {
        let mut positions = HashMap::new();
        positions.insert(2, 0);
        positions.insert(3, 0);
        positions.insert(4, 0);
        positions.insert(5, 0);

        assert_eq!(effective_position(&positions, 6), 0);
    }

    #[test]
    fn propagate_keeps_aggregate_when_individuals_diverge() {
        let mut positions = HashMap::new();
        positions.insert(2, 0);
        positions.insert(3, 0);
        positions.insert(4, 0);
        positions.insert(5, 0);
        positions.insert(6, 0);

        let changed = find_blind(2).unwrap();
        positions.insert(2, 100);
        propagate_positions(&mut positions, changed, 100);
        assert_eq!(positions.get(&6), Some(&0));

        let all_blind = find_blind(6).unwrap();
        assert!(matches!(all_blind.channel, Channel::ALL));
        assert!(!already_at_target(&positions, all_blind, 0));
    }

    #[test]
    fn all_blinds_position_falls_back_when_individual_positions_are_missing_or_mixed() {
        let mut positions = HashMap::new();
        positions.insert(6, 100);
        positions.insert(2, 0);
        positions.insert(3, 0);
        positions.insert(4, 100);
        positions.insert(5, 0);

        assert_eq!(effective_position(&positions, 6), 100);

        positions.insert(6, 0);
        positions.remove(&4);
        assert_eq!(effective_position(&positions, 6), 0);
    }

    #[test]
    fn all_write_propagates_to_individual_blinds() {
        let mut positions = HashMap::new();
        let all_blind = find_blind(6).unwrap();
        propagate_positions(&mut positions, all_blind, 100);

        for aid in [2, 3, 4, 5] {
            assert_eq!(positions.get(&aid), Some(&100));
        }
    }

    #[test]
    fn individual_write_updates_aggregate_when_all_match() {
        let mut positions = HashMap::new();
        positions.insert(2, 100);
        positions.insert(3, 0);
        positions.insert(4, 0);
        positions.insert(5, 0);
        positions.insert(6, 100);

        let changed = find_blind(2).unwrap();
        positions.insert(2, 0);
        propagate_positions(&mut positions, changed, 0);

        assert_eq!(positions.get(&6), Some(&0));
    }

    #[test]
    fn repeated_target_is_noop() {
        let mut positions = HashMap::new();
        positions.insert(2, 100);
        let blind = find_blind(2).unwrap();

        assert!(already_at_target(&positions, blind, 100));
    }

    #[test]
    fn external_position_broadcast_produces_hap_events() {
        let events = position_events(2, 0);

        assert_eq!(events.len(), 3);
        assert!(events
            .iter()
            .any(|e| e.id == CharacteristicId::new(2, IID_CURRENT_POSITION)));
    }
}
