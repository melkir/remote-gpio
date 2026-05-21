//! HomeKit characteristic read helpers and metadata checks.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::hap::runtime::{CharacteristicId, CharacteristicRead, HapStatus};
use crate::homekit::accessory_db;
use crate::homekit::accessory_db::{
    BlindAccessory, BRIDGE_AID, IID_BRIDGE_VERSION, IID_CURRENT_POSITION, IID_FIRMWARE,
    IID_IDENTIFY, IID_MANUFACTURER, IID_MODEL, IID_NAME, IID_POSITION_STATE, IID_SERIAL,
    IID_TARGET_POSITION, POSITION_STATE_STOPPED,
};
use crate::homekit::blinds::{find_blind, BLINDS};
use crate::homekit::position_cache::effective_position;

pub fn read_characteristic(
    positions: &HashMap<u64, u8>,
    id: CharacteristicId,
) -> CharacteristicRead {
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
            IID_CURRENT_POSITION | IID_TARGET_POSITION => {
                json!(effective_position(positions, aid))
            }
            IID_POSITION_STATE => json!(POSITION_STATE_STOPPED),
            _ => return CharacteristicRead::error(id, HapStatus::ResourceDoesNotExist),
        }
    } else {
        return CharacteristicRead::error(id, HapStatus::ResourceDoesNotExist);
    };
    CharacteristicRead::success(id, value)
}

pub fn write_error_status(id: CharacteristicId) -> HapStatus {
    if is_known_characteristic(id) {
        HapStatus::ReadOnly
    } else {
        HapStatus::ResourceDoesNotExist
    }
}

pub fn is_known_characteristic(id: CharacteristicId) -> bool {
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

pub fn supports_events(id: CharacteristicId) -> bool {
    find_blind(id.aid.0).is_some()
        && matches!(
            id.iid.0,
            IID_CURRENT_POSITION | IID_TARGET_POSITION | IID_POSITION_STATE
        )
}

pub fn build_accessories(positions: &[(u64, u8)]) -> Value {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::homekit::accessory_db::{IID_POSITION_STATE, POSITION_STATE_STOPPED};
    use std::collections::HashMap;

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
}
