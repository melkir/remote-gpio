//! Somfy HomeKit accessory adapter.
//!
//! This module maps HomeKit characteristics onto the shared blind controller.

use serde_json::Value;
use std::sync::Arc;

use crate::controller::BlindController;
use crate::hap::runtime::{
    CharacteristicEvent, CharacteristicId, CharacteristicRead, CharacteristicWrite,
    CharacteristicWriteOutcome, CharacteristicWriteStatus, HapAccessoryApp, HapFuture, HapStatus,
    Subscriptions,
};
use crate::homekit::accessory_db::{
    self, BlindAccessory, BRIDGE_AID, IID_BRIDGE_VERSION, IID_CURRENT_POSITION, IID_FIRMWARE,
    IID_IDENTIFY, IID_MANUFACTURER, IID_MODEL, IID_NAME, IID_POSITION_STATE, IID_SERIAL,
    IID_TARGET_POSITION,
};
use crate::homekit::target_writes::{plan_target_writes, PendingTargetWrite};
use crate::positioning::state::{find_blind, BlindPosition, PositionDelta, BLINDS};

pub struct SomfyHapApp {
    controller: Arc<BlindController>,
}

impl SomfyHapApp {
    pub fn new(controller: Arc<BlindController>) -> Self {
        Self { controller }
    }

    async fn execute_targets(&self, targets: &[PendingTargetWrite]) -> anyhow::Result<()> {
        self.controller
            .set_target_positions(
                targets
                    .iter()
                    .map(|target| (target.blind.aid, target.target))
                    .collect(),
            )
            .await
            .map(|_| ())
    }
}

/// Map controller position deltas to HAP characteristic events for EVENT push.
pub(crate) fn position_characteristic_events(deltas: &[PositionDelta]) -> Vec<CharacteristicEvent> {
    let mut events = Vec::new();
    for delta in deltas {
        if let Some(current) = delta.current {
            events.push(CharacteristicEvent {
                id: CharacteristicId::new(delta.aid, IID_CURRENT_POSITION),
                value: serde_json::json!(current),
            });
        }
        if let Some(target) = delta.target {
            events.push(CharacteristicEvent {
                id: CharacteristicId::new(delta.aid, IID_TARGET_POSITION),
                value: serde_json::json!(target),
            });
        }
        if let Some(status) = delta.status {
            events.push(CharacteristicEvent {
                id: CharacteristicId::new(delta.aid, IID_POSITION_STATE),
                value: serde_json::json!(status),
            });
        }
    }
    events
}

impl HapAccessoryApp for SomfyHapApp {
    fn accessories(&self) -> HapFuture<'_, Value> {
        Box::pin(async move {
            let positions = self.controller.position_snapshot().await;
            Ok(build_accessories(&positions))
        })
    }

    fn read_characteristics<'a>(
        &'a self,
        ids: &'a [CharacteristicId],
    ) -> HapFuture<'a, Vec<CharacteristicRead>> {
        Box::pin(async move {
            let positions = self.controller.position_snapshot().await;
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
            let plan = plan_target_writes(writes, subscriptions);
            let mut outcome = CharacteristicWriteOutcome::default();
            let mut statuses = plan.statuses;

            // Position EVENT push is via the position bridge (see `homekit::start`).
            self.execute_targets(&plan.targets).await?;
            for target in plan.targets {
                statuses[target.index] = Some(CharacteristicWriteStatus::success(target.id));
            }
            outcome.statuses = statuses.into_iter().flatten().collect();
            Ok(outcome)
        })
    }
}

fn read_characteristic(positions: &[BlindPosition], id: CharacteristicId) -> CharacteristicRead {
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
        let pos = position_for_aid(positions, aid);
        match iid {
            IID_IDENTIFY => return CharacteristicRead::error(id, HapStatus::WriteOnly),
            IID_MANUFACTURER => serde_json::json!("Somfy"),
            IID_MODEL => serde_json::json!("Telis 4"),
            IID_NAME => serde_json::json!(blind.name),
            IID_SERIAL => serde_json::json!(blind.serial),
            IID_FIRMWARE => serde_json::json!(env!("CARGO_PKG_VERSION")),
            IID_CURRENT_POSITION => serde_json::json!(pos.current),
            IID_TARGET_POSITION => serde_json::json!(pos.target),
            IID_POSITION_STATE => serde_json::json!(pos.status),
            _ => return CharacteristicRead::error(id, HapStatus::ResourceDoesNotExist),
        }
    } else {
        return CharacteristicRead::error(id, HapStatus::ResourceDoesNotExist);
    };
    CharacteristicRead::success(id, value)
}

fn position_for_aid(positions: &[BlindPosition], aid: u64) -> BlindPosition {
    positions
        .iter()
        .copied()
        .find(|position| position.aid == aid)
        .unwrap_or_else(|| BlindPosition::default_for_aid(aid))
}

