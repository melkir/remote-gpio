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

#[derive(Copy, Clone, Debug)]
struct PendingTargetWrite {
    index: usize,
    id: CharacteristicId,
    blind: &'static Blind,
    snapped: u8,
}

pub struct SomfyHapApp {
    remote_control: Arc<RemoteControl>,
    /// aid -> cached position (0 or 100). Updated on any successful write or
    /// external position broadcast.
    positions: Mutex<HashMap<u64, u8>>,
    persist_positions: bool,
}

impl SomfyHapApp {
    pub fn new(remote_control: Arc<RemoteControl>) -> Self {
        Self {
            remote_control,
            positions: Mutex::new(positions::load()),
            persist_positions: true,
        }
    }

    #[cfg(all(test, feature = "fake"))]
    fn new_with_positions(remote_control: Arc<RemoteControl>, positions: HashMap<u64, u8>) -> Self {
        Self {
            remote_control,
            positions: Mutex::new(positions),
            persist_positions: false,
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
        let snapped = snap_position(pos);
        if matches!(channel, Channel::ALL) {
            return self.apply_all_position_change(snapped).await;
        }
        let Some(blind) = BLINDS.iter().find(|b| b.channel == channel) else {
            return Vec::new();
        };
        self.apply_position_change(blind, snapped).await
    }

    /// Update the cached position for `blind` and persist the snapshot. Returns
    /// the characteristic events changed vs. the prior cache.
    async fn apply_position_change(&self, blind: &Blind, snapped: u8) -> Vec<CharacteristicEvent> {
        let mut positions = self.positions.lock().await;
        if positions.get(&blind.aid).copied() == Some(snapped) {
            return Vec::new();
        }
        let before = positions.clone();
        positions.insert(blind.aid, snapped);
        let snapshot = positions.clone();
        if self.persist_positions {
            if let Err(e) = positions::save(&snapshot) {
                tracing::warn!("failed to persist positions: {e}");
            }
        }
        drop(positions);

        snapshot
            .iter()
            .filter(|(aid, pos)| before.get(aid) != Some(pos))
            .flat_map(|(aid, pos)| position_events(*aid, *pos))
            .collect()
    }

    async fn apply_all_position_change(&self, snapped: u8) -> Vec<CharacteristicEvent> {
        let mut positions = self.positions.lock().await;
        if BLINDS
            .iter()
            .all(|blind| positions.get(&blind.aid).copied() == Some(snapped))
        {
            return Vec::new();
        }

        let before = positions.clone();
        for blind in BLINDS {
            positions.insert(blind.aid, snapped);
        }
        let snapshot = positions.clone();
        if self.persist_positions {
            if let Err(e) = positions::save(&snapshot) {
                tracing::warn!("failed to persist positions: {e}");
            }
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
            let mut statuses = Vec::new();
            let mut target_writes = Vec::new();
            for write in writes {
                let index = statuses.len();
                statuses.push(None);

                if let Some(ev) = write.ev {
                    let status = handle_subscription(write.id, ev, subscriptions);
                    statuses[index] = Some(status);
                    continue;
                }

                if write.id.iid.0 == IID_IDENTIFY && is_known_characteristic(write.id) {
                    statuses[index] = Some(CharacteristicWriteStatus::success(write.id));
                    continue;
                }

                if write.id.iid.0 != IID_TARGET_POSITION || find_blind(write.id.aid.0).is_none() {
                    statuses[index] = Some(CharacteristicWriteStatus::error(
                        write.id,
                        write_error_status(write.id),
                    ));
                    continue;
                }

                let value = match write.value.and_then(|v| v.as_u64()) {
                    Some(v) if v <= 100 => v as u8,
                    _ => {
                        statuses[index] = Some(CharacteristicWriteStatus::error(
                            write.id,
                            HapStatus::InvalidValue,
                        ));
                        continue;
                    }
                };
                let blind = find_blind(write.id.aid.0).expect("blind checked above");
                let snapped = snap_position(value);

                target_writes.push(PendingTargetWrite {
                    index,
                    id: write.id,
                    blind,
                    snapped,
                });
            }

            if let Some(snapped) = grouped_all_target(&target_writes) {
                let positions = self.positions.lock().await;
                if all_at_target(&positions, snapped) {
                    tracing::debug!("PUT TargetPosition grouped value={snapped}: cache hit, no-op");
                } else {
                    drop(positions);

                    let command = if snapped == 100 {
                        Command::Up
                    } else {
                        Command::Down
                    };
                    self.remote_control
                        .execute_on(Channel::ALL, command)
                        .await
                        .map_err(|e| anyhow!(e))?;

                    outcome
                        .events
                        .extend(self.apply_all_position_change(snapped).await);
                }
                for target in target_writes {
                    statuses[target.index] = Some(CharacteristicWriteStatus::success(target.id));
                }
                outcome.statuses = statuses.into_iter().flatten().collect();
                return Ok(outcome);
            }

            for target in target_writes {
                let positions = self.positions.lock().await;
                if positions.get(&target.blind.aid).copied() == Some(target.snapped) {
                    tracing::debug!(
                        "PUT TargetPosition aid={} value={}: cache hit, no-op",
                        target.id.aid.0,
                        target.snapped
                    );
                    statuses[target.index] = Some(CharacteristicWriteStatus::success(target.id));
                    continue;
                }
                drop(positions);

                let command = if target.snapped == 100 {
                    Command::Up
                } else {
                    Command::Down
                };
                self.remote_control
                    .execute_on(target.blind.channel, command)
                    .await
                    .map_err(|e| anyhow!(e))?;

                outcome.events.extend(
                    self.apply_position_change(target.blind, target.snapped)
                        .await,
                );
                statuses[target.index] = Some(CharacteristicWriteStatus::success(target.id));
            }
            outcome.statuses = statuses.into_iter().flatten().collect();
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
    let value = if aid == BRIDGE_AID {
        match iid {
            IID_IDENTIFY => return CharacteristicRead::error(id, HapStatus::WriteOnly),
            IID_MANUFACTURER => json!("Somfy"),
            IID_MODEL => json!("Telis 4 Bridge"),
            IID_NAME => json!("Somfy Bridge"),
            IID_SERIAL => json!("somfy-bridge"),
            IID_FIRMWARE => json!(env!("CARGO_PKG_VERSION")),
            IID_BRIDGE_VERSION => json!("1.1.0"),
            _ => return CharacteristicRead::error(id, HapStatus::ResourceDoesNotExist),
        }
    } else if let Some(blind) = find_blind(aid) {
        match iid {
            IID_IDENTIFY => return CharacteristicRead::error(id, HapStatus::WriteOnly),
            IID_MANUFACTURER => json!("Somfy"),
            IID_MODEL => json!("Telis 4"),
            IID_NAME => json!(blind.name),
            IID_SERIAL => json!(blind.serial),
            IID_FIRMWARE => json!(env!("CARGO_PKG_VERSION")),
            IID_CURRENT_POSITION | IID_TARGET_POSITION => json!(effective_position(positions, aid)),
            IID_POSITION_STATE => json!(POSITION_STATE_STOPPED),
            _ => return CharacteristicRead::error(id, HapStatus::ResourceDoesNotExist),
        }
    } else {
        return CharacteristicRead::error(id, HapStatus::ResourceDoesNotExist);
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

fn all_at_target(positions: &HashMap<u64, u8>, snapped: u8) -> bool {
    BLINDS
        .iter()
        .all(|blind| positions.get(&blind.aid).copied() == Some(snapped))
}

fn grouped_all_target(targets: &[PendingTargetWrite]) -> Option<u8> {
    let first = targets.first()?;
    if !targets.iter().all(|target| target.snapped == first.snapped) {
        return None;
    }

    let covers_all_individuals = BLINDS
        .iter()
        .all(|blind| targets.iter().any(|target| target.blind.aid == blind.aid));

    if covers_all_individuals {
        Some(first.snapped)
    } else {
        None
    }
}

fn effective_position(positions: &HashMap<u64, u8>, aid: u64) -> u8 {
    let Some(blind) = find_blind(aid) else {
        return 100;
    };
    positions.get(&blind.aid).copied().unwrap_or(100)
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
    fn all_at_target_checks_individual_blinds_only() {
        let mut positions = HashMap::new();
        positions.insert(2, 0);
        positions.insert(3, 0);
        positions.insert(4, 0);
        positions.insert(5, 0);

        assert!(all_at_target(&positions, 0));
        assert!(!all_at_target(&positions, 100));
    }

    #[test]
    fn full_individual_batch_groups_to_all_blinds() {
        let targets = [2, 3, 4, 5]
            .into_iter()
            .map(|aid| pending_target(aid, 100))
            .collect::<Vec<_>>();

        let snapped = grouped_all_target(&targets).unwrap();

        assert_eq!(snapped, 100);
    }

    #[test]
    fn accessories_expose_four_blinds() {
        let body = build_accessories(&[(2, 100), (3, 100), (4, 100), (5, 100)]);
        let aids = body["accessories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|accessory| accessory["aid"].as_u64().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(aids, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn partial_individual_batch_does_not_group_to_all_blinds() {
        let targets = [2, 3]
            .into_iter()
            .map(|aid| pending_target(aid, 100))
            .collect::<Vec<_>>();

        assert!(grouped_all_target(&targets).is_none());
    }

    #[test]
    fn mixed_direction_batch_does_not_group_to_all_blinds() {
        let targets = vec![
            pending_target(2, 100),
            pending_target(3, 100),
            pending_target(4, 0),
            pending_target(5, 100),
        ];

        assert!(grouped_all_target(&targets).is_none());
    }

    #[cfg(feature = "fake")]
    #[tokio::test]
    async fn full_individual_write_batch_sends_one_all_driver_command() {
        let remote_control = Arc::new(
            RemoteControl::with_driver(crate::driver::DriverConfig::fake())
                .await
                .unwrap(),
        );
        let app = SomfyHapApp::new_with_positions(remote_control.clone(), HashMap::new());
        let writes = [2, 3, 4, 5]
            .into_iter()
            .map(|aid| CharacteristicWrite {
                id: CharacteristicId::new(aid, IID_TARGET_POSITION),
                value: Some(json!(100)),
                ev: None,
            })
            .collect::<Vec<_>>();
        let mut subscriptions = Subscriptions::default();

        let outcome = app
            .write_characteristics(writes, &mut subscriptions)
            .await
            .unwrap();

        assert!(outcome.all_success());
        assert_eq!(outcome.statuses.len(), 4);
        assert_eq!(
            remote_control.operations(),
            vec![crate::driver::ProtocolOperation::FakeCommand {
                channel: Channel::ALL,
                command: Command::Up,
            }]
        );
    }

    #[cfg(feature = "fake")]
    #[tokio::test]
    async fn cache_hit_does_not_break_full_batch_coalesce() {
        let remote_control = Arc::new(
            RemoteControl::with_driver(crate::driver::DriverConfig::fake())
                .await
                .unwrap(),
        );
        let mut positions = HashMap::new();
        positions.insert(2, 100);
        let app = SomfyHapApp::new_with_positions(remote_control.clone(), positions);
        let writes = [2, 3, 4, 5]
            .into_iter()
            .map(|aid| CharacteristicWrite {
                id: CharacteristicId::new(aid, IID_TARGET_POSITION),
                value: Some(json!(100)),
                ev: None,
            })
            .collect::<Vec<_>>();
        let mut subscriptions = Subscriptions::default();

        let outcome = app
            .write_characteristics(writes, &mut subscriptions)
            .await
            .unwrap();

        assert!(outcome.all_success());
        assert_eq!(
            remote_control.operations(),
            vec![crate::driver::ProtocolOperation::FakeCommand {
                channel: Channel::ALL,
                command: Command::Up,
            }]
        );
    }

    #[test]
    fn external_position_broadcast_produces_hap_events() {
        let events = position_events(2, 0);

        assert_eq!(events.len(), 3);
        assert!(events
            .iter()
            .any(|e| e.id == CharacteristicId::new(2, IID_CURRENT_POSITION)));
    }

    fn pending_target(aid: u64, snapped: u8) -> PendingTargetWrite {
        PendingTargetWrite {
            index: 0,
            id: CharacteristicId::new(aid, IID_TARGET_POSITION),
            blind: find_blind(aid).unwrap(),
            snapped,
        }
    }
}
