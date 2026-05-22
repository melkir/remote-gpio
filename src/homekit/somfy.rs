//! Somfy HomeKit accessory adapter.
//!
//! This module wires the HAP trait implementation to the shared position cache,
//! target-write planner, and blind controller.

use anyhow::anyhow;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::controller::{BlindController, PositionUpdate};
use crate::core::{Channel, Command};
use crate::hap::runtime::{
    CharacteristicEvent, CharacteristicId, CharacteristicRead, CharacteristicWrite,
    CharacteristicWriteOutcome, CharacteristicWriteStatus, HapAccessoryApp, HapFuture, HapStatus,
    Subscriptions,
};
use crate::homekit::accessory_db::{
    self, BlindAccessory, BRIDGE_AID, IID_BRIDGE_VERSION, IID_CURRENT_POSITION, IID_FIRMWARE,
    IID_IDENTIFY, IID_MANUFACTURER, IID_MODEL, IID_NAME, IID_POSITION_STATE, IID_SERIAL,
    IID_TARGET_POSITION, POSITION_STATE_STOPPED,
};
use crate::homekit::position_cache::{
    effective_position, find_blind, PositionCache, SnappedPosition, BLINDS,
};
use crate::homekit::target_writes::{grouped_all_target, plan_target_writes, PendingTargetWrite};

pub struct SomfyHapApp {
    controller: Arc<BlindController>,
    positions: PositionCache,
}

impl SomfyHapApp {
    pub fn new(controller: Arc<BlindController>) -> Self {
        Self {
            controller,
            positions: PositionCache::new(),
        }
    }

    #[cfg(test)]
    fn new_with_positions(
        controller: Arc<BlindController>,
        positions: std::collections::HashMap<u64, u8>,
    ) -> Self {
        Self {
            controller,
            positions: PositionCache::from_positions(positions),
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
                        .positions
                        .apply_for_channel(update.channel, update.position)
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

    async fn execute_grouped_all(
        &self,
        snapped: SnappedPosition,
    ) -> Result<Vec<CharacteristicEvent>, anyhow::Error> {
        if self.positions.all_at_target(snapped).await {
            tracing::debug!(
                "PUT TargetPosition grouped value={}: cache hit, no-op",
                snapped.as_u8()
            );
            return Ok(Vec::new());
        }

        self.controller
            .execute_on(Channel::All, command_for_snapped(snapped))
            .await
            .map_err(|e| anyhow!(e))?;

        Ok(self.positions.apply_all(snapped).await)
    }

    async fn execute_target(
        &self,
        target: PendingTargetWrite,
    ) -> Result<Vec<CharacteristicEvent>, anyhow::Error> {
        if self.positions.get(target.blind.aid).await == Some(target.snapped.as_u8()) {
            tracing::debug!(
                "PUT TargetPosition aid={} value={}: cache hit, no-op",
                target.id.aid.0,
                target.snapped.as_u8()
            );
            return Ok(Vec::new());
        }

        self.controller
            .execute_on(target.blind.channel, command_for_snapped(target.snapped))
            .await
            .map_err(|e| anyhow!(e))?;

        Ok(self
            .positions
            .apply_blind(target.blind, target.snapped)
            .await)
    }
}

impl HapAccessoryApp for SomfyHapApp {
    fn accessories(&self) -> HapFuture<'_, Value> {
        Box::pin(async move {
            let positions = self.positions.snapshot().await;
            Ok(build_accessories(&positions))
        })
    }

    fn read_characteristics<'a>(
        &'a self,
        ids: &'a [CharacteristicId],
    ) -> HapFuture<'a, Vec<CharacteristicRead>> {
        Box::pin(async move {
            let values = self
                .positions
                .with_positions(|positions| {
                    ids.iter()
                        .map(|id| read_characteristic(positions, *id))
                        .collect()
                })
                .await;
            Ok(values)
        })
    }

    fn write_characteristics<'a>(
        &'a self,
        writes: Vec<CharacteristicWrite>,
        subscriptions: &'a mut Subscriptions,
    ) -> HapFuture<'a, CharacteristicWriteOutcome> {
        Box::pin(async move {
            let plan = plan_target_writes(writes, subscriptions);
            let mut outcome = CharacteristicWriteOutcome::default();
            let mut statuses = plan.statuses;

            if let Some(snapped) = grouped_all_target(&plan.targets) {
                outcome
                    .events
                    .extend(self.execute_grouped_all(snapped).await?);
                for target in plan.targets {
                    statuses[target.index] = Some(CharacteristicWriteStatus::success(target.id));
                }
                outcome.statuses = statuses.into_iter().flatten().collect();
                return Ok(outcome);
            }

            for target in plan.targets {
                outcome.events.extend(self.execute_target(target).await?);
                statuses[target.index] = Some(CharacteristicWriteStatus::success(target.id));
            }
            outcome.statuses = statuses.into_iter().flatten().collect();
            Ok(outcome)
        })
    }
}

