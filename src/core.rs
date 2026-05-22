//! Domain vocabulary shared across drivers, transports, and HomeKit.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt;
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
    #[serde(rename = "ALL")]
    All,
}

impl Channel {
    pub const INDIVIDUALS: [Channel; 4] = [Channel::L1, Channel::L2, Channel::L3, Channel::L4];

    /// Advance the Telis selector one step (L1 → L2 → … → ALL → L1).
    pub fn next(self) -> Self {
        match self {
            Channel::L1 => Channel::L2,
            Channel::L2 => Channel::L3,
            Channel::L3 => Channel::L4,
            Channel::L4 => Channel::All,
            Channel::All => Channel::L1,
        }
    }
}

impl FromStr for Channel {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ALL" => Ok(Channel::All),
            _ => CHANNELS
                .iter()
                .find(|(_, name)| *name == s)
                .map(|(ch, _)| *ch)
                .ok_or_else(|| anyhow::anyhow!("Invalid channel value: {s}")),
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Channel::L1 => write!(f, "L1"),
            Channel::L2 => write!(f, "L2"),
            Channel::L3 => write!(f, "L3"),
            Channel::L4 => write!(f, "L4"),
            Channel::All => write!(f, "ALL"),
        }
    }
}

/// Press the wired remote or transmit an RTS frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Up,
    Down,
    Stop,
    Select,
    Prog,
    ProgLong,
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Command::Up => "up",
            Command::Down => "down",
            Command::Stop => "stop",
            Command::Select => "select",
            Command::Prog => "prog",
            Command::ProgLong => "prog_long",
        })
    }
}

impl FromStr for Command {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "up" => Ok(Command::Up),
            "down" => Ok(Command::Down),
            "stop" => Ok(Command::Stop),
            "select" => Ok(Command::Select),
            "prog" => Ok(Command::Prog),
            "prog_long" => Ok(Command::ProgLong),
            _ => Err(anyhow::anyhow!("Invalid command: {s}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_from_str_valid() {
        for (ch, name) in CHANNELS {
            assert_eq!(Channel::from_str(name).unwrap(), ch);
        }
        assert_eq!(Channel::from_str("ALL").unwrap(), Channel::All);
    }

    #[test]
    fn channel_from_str_invalid() {
        assert!(Channel::from_str("L5").is_err());
        assert!(Channel::from_str("").is_err());
    }

    #[test]
    fn channel_display_round_trip() {
        for ch in [
            Channel::L1,
            Channel::L2,
            Channel::L3,
            Channel::L4,
            Channel::All,
        ] {
            let s = ch.to_string();
            assert_eq!(Channel::from_str(&s).unwrap(), ch);
        }
    }

    #[test]
    fn channel_serde_preserves_all_spelling() {
        assert_eq!(serde_json::to_string(&Channel::All).unwrap(), r#""ALL""#);
        assert_eq!(
            serde_json::from_str::<Channel>(r#""ALL""#).unwrap(),
            Channel::All
        );
    }

    #[test]
    fn command_from_str_valid() {
        assert_eq!(Command::from_str("up").unwrap(), Command::Up);
        assert_eq!(Command::from_str("down").unwrap(), Command::Down);
        assert_eq!(Command::from_str("stop").unwrap(), Command::Stop);
        assert_eq!(Command::from_str("select").unwrap(), Command::Select);
        assert_eq!(Command::from_str("prog").unwrap(), Command::Prog);
        assert_eq!(Command::from_str("prog_long").unwrap(), Command::ProgLong);
    }

    #[test]
    fn command_from_str_invalid() {
        assert!(Command::from_str("UP").is_err());
        assert!(Command::from_str("toggle").is_err());
        assert!(Command::from_str("").is_err());
    }

    #[test]
    fn command_display_round_trip() {
        for cmd in [
            Command::Up,
            Command::Down,
            Command::Stop,
            Command::Select,
            Command::Prog,
            Command::ProgLong,
        ] {
            let s = cmd.to_string();
            assert_eq!(Command::from_str(&s).unwrap(), cmd);
        }
    }
}
