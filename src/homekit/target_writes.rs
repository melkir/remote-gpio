//! HomeKit `TargetPosition` write planning and batch coalescing.

use crate::hap::runtime::{
    CharacteristicId, CharacteristicWrite, CharacteristicWriteStatus, HapStatus, Subscriptions,
};
use crate::homekit::characteristic::{
    BlindCharacteristic, BridgeCharacteristic, HomeKitCharacteristic,
};
use crate::positioning::state::Blind;

#[derive(Copy, Clone, Debug)]
pub struct PendingTargetWrite {
    pub index: usize,
    pub id: CharacteristicId,
    pub blind: &'static Blind,
    pub target: u8,
}

pub struct TargetWritePlan {
    pub statuses: Vec<Option<CharacteristicWriteStatus>>,
    pub targets: Vec<PendingTargetWrite>,
}

pub fn plan_target_writes(
    writes: Vec<CharacteristicWrite>,
    subscriptions: &mut Subscriptions,
) -> TargetWritePlan {
    let mut statuses = Vec::new();
    let mut targets = Vec::new();

    for write in writes {
        let index = statuses.len();
        statuses.push(None);

        if let Some(ev) = write.ev {
            statuses[index] = Some(handle_subscription(write.id, ev, subscriptions));
            continue;
        }

        let Some(characteristic) = HomeKitCharacteristic::resolve(write.id) else {
            statuses[index] = Some(CharacteristicWriteStatus::error(
                write.id,
                HapStatus::ResourceDoesNotExist,
            ));
            continue;
        };

        if matches!(
            characteristic,
            HomeKitCharacteristic::Bridge(BridgeCharacteristic::Identify)
                | HomeKitCharacteristic::Blind {
                    characteristic: BlindCharacteristic::Identify,
                    ..
                }
        ) {
            statuses[index] = Some(CharacteristicWriteStatus::success(write.id));
            continue;
        };

        let HomeKitCharacteristic::Blind {
            blind,
            characteristic: BlindCharacteristic::TargetPosition,
        } = characteristic
        else {
            statuses[index] = Some(CharacteristicWriteStatus::error(
                write.id,
                HomeKitCharacteristic::write_error_status(write.id),
            ));
            continue;
        };

        let value = match write.value.and_then(|v| v.as_u64()) {
            Some(v) if v <= 100 => v as u8,
            _ => {
                statuses[index] = Some(CharacteristicWriteStatus::error(
                    write.id,
                    HapStatus::InvalidValueInRequest,
                ));
                continue;
            }
        };
        targets.push(PendingTargetWrite {
            index,
            id: write.id,
            blind,
            target: value,
        });
    }

    TargetWritePlan { statuses, targets }
}

fn handle_subscription(
    id: CharacteristicId,
    enabled: bool,
    subscriptions: &mut Subscriptions,
) -> CharacteristicWriteStatus {
    let Some(characteristic) = HomeKitCharacteristic::resolve(id) else {
        return CharacteristicWriteStatus::error(id, HapStatus::ResourceDoesNotExist);
    };
    if !characteristic.supports_events() {
        return CharacteristicWriteStatus::error(id, HapStatus::NotificationNotSupported);
    }
    if enabled {
        subscriptions.insert(id);
    } else {
        subscriptions.remove(&id);
    }
    CharacteristicWriteStatus::success(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::homekit::accessory_db::{IID_CURRENT_POSITION, IID_TARGET_POSITION};
    use serde_json::json;

    #[test]
    fn batch_target_writes_plan_four_blinds() {
        let writes = [2, 3, 4, 5]
            .into_iter()
            .map(|aid| CharacteristicWrite {
                id: CharacteristicId::new(aid, IID_TARGET_POSITION),
                value: Some(json!(50)),
                ev: None,
            })
            .collect::<Vec<_>>();
        let mut subscriptions = Subscriptions::default();
        let plan = plan_target_writes(writes, &mut subscriptions);

        assert_eq!(plan.targets.len(), 4);
        assert_eq!(
            plan.targets
                .iter()
                .map(|target| target.blind.aid)
                .collect::<Vec<_>>(),
            vec![2, 3, 4, 5]
        );
        assert!(plan
            .targets
            .iter()
            .all(|target| target.target == 50 && target.id.iid.0 == IID_TARGET_POSITION));
        assert_eq!(plan.statuses.len(), 4);
        assert!(subscriptions.is_empty());
    }

    #[test]
    fn subscription_toggle_does_not_plan_motion() {
        let id = CharacteristicId::new(2, IID_CURRENT_POSITION);
        let writes = vec![CharacteristicWrite {
            id,
            value: None,
            ev: Some(true),
        }];
        let mut subscriptions = Subscriptions::default();

        let plan = plan_target_writes(writes, &mut subscriptions);

        assert!(plan.targets.is_empty());
        assert!(subscriptions.contains(&id));
    }

    #[test]
    fn unsupported_write_reports_protocol_status() {
        assert_eq!(
            HomeKitCharacteristic::write_error_status(CharacteristicId::new(
                2,
                IID_CURRENT_POSITION
            )),
            HapStatus::ReadOnly
        );
        assert_eq!(
            HomeKitCharacteristic::write_error_status(CharacteristicId::new(99, 99)),
            HapStatus::ResourceDoesNotExist
        );
    }
}
