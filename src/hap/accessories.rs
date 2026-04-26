//! HAP accessory database. We expose a Bridge (aid=1) with 5 bridged
//! WindowCovering accessories (aid=2..6) — one per Somfy LED selector.
//!
//! IIDs are stable: the same characteristic always has the same iid across
//! runs because controllers cache the schema. Don't renumber without bumping
//! `config_number` in HapState.

use crate::gpio::Input;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Copy, Clone, Debug)]
pub struct Blind {
    pub aid: u64,
    pub name: &'static str,
    pub led: Input,
    pub serial: &'static str,
}

pub const BRIDGE_AID: u64 = 1;

pub const BLINDS: &[Blind] = &[
    Blind {
        aid: 2,
        name: "Blind 1",
        led: Input::L1,
        serial: "somfy-L1",
    },
    Blind {
        aid: 3,
        name: "Blind 2",
        led: Input::L2,
        serial: "somfy-L2",
    },
    Blind {
        aid: 4,
        name: "Blind 3",
        led: Input::L3,
        serial: "somfy-L3",
    },
    Blind {
        aid: 5,
        name: "Blind 4",
        led: Input::L4,
        serial: "somfy-L4",
    },
    Blind {
        aid: 6,
        name: "All Blinds",
        led: Input::ALL,
        serial: "somfy-ALL",
    },
];

// Per-blind characteristic IIDs. AccessoryInformation iids are 1..7,
// WindowCovering iids are 8..12.
pub const IID_AINFO_SERVICE: u64 = 1;
pub const IID_IDENTIFY: u64 = 2;
pub const IID_MANUFACTURER: u64 = 3;
pub const IID_MODEL: u64 = 4;
pub const IID_NAME: u64 = 5;
pub const IID_SERIAL: u64 = 6;
pub const IID_FIRMWARE: u64 = 7;
pub const IID_WC_SERVICE: u64 = 8;
pub const IID_CURRENT_POSITION: u64 = 9;
pub const IID_TARGET_POSITION: u64 = 10;
pub const IID_POSITION_STATE: u64 = 11;

// Bridge-only iids. These reuse 8/9 because HAP iids are scoped per-aid;
// the Bridge (aid=1) and the WindowCovering accessories (aid=2..6) live in
// separate namespaces, so there's no collision.
pub const IID_BRIDGE_PROTO_SERVICE: u64 = 8;
pub const IID_BRIDGE_VERSION: u64 = 9;

pub fn find_blind(aid: u64) -> Option<&'static Blind> {
    BLINDS.iter().find(|b| b.aid == aid)
}

#[derive(Clone, Debug, Serialize)]
pub struct CharRead {
    pub aid: u64,
    pub iid: u64,
    pub value: Value,
}

/// Build the full /accessories response. Positions snapshot from the runtime
/// position cache (aid → 0/100).
pub fn build_accessories(positions: &[(u64, u8)]) -> Value {
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
                    char_uint8(IID_CURRENT_POSITION, "6D", position, &["pr", "ev"]),
                    char_uint8(IID_TARGET_POSITION, "7C", position, &["pr", "pw", "ev"]),
                    char_uint8(IID_POSITION_STATE, "72", 2, &["pr", "ev"]),
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

fn char_uint8(iid: u64, type_: &str, value: u8, perms: &[&str]) -> Value {
    json!({
        "iid": iid,
        "type": type_,
        "perms": perms,
        "format": "uint8",
        "value": value,
        "minValue": 0,
        "maxValue": 100,
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
