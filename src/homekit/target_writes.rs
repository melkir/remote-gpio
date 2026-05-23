//! HomeKit `TargetPosition` write planning and batch coalescing.

use crate::hap::runtime::{
    CharacteristicId, CharacteristicWrite, CharacteristicWriteStatus, HapStatus,
};
use crate::homekit::accessory_db::IID_IDENTIFY;
use crate::homekit::accessory_db::IID_TARGET_POSITION;
use crate::homekit::accessory_db::{
    BRIDGE_AID, IID_BRIDGE_VERSION, IID_CURRENT_POSITION, IID_FIRMWARE, IID_MANUFACTURER,
    IID_MODEL, IID_NAME, IID_POSITION_STATE, IID_SERIAL,
};
use crate::positioning::state::{find_blind, Blind};

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
    subscriptions: &mut crate::hap::runtime::Subscriptions,
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

        if write.id.iid.0 == IID_IDENTIFY && is_known_characteristic(write.id) {
            statuses[index] = Some(CharacteristicWriteStatus::success(write.id));
            continue;
        }

        let Some(blind) = find_blind(write.id.aid.0) else {
            statuses[index] = Some(CharacteristicWriteStatus::error(
                write.id,
                write_error_status(write.id),
            ));
            continue;
        };

        if write.id.iid.0 != IID_TARGET_POSITION {
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
    subscriptions: &mut crate::hap::runtime::Subscriptions,
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

#[cfg(test)]
mod tests {
    use super::*;
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
        let mut subscriptions = crate::hap::runtime::Subscriptions::default();
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
        let mut subscriptions = crate::hap::runtime::Subscriptions::default();

        let plan = plan_target_writes(writes, &mut subscriptions);

        assert!(plan.targets.is_empty());
        assert!(subscriptions.contains(&id));
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
}
