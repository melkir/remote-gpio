use serde_json::{json, Value};

use crate::hap::runtime::{CharacteristicId, HapStatus};
use crate::homekit::accessory_db::{
    BRIDGE_AID, IID_BRIDGE_VERSION, IID_CURRENT_POSITION, IID_FIRMWARE, IID_IDENTIFY,
    IID_MANUFACTURER, IID_MODEL, IID_NAME, IID_POSITION_STATE, IID_SERIAL, IID_TARGET_POSITION,
};
use crate::positioning::state::{find_blind, Blind, BlindPosition};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum BridgeCharacteristic {
    Identify,
    Manufacturer,
    Model,
    Name,
    Serial,
    Firmware,
    BridgeVersion,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum BlindCharacteristic {
    Identify,
    Manufacturer,
    Model,
    Name,
    Serial,
    Firmware,
    CurrentPosition,
    TargetPosition,
    PositionState,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum HomeKitCharacteristic {
    Bridge(BridgeCharacteristic),
    Blind {
        blind: &'static Blind,
        characteristic: BlindCharacteristic,
    },
}

impl HomeKitCharacteristic {
    pub(crate) fn resolve(id: CharacteristicId) -> Option<Self> {
        let iid = id.iid.0;
        if id.aid.0 == BRIDGE_AID {
            return bridge_characteristic(iid).map(Self::Bridge);
        }

        let blind = find_blind(id.aid.0)?;
        blind_characteristic(iid).map(|characteristic| Self::Blind {
            blind,
            characteristic,
        })
    }

    pub(crate) fn read_value(self, positions: &[BlindPosition]) -> Result<Value, HapStatus> {
        match self {
            Self::Bridge(BridgeCharacteristic::Identify)
            | Self::Blind {
                characteristic: BlindCharacteristic::Identify,
                ..
            } => Err(HapStatus::WriteOnly),
            Self::Bridge(BridgeCharacteristic::Manufacturer) => Ok(json!("Somfy")),
            Self::Bridge(BridgeCharacteristic::Model) => Ok(json!("Telis 4 Bridge")),
            Self::Bridge(BridgeCharacteristic::Name) => Ok(json!("Somfy Bridge")),
            Self::Bridge(BridgeCharacteristic::Serial) => Ok(json!("somfy-bridge")),
            Self::Bridge(BridgeCharacteristic::Firmware) => Ok(json!(env!("CARGO_PKG_VERSION"))),
            Self::Bridge(BridgeCharacteristic::BridgeVersion) => Ok(json!("1.1.0")),
            Self::Blind {
                blind: _,
                characteristic: BlindCharacteristic::Manufacturer,
            } => Ok(json!("Somfy")),
            Self::Blind {
                blind: _,
                characteristic: BlindCharacteristic::Model,
            } => Ok(json!("Telis 4")),
            Self::Blind {
                blind,
                characteristic: BlindCharacteristic::Name,
            } => Ok(json!(blind.name)),
            Self::Blind {
                blind,
                characteristic: BlindCharacteristic::Serial,
            } => Ok(json!(blind.serial)),
            Self::Blind {
                blind: _,
                characteristic: BlindCharacteristic::Firmware,
            } => Ok(json!(env!("CARGO_PKG_VERSION"))),
            Self::Blind {
                blind,
                characteristic: BlindCharacteristic::CurrentPosition,
            } => {
                let pos = position_for_aid(positions, blind.aid);
                Ok(json!(pos.current))
            }
            Self::Blind {
                blind,
                characteristic: BlindCharacteristic::TargetPosition,
            } => {
                let pos = position_for_aid(positions, blind.aid);
                Ok(json!(pos.target))
            }
            Self::Blind {
                blind,
                characteristic: BlindCharacteristic::PositionState,
            } => {
                let pos = position_for_aid(positions, blind.aid);
                Ok(json!(pos.status))
            }
        }
    }

    pub(crate) fn supports_events(self) -> bool {
        matches!(
            self,
            Self::Blind {
                characteristic: BlindCharacteristic::CurrentPosition
                    | BlindCharacteristic::TargetPosition
                    | BlindCharacteristic::PositionState,
                ..
            }
        )
    }

    pub(crate) fn write_error_status(id: CharacteristicId) -> HapStatus {
        if Self::resolve(id).is_some() {
            HapStatus::ReadOnly
        } else {
            HapStatus::ResourceDoesNotExist
        }
    }
}

pub(crate) fn position_for_aid(positions: &[BlindPosition], aid: u64) -> BlindPosition {
    positions
        .iter()
        .copied()
        .find(|position| position.aid == aid)
        .unwrap_or_else(|| BlindPosition::default_for_aid(aid))
}

fn bridge_characteristic(iid: u64) -> Option<BridgeCharacteristic> {
    match iid {
        IID_IDENTIFY => Some(BridgeCharacteristic::Identify),
        IID_MANUFACTURER => Some(BridgeCharacteristic::Manufacturer),
        IID_MODEL => Some(BridgeCharacteristic::Model),
        IID_NAME => Some(BridgeCharacteristic::Name),
        IID_SERIAL => Some(BridgeCharacteristic::Serial),
        IID_FIRMWARE => Some(BridgeCharacteristic::Firmware),
        IID_BRIDGE_VERSION => Some(BridgeCharacteristic::BridgeVersion),
        _ => None,
    }
}

fn blind_characteristic(iid: u64) -> Option<BlindCharacteristic> {
    match iid {
        IID_IDENTIFY => Some(BlindCharacteristic::Identify),
        IID_MANUFACTURER => Some(BlindCharacteristic::Manufacturer),
        IID_MODEL => Some(BlindCharacteristic::Model),
        IID_NAME => Some(BlindCharacteristic::Name),
        IID_SERIAL => Some(BlindCharacteristic::Serial),
        IID_FIRMWARE => Some(BlindCharacteristic::Firmware),
        IID_CURRENT_POSITION => Some(BlindCharacteristic::CurrentPosition),
        IID_TARGET_POSITION => Some(BlindCharacteristic::TargetPosition),
        IID_POSITION_STATE => Some(BlindCharacteristic::PositionState),
        _ => None,
    }
}
