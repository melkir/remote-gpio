use serde_json::{json, Value};

use crate::hap::runtime::{CharacteristicId, HapStatus};
use crate::homekit::accessory_db::{
    BLIND_MODEL, BRIDGE_AID, BRIDGE_MODEL, BRIDGE_NAME, BRIDGE_SERIAL, BRIDGE_VERSION, FIRMWARE,
    IID_BRIDGE_VERSION, IID_CURRENT_POSITION, IID_FIRMWARE, IID_IDENTIFY, IID_MANUFACTURER,
    IID_MODEL, IID_NAME, IID_POSITION_STATE, IID_SERIAL, IID_TARGET_POSITION, MANUFACTURER,
};
use crate::positioning::state::{find_blind, position_for_aid, Blind, BlindPosition};

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
            Self::Bridge(characteristic) => read_bridge(characteristic),
            Self::Blind {
                blind,
                characteristic,
            } => read_blind(blind, characteristic, positions),
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

fn read_bridge(characteristic: BridgeCharacteristic) -> Result<Value, HapStatus> {
    match characteristic {
        BridgeCharacteristic::Identify => Err(HapStatus::WriteOnly),
        BridgeCharacteristic::Manufacturer => Ok(json!(MANUFACTURER)),
        BridgeCharacteristic::Model => Ok(json!(BRIDGE_MODEL)),
        BridgeCharacteristic::Name => Ok(json!(BRIDGE_NAME)),
        BridgeCharacteristic::Serial => Ok(json!(BRIDGE_SERIAL)),
        BridgeCharacteristic::Firmware => Ok(json!(FIRMWARE)),
        BridgeCharacteristic::BridgeVersion => Ok(json!(BRIDGE_VERSION)),
    }
}

fn read_blind(
    blind: &Blind,
    characteristic: BlindCharacteristic,
    positions: &[BlindPosition],
) -> Result<Value, HapStatus> {
    let position = || position_for_aid(positions, blind.aid);
    match characteristic {
        BlindCharacteristic::Identify => Err(HapStatus::WriteOnly),
        BlindCharacteristic::Manufacturer => Ok(json!(MANUFACTURER)),
        BlindCharacteristic::Model => Ok(json!(BLIND_MODEL)),
        BlindCharacteristic::Name => Ok(json!(blind.name)),
        BlindCharacteristic::Serial => Ok(json!(blind.serial)),
        BlindCharacteristic::Firmware => Ok(json!(FIRMWARE)),
        BlindCharacteristic::CurrentPosition => Ok(json!(position().current)),
        BlindCharacteristic::TargetPosition => Ok(json!(position().target)),
        BlindCharacteristic::PositionState => Ok(json!(position().status)),
    }
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