fn build_accessories(positions: &[BlindPosition]) -> Value {
    let blinds: Vec<BlindAccessory<'_>> = BLINDS
        .iter()
        .map(|blind| BlindAccessory {
            aid: blind.aid,
            name: blind.name,
            serial: blind.serial,
            position: position_for_aid(positions, blind.aid).current,
        })
        .collect();
    accessory_db::build_accessories(&blinds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Channel, Command};
    use crate::driver::ProtocolOperation;
    use crate::positioning::state::STATUS_STOPPED;
    use crate::testing::fixtures::fake_four_blinds;
    use serde_json::json;
    use tokio::time::Duration;

    async fn wait_for_current(app: &SomfyHapApp, aid: u64, expected: u8) {
        for _ in 0..20 {
            if app.controller.position_for_aid(aid).await.current == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(app.controller.position_for_aid(aid).await.current, expected);
    }

    #[test]
    fn read_position_returns_cached_value() {
        let positions = [BlindPosition {
            aid: 2,
            current: 0,
            target: 0,
            status: STATUS_STOPPED,
        }];

        let read = read_characteristic(&positions, CharacteristicId::new(2, IID_CURRENT_POSITION));

        assert_eq!(read.status, HapStatus::Success);
        assert_eq!(read.value, Some(json!(0)));
    }

    #[test]
    fn accessories_expose_four_blinds() {
        let body = build_accessories(&[
            BlindPosition {
                aid: 2,
                current: 100,
                target: 100,
                status: STATUS_STOPPED,
            },
            BlindPosition {
                aid: 3,
                current: 100,
                target: 100,
                status: STATUS_STOPPED,
            },
            BlindPosition {
                aid: 4,
                current: 100,
                target: 100,
                status: STATUS_STOPPED,
            },
            BlindPosition {
                aid: 5,
                current: 100,
                target: 100,
                status: STATUS_STOPPED,
            },
        ]);
        let aids = body["accessories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|accessory| accessory["aid"].as_u64().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(aids, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn target_position_starts_motion_and_stops_after_timed_percentage() {
        let controller = fake_four_blinds(2).await;
        let app = SomfyHapApp::new(controller.clone());
        let writes = vec![CharacteristicWrite {
            id: CharacteristicId::new(2, IID_TARGET_POSITION),
            value: Some(json!(50)),
            ev: None,
        }];
        let mut subscriptions = Subscriptions::default();

        let outcome = app
            .write_characteristics(writes, &mut subscriptions)
            .await
            .unwrap();

        assert!(outcome.all_success());
        assert_eq!(
            controller.operations(),
            vec![ProtocolOperation::FakeCommand {
                channel: Channel::L1,
                command: Command::Down,
            }]
        );
        assert_eq!(app.controller.position_for_aid(2).await.current, 100);
        wait_for_current(&app, 2, 50).await;
        assert_eq!(
            controller.operations(),
            vec![
                ProtocolOperation::FakeCommand {
                    channel: Channel::L1,
                    command: Command::Down,
                },
                ProtocolOperation::FakeCommand {
                    channel: Channel::L1,
                    command: Command::Stop,
                },
            ]
        );
        assert_eq!(app.controller.position_for_aid(2).await.current, 50);
    }

    #[tokio::test]
    async fn target_position_write_leaves_outcome_events_empty() {
        let controller = fake_four_blinds(10).await;
        let mut position_rx = controller.subscribe_positions();
        let app = SomfyHapApp::new(controller);
        let mut subscriptions = Subscriptions::default();

        let outcome = app
            .write_characteristics(
                vec![CharacteristicWrite {
                    id: CharacteristicId::new(2, IID_TARGET_POSITION),
                    value: Some(json!(50)),
                    ev: None,
                }],
                &mut subscriptions,
            )
            .await
            .unwrap();

        assert!(outcome.events.is_empty());
        assert!(outcome.all_success());
        let published = position_rx.recv().await.unwrap();
        assert!(!published.is_empty());
        assert_eq!(published[0].target, Some(50));
    }

    #[tokio::test]
    async fn full_individual_write_batch_sends_one_all_start_command() {
        let controller = fake_four_blinds(10).await;
        let app = SomfyHapApp::new(controller.clone());
        let writes = [2, 3, 4, 5]
            .into_iter()
            .map(|aid| CharacteristicWrite {
                id: CharacteristicId::new(aid, IID_TARGET_POSITION),
                value: Some(json!(50)),
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
            vec![ProtocolOperation::FakeCommand {
                channel: Channel::All,
                command: Command::Down,
            }]
        );
    }

    #[tokio::test]
    async fn endpoint_target_does_not_send_timed_stop() {
        let controller = fake_four_blinds(2).await;
        let app = SomfyHapApp::new(controller.clone());
        let mut subscriptions = Subscriptions::default();

        app.write_characteristics(
            vec![CharacteristicWrite {
                id: CharacteristicId::new(2, IID_TARGET_POSITION),
                value: Some(json!(0)),
                ev: None,
            }],
            &mut subscriptions,
        )
        .await
        .unwrap();
        wait_for_current(&app, 2, 0).await;

        assert_eq!(
            controller.operations(),
            vec![ProtocolOperation::FakeCommand {
                channel: Channel::L1,
                command: Command::Down,
            }]
        );
        assert_eq!(app.controller.position_for_aid(2).await.current, 0);
    }

    #[tokio::test]
    async fn writing_current_position_cancels_pending_motion() {
        let controller = fake_four_blinds(20).await;
        let app = SomfyHapApp::new(controller.clone());
        let mut subscriptions = Subscriptions::default();

        app.write_characteristics(
            vec![CharacteristicWrite {
                id: CharacteristicId::new(2, IID_TARGET_POSITION),
                value: Some(json!(0)),
                ev: None,
            }],
            &mut subscriptions,
        )
        .await
        .unwrap();
        app.write_characteristics(
            vec![CharacteristicWrite {
                id: CharacteristicId::new(2, IID_TARGET_POSITION),
                value: Some(json!(100)),
                ev: None,
            }],
            &mut subscriptions,
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;

        assert_eq!(
            controller.operations(),
            vec![
                ProtocolOperation::FakeCommand {
                    channel: Channel::L1,
                    command: Command::Down,
                },
                ProtocolOperation::FakeCommand {
                    channel: Channel::L1,
                    command: Command::Stop,
                },
            ]
        );
        let pos = app.controller.position_for_aid(2).await;
        assert_eq!(pos.current, 100);
        assert_eq!(pos.target, 100);
    }
}
