//! Domain vocabulary shared across drivers, transports, and HomeKit.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

const CHANNELS: [(Channel, &str); 5] = [
    (Channel::L1, "L1"),
    (Channel::L2, "L2"),
    (Channel::L3, "L3"),
    (Channel::L4, "L4"),
    (Channel::All, "ALL"),
];

const COMMANDS: [(Command, &str); 6] = [
    (Command::Up, "up"),
    (Command::Down, "down"),
    (Command::Stop, "stop"),
    (Command::Select, "select"),
    (Command::Prog, "prog"),
    (Command::ProgLong, "prog_long"),
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

    pub fn individual_index(self) -> Option<usize> {
        match self {
            Channel::L1 => Some(0),
            Channel::L2 => Some(1),
            Channel::L3 => Some(2),
            Channel::L4 => Some(3),
            Channel::All => None,
        }
    }

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
        CHANNELS
            .iter()
            .find(|(_, name)| *name == s)
            .map(|(ch, _)| *ch)
            .ok_or_else(|| anyhow::anyhow!("Invalid channel value: {s}"))
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Channel::L1 => "L1",
            Channel::L2 => "L2",
            Channel::L3 => "L3",
            Channel::L4 => "L4",
            Channel::All => "ALL",
        })
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
        COMMANDS
            .iter()
            .find(|(_, name)| *name == s)
            .map(|(command, _)| *command)
            .ok_or_else(|| anyhow::anyhow!("Invalid command: {s}"))
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
    }

    #[test]
    fn channel_from_str_invalid() {
        assert!(Channel::from_str("L5").is_err());
        assert!(Channel::from_str("").is_err());
    }

    #[test]
    fn channel_display_round_trip() {
        for (ch, _) in CHANNELS {
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
        for (command, name) in COMMANDS {
            assert_eq!(Command::from_str(name).unwrap(), command);
        }
    }

    #[test]
    fn command_from_str_invalid() {
        assert!(Command::from_str("UP").is_err());
        assert!(Command::from_str("toggle").is_err());
        assert!(Command::from_str("").is_err());
    }

    #[test]
    fn command_display_round_trip() {
        for (command, _) in COMMANDS {
            let s = command.to_string();
            assert_eq!(Command::from_str(&s).unwrap(), command);
        }
    }
}
