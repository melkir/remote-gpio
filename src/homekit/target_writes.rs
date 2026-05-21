//! HomeKit `TargetPosition` write planning and batch coalescing.

use crate::hap::runtime::{
    CharacteristicId, CharacteristicWrite, CharacteristicWriteStatus, HapStatus,
};
use crate::homekit::accessory_db::IID_IDENTIFY;
use crate::homekit::accessory_db::IID_TARGET_POSITION;
use crate::homekit::blinds::{find_blind, Blind, BLINDS};
use crate::homekit::position_cache::SnappedPosition;
use crate::homekit::reads::{is_known_characteristic, supports_events, write_error_status};

#[derive(Copy, Clone, Debug)]
pub struct PendingTargetWrite {
    pub index: usize,
    pub id: CharacteristicId,
    pub blind: &'static Blind,
    pub snapped: SnappedPosition,
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
                    HapStatus::InvalidValueInRequest,
                ));
                continue;
            }
        };
        let blind = find_blind(write.id.aid.0).expect("blind checked above");
        let snapped = SnappedPosition::snap(value);

        targets.push(PendingTargetWrite {
            index,
            id: write.id,
            blind,
            snapped,
        });
    }

    TargetWritePlan { statuses, targets }
}

pub fn grouped_all_target(targets: &[PendingTargetWrite]) -> Option<SnappedPosition> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::homekit::accessory_db::IID_TARGET_POSITION;

    #[test]
    fn full_individual_batch_groups_to_all_blinds() {
        let targets = [2, 3, 4, 5]
            .into_iter()
            .map(|aid| pending_target(aid, SnappedPosition::Open))
            .collect::<Vec<_>>();

        let snapped = grouped_all_target(&targets).unwrap();

        assert_eq!(snapped, SnappedPosition::Open);
    }

    #[test]
    fn partial_individual_batch_does_not_group_to_all_blinds() {
        let targets = [2, 3]
            .into_iter()
            .map(|aid| pending_target(aid, SnappedPosition::Open))
            .collect::<Vec<_>>();

        assert!(grouped_all_target(&targets).is_none());
    }

    #[test]
    fn mixed_direction_batch_does_not_group_to_all_blinds() {
        let targets = vec![
            pending_target(2, SnappedPosition::Open),
            pending_target(3, SnappedPosition::Open),
            pending_target(4, SnappedPosition::Closed),
            pending_target(5, SnappedPosition::Open),
        ];

        assert!(grouped_all_target(&targets).is_none());
    }

    fn pending_target(aid: u64, snapped: SnappedPosition) -> PendingTargetWrite {
        PendingTargetWrite {
            index: 0,
            id: CharacteristicId::new(aid, IID_TARGET_POSITION),
            blind: find_blind(aid).unwrap(),
            snapped,
        }
    }
}
