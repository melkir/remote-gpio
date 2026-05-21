use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

const CHANNELS: [(Channel, &str); 4] = [
    (Channel::L1, "L1"),
    (Channel::L2, "L2"),
    (Channel::L3, "L3"),
    (Channel::L4, "L4"),
];

/// Installation target: one LED row (`L1`–`L4`) or the group (`ALL`).
#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Channel {
    L1,
    L2,
    L3,
    L4,
    ALL,
}

impl Channel {
    pub const INDIVIDUALS: [Channel; 4] = [Channel::L1, Channel::L2, Channel::L3, Channel::L4];

    /// Advance the Telis selector one step (L1 → L2 → … → ALL → L1).
    pub fn next(self) -> Self {
        match self {
            Channel::L1 => Channel::L2,
            Channel::L2 => Channel::L3,
            Channel::L3 => Channel::L4,
            Channel::L4 => Channel::ALL,
            Channel::ALL => Channel::L1,
        }
    }
}

impl FromStr for Channel {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ALL" => Ok(Channel::ALL),
            _ => CHANNELS
                .iter()
                .find(|(_, name)| *name == s)
                .map(|(ch, _)| *ch)
                .ok_or_else(|| anyhow::anyhow!("Invalid channel value: {s}")),
        }
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Channel::ALL => write!(f, "ALL"),
            other => CHANNELS
                .iter()
                .find(|(ch, _)| ch == other)
                .map(|(_, name)| write!(f, "{name}"))
                .unwrap_or_else(|| write!(f, "{other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_valid() {
        for (ch, name) in CHANNELS {
            assert_eq!(Channel::from_str(name).unwrap(), ch);
        }
        assert_eq!(Channel::from_str("ALL").unwrap(), Channel::ALL);
    }

    #[test]
    fn from_str_invalid() {
        assert!(Channel::from_str("L5").is_err());
        assert!(Channel::from_str("").is_err());
    }

    #[test]
    fn display_round_trip() {
        for ch in [
            Channel::L1,
            Channel::L2,
            Channel::L3,
            Channel::L4,
            Channel::ALL,
        ] {
            let s = ch.to_string();
            assert_eq!(Channel::from_str(&s).unwrap(), ch);
        }
    }
}
