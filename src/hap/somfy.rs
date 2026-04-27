//! Somfy accessory adapter for the internal HAP runtime.
//!
//! This module owns the HomeKit accessory shape, the persisted blind position
//! cache, and the mapping from HAP TargetPosition writes to remote-control
//! commands. The protocol server calls it only through `HapAccessoryApp`.

use anyhow::anyhow;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

use crate::gpio::Input;
use crate::hap::accessories::{self, Blind, IID_CURRENT_POSITION, IID_TARGET_POSITION};
use crate::hap::positions;
use crate::hap::runtime::{
    CharacteristicEvent, CharacteristicId, CharacteristicValue, CharacteristicWrite,
    HapAccessoryApp, HapFuture, Subscriptions,
};
use crate::remote::{Command, RemoteControl};

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
        mut rx: broadcast::Receiver<(Input, u8)>,
    ) {
        loop {
            match rx.recv().await {
                Ok((led, pos)) => {
                    let changes = self.apply_position_for_input(led, pos).await;
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
        accessories::BLINDS
            .iter()
            .map(|b| (b.aid, effective_position(&positions, b.aid)))
            .collect()
    }

    async fn apply_position_for_input(&self, led: Input, pos: u8) -> Vec<CharacteristicEvent> {
        let Some(blind) = accessories::BLINDS.iter().find(|b| b.led == led) else {
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
            let needs_propagate = matches!(blind.led, Input::ALL)
                && accessories::BLINDS
                    .iter()
                    .filter(|b| !matches!(b.led, Input::ALL))
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
            Ok(accessories::build_accessories(&positions))
        })
    }

    fn read_characteristics<'a>(
        &'a self,
        ids: &'a [CharacteristicId],
    ) -> HapFuture<'a, Vec<CharacteristicValue>> {
        Box::pin(async move {
            let positions = self.positions.lock().await;
            let values = ids
                .iter()
                .map(|id| {
                    let aid = id.aid.0;
                    let iid = id.iid.0;
                    let value = match (aid, iid) {
                        (a, i)
                            if accessories::find_blind(a).is_some()
                                && i == IID_CURRENT_POSITION =>
                        {
                            Value::Number(effective_position(&positions, a).into())
                        }
                        (a, i)
                            if accessories::find_blind(a).is_some() && i == IID_TARGET_POSITION =>
                        {
                            Value::Number(effective_position(&positions, a).into())
                        }
                        (a, i)
                            if accessories::find_blind(a).is_some()
                                && i == accessories::IID_POSITION_STATE =>
                        {
                            Value::Number(2.into())
                        }
                        _ => Value::Null,
                    };
                    CharacteristicValue { id: *id, value }
                })
                .collect();
            Ok(values)
        })
    }

    fn write_characteristics<'a>(
        &'a self,
        writes: Vec<CharacteristicWrite>,
        subscriptions: &'a mut Subscriptions,
    ) -> HapFuture<'a, Vec<CharacteristicEvent>> {
        Box::pin(async move {
            let mut changes = Vec::new();
            for write in writes {
                if let Some(ev) = write.ev {
                    if ev {
                        subscriptions.insert(write.id);
                    } else {
                        subscriptions.remove(&write.id);
                    }
                    continue;
                }

                if write.id.iid.0 != IID_TARGET_POSITION {
                    continue;
                }
                let value = match write.value.and_then(|v| v.as_u64()) {
                    Some(v) => v as u8,
                    None => continue,
                };
                let blind = match accessories::find_blind(write.id.aid.0) {
                    Some(b) => b,
                    None => continue,
                };
                let snapped = snap_position(value);

                let positions = self.positions.lock().await;
                if already_at_target(&positions, blind, snapped) {
                    tracing::debug!(
                        "PUT TargetPosition aid={} value={snapped}: cache hit, no-op",
                        write.id.aid.0
                    );
                    continue;
                }
                drop(positions);

                let command = if snapped == 100 {
                    Command::Up
                } else {
                    Command::Down
                };
                self.remote_control
                    .execute(Some(blind.led), command)
                    .await
                    .map_err(|e| anyhow!(e))?;

                changes.extend(self.apply_position_change(blind, snapped).await);
            }
            Ok(changes)
        })
    }
}

fn position_events(aid: u64, position: u8) -> Vec<CharacteristicEvent> {
    [
        (IID_CURRENT_POSITION, Value::Number(position.into())),
        (IID_TARGET_POSITION, Value::Number(position.into())),
        (accessories::IID_POSITION_STATE, Value::Number(2.into())),
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
    if matches!(blind.led, Input::ALL) {
        accessories::BLINDS
            .iter()
            .filter(|b| !matches!(b.led, Input::ALL))
            .all(|b| positions.get(&b.aid).copied() == Some(snapped))
    } else {
        positions.get(&blind.aid).copied() == Some(snapped)
    }
}

fn propagate_positions(positions: &mut HashMap<u64, u8>, changed: &Blind, snapped: u8) {
    if matches!(changed.led, Input::ALL) {
        for b in accessories::BLINDS
            .iter()
            .filter(|b| !matches!(b.led, Input::ALL))
        {
            positions.insert(b.aid, snapped);
        }
        return;
    }
    let individuals: Vec<&Blind> = accessories::BLINDS
        .iter()
        .filter(|b| !matches!(b.led, Input::ALL))
        .collect();
    let all_match = individuals
        .iter()
        .all(|b| positions.get(&b.aid).copied() == Some(snapped));
    let all_blind = accessories::BLINDS
        .iter()
        .find(|b| matches!(b.led, Input::ALL));
    if let Some(all_blind) = all_blind {
        if all_match {
            positions.insert(all_blind.aid, snapped);
        }
    }
}

fn effective_position(positions: &HashMap<u64, u8>, aid: u64) -> u8 {
    let Some(blind) = accessories::find_blind(aid) else {
        return 100;
    };
    if !matches!(blind.led, Input::ALL) {
        return positions.get(&aid).copied().unwrap_or(100);
    }

    let mut individual_positions = accessories::BLINDS
        .iter()
        .filter(|b| !matches!(b.led, Input::ALL))
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

#[cfg(test)]
mod tests {
    use super::*;

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

        let changed = accessories::find_blind(2).unwrap();
        positions.insert(2, 100);
        propagate_positions(&mut positions, changed, 100);
        assert_eq!(positions.get(&6), Some(&0));

        let all_blind = accessories::find_blind(6).unwrap();
        assert!(matches!(all_blind.led, Input::ALL));
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
        let all_blind = accessories::find_blind(6).unwrap();
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

        let changed = accessories::find_blind(2).unwrap();
        positions.insert(2, 0);
        propagate_positions(&mut positions, changed, 0);

        assert_eq!(positions.get(&6), Some(&0));
    }

    #[test]
    fn repeated_target_is_noop() {
        let mut positions = HashMap::new();
        positions.insert(2, 100);
        let blind = accessories::find_blind(2).unwrap();

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
