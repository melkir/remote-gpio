//! Somfy HomeKit accessory adapter.
//!
//! This module wires the HAP trait implementation to the shared position cache,
//! target-write planner, and remote-control command router.

use anyhow::anyhow;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::gpio::Channel;
use crate::hap::runtime::{
    CharacteristicEvent, CharacteristicId, CharacteristicRead, CharacteristicWrite,
    CharacteristicWriteOutcome, CharacteristicWriteStatus, HapAccessoryApp, HapFuture,
    Subscriptions,
};
use crate::homekit::position_cache::{PositionCache, SnappedPosition};
use crate::homekit::reads::{build_accessories, read_characteristic};
use crate::homekit::target_writes::{grouped_all_target, plan_target_writes, PendingTargetWrite};
use crate::remote::{PositionUpdate, RemoteControl};

pub struct SomfyHapApp {
    remote_control: Arc<RemoteControl>,
    positions: PositionCache,
}

impl SomfyHapApp {
    pub fn new(remote_control: Arc<RemoteControl>) -> Self {
        Self {
            remote_control,
            positions: PositionCache::new(),
        }
    }

    #[cfg(test)]
    fn new_with_positions(
        remote_control: Arc<RemoteControl>,
        positions: std::collections::HashMap<u64, u8>,
    ) -> Self {
        Self {
            remote_control,
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

        self.remote_control
            .execute_on(Channel::ALL, snapped.command())
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

        self.remote_control
            .execute_on(target.blind.channel, target.snapped.command())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpio::Channel;
    use crate::homekit::accessory_db::IID_TARGET_POSITION;
    use crate::remote::Command;
    use serde_json::json;
    use std::collections::HashMap;

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
}
