//! HAP accessory database JSON for the Somfy bridge and Telis blinds.

use serde_json::{json, Value};

use crate::positioning::state::STATUS_STOPPED;

pub(crate) const BRIDGE_AID: u64 = 1;

pub(crate) const IID_AINFO_SERVICE: u64 = 1;
pub(crate) const IID_IDENTIFY: u64 = 2;
pub(crate) const IID_MANUFACTURER: u64 = 3;
pub(crate) const IID_MODEL: u64 = 4;
pub(crate) const IID_NAME: u64 = 5;
pub(crate) const IID_SERIAL: u64 = 6;
pub(crate) const IID_FIRMWARE: u64 = 7;
pub(crate) const IID_WC_SERVICE: u64 = 8;
pub(crate) const IID_CURRENT_POSITION: u64 = 9;
pub(crate) const IID_TARGET_POSITION: u64 = 10;
pub(crate) const IID_POSITION_STATE: u64 = 11;
pub(crate) const IID_BRIDGE_PROTO_SERVICE: u64 = 8;
pub(crate) const IID_BRIDGE_VERSION: u64 = 9;

pub(crate) struct BlindAccessory<'a> {
    pub aid: u64,
    pub name: &'a str,
    pub serial: &'a str,
    pub position: u8,
}

pub(crate) fn build_accessories(blinds: &[BlindAccessory<'_>]) -> Value {
    let mut accessories = vec![bridge_accessory()];
    for blind in blinds {
        accessories.push(blind_accessory(blind));
    }
    json!({ "accessories": accessories })
}

fn bridge_accessory() -> Value {
    let firmware = env!("CARGO_PKG_VERSION");
    json!({
        "aid": BRIDGE_AID,
        "services": [
            accessory_info_service(
                "Somfy Bridge",
                "Telis 4 Bridge",
                "somfy-bridge",
                firmware,
            ),
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

fn blind_accessory(blind: &BlindAccessory<'_>) -> Value {
    let firmware = env!("CARGO_PKG_VERSION");
    json!({
        "aid": blind.aid,
        "services": [
            accessory_info_service(blind.name, "Telis 4", blind.serial, firmware),
            window_covering_service(blind.position),
        ]
    })
}

fn accessory_info_service(name: &str, model: &str, serial: &str, firmware: &str) -> Value {
    json!({
        "iid": IID_AINFO_SERVICE,
        "type": "3E",
        "characteristics": [
            char_string(IID_MANUFACTURER, "20", "Somfy", &["pr"]),
            char_string(IID_MODEL, "21", model, &["pr"]),
            char_string(IID_NAME, "23", name, &["pr"]),
            char_string(IID_SERIAL, "30", serial, &["pr"]),
            char_string(IID_FIRMWARE, "52", firmware, &["pr"]),
            char_bool_pw(IID_IDENTIFY, "14"),
        ],
    })
}

fn window_covering_service(position: u8) -> Value {
    json!({
        "iid": IID_WC_SERVICE,
        "type": "8C",
        "characteristics": [
            char_uint8(IID_CURRENT_POSITION, "6D", position, &["pr", "ev"], 100),
            char_uint8(IID_TARGET_POSITION, "7C", position, &["pr", "pw", "ev"], 100),
            char_uint8(
                IID_POSITION_STATE,
                "72",
                STATUS_STOPPED,
                &["pr", "ev"],
                2,
            ),
        ],
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