fn read_characteristic(
    positions: &std::collections::HashMap<u64, u8>,
    id: CharacteristicId,
) -> CharacteristicRead {
    let aid = id.aid.0;
    let iid = id.iid.0;
    let value = if aid == BRIDGE_AID {
        match iid {
            IID_IDENTIFY => return CharacteristicRead::error(id, HapStatus::WriteOnly),
            IID_MANUFACTURER => serde_json::json!("Somfy"),
            IID_MODEL => serde_json::json!("Telis 4 Bridge"),
            IID_NAME => serde_json::json!("Somfy Bridge"),
            IID_SERIAL => serde_json::json!("somfy-bridge"),
            IID_FIRMWARE => serde_json::json!(env!("CARGO_PKG_VERSION")),
            IID_BRIDGE_VERSION => serde_json::json!("1.1.0"),
            _ => return CharacteristicRead::error(id, HapStatus::ResourceDoesNotExist),
        }
    } else if let Some(blind) = find_blind(aid) {
        match iid {
            IID_IDENTIFY => return CharacteristicRead::error(id, HapStatus::WriteOnly),
            IID_MANUFACTURER => serde_json::json!("Somfy"),
            IID_MODEL => serde_json::json!("Telis 4"),
            IID_NAME => serde_json::json!(blind.name),
            IID_SERIAL => serde_json::json!(blind.serial),
            IID_FIRMWARE => serde_json::json!(env!("CARGO_PKG_VERSION")),
            IID_CURRENT_POSITION | IID_TARGET_POSITION => {
                serde_json::json!(effective_position(positions, aid))
            }
            IID_POSITION_STATE => serde_json::json!(POSITION_STATE_STOPPED),
            _ => return CharacteristicRead::error(id, HapStatus::ResourceDoesNotExist),
        }
    } else {
        return CharacteristicRead::error(id, HapStatus::ResourceDoesNotExist);
    };
    CharacteristicRead::success(id, value)
}

fn build_accessories(positions: &[(u64, u8)]) -> Value {
    let blinds: Vec<BlindAccessory<'_>> = BLINDS
        .iter()
        .map(|blind| BlindAccessory {
            aid: blind.aid,
            name: blind.name,
            serial: blind.serial,
            position: positions
                .iter()
                .find(|(aid, _)| *aid == blind.aid)
                .map(|(_, pos)| *pos)
                .unwrap_or(100),
        })
        .collect();
    accessory_db::build_accessories(&blinds)
}

fn command_for_snapped(snapped: SnappedPosition) -> Command {
    match snapped {
        SnappedPosition::Open => Command::Up,
        SnappedPosition::Closed => Command::Down,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Channel, Command};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn read_position_returns_cached_value() {
        let mut positions = HashMap::new();
        positions.insert(2, 0);

        let read = read_characteristic(&positions, CharacteristicId::new(2, IID_CURRENT_POSITION));

        assert_eq!(read.status, HapStatus::Success);
        assert_eq!(read.value, Some(json!(0)));
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

    #[tokio::test]
    async fn full_individual_write_batch_sends_one_all_driver_command() {
        let controller = Arc::new(
            BlindController::with_driver(crate::config::DriverConfig::fake())
                .await
                .unwrap(),
        );
        let app = SomfyHapApp::new_with_positions(controller.clone(), HashMap::new());
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
            controller.operations(),
            vec![crate::driver::ProtocolOperation::FakeCommand {
                channel: Channel::All,
                command: Command::Up,
            }]
        );
    }

    #[tokio::test]
    async fn cache_hit_does_not_break_full_batch_coalesce() {
        let controller = Arc::new(
            BlindController::with_driver(crate::config::DriverConfig::fake())
                .await
                .unwrap(),
        );
        let mut positions = HashMap::new();
        positions.insert(2, 100);
        let app = SomfyHapApp::new_with_positions(controller.clone(), positions);
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
            controller.operations(),
            vec![crate::driver::ProtocolOperation::FakeCommand {
                channel: Channel::All,
                command: Command::Up,
            }]
        );
    }
}
