use anyhow::{bail, Result};

use crate::remote::Command;

pub const FRAME_LEN: usize = 7;
pub const DEFAULT_KEY: u8 = 0xA0;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RtsCommand {
    My,
    Up,
    Down,
    Prog,
}

impl RtsCommand {
    pub fn code(self) -> u8 {
        match self {
            Self::My => 0x1,
            Self::Up => 0x2,
            Self::Down => 0x4,
            Self::Prog => 0x8,
        }
    }
}

impl TryFrom<Command> for RtsCommand {
    type Error = anyhow::Error;

    fn try_from(command: Command) -> Result<Self> {
        match command {
            Command::My | Command::Stop => Ok(Self::My),
            Command::Up => Ok(Self::Up),
            Command::Down => Ok(Self::Down),
            Command::Prog => Ok(Self::Prog),
            Command::Select => bail!("select is not an RTS radio command"),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RtsFrame {
    bytes: [u8; FRAME_LEN],
}

impl RtsFrame {
    pub fn encode(command: RtsCommand, rolling_code: u16, remote_id: u32) -> Result<Self> {
        if remote_id == 0 || remote_id > 0xFF_FFFF {
            bail!("remote_id must be a non-zero 24-bit value");
        }

        let mut bytes = [
            DEFAULT_KEY,
            command.code() << 4,
            (rolling_code >> 8) as u8,
            rolling_code as u8,
            remote_id as u8,
            (remote_id >> 8) as u8,
            (remote_id >> 16) as u8,
        ];
        bytes[1] |= checksum(bytes);
        obfuscate(&mut bytes);
        Ok(Self { bytes })
    }

    pub fn bytes(self) -> [u8; FRAME_LEN] {
        self.bytes
    }
}

pub fn checksum(bytes: [u8; FRAME_LEN]) -> u8 {
    bytes.iter().fold(0u8, |acc, byte| acc ^ byte ^ (byte >> 4)) & 0x0F
}

pub fn obfuscate(bytes: &mut [u8; FRAME_LEN]) {
    for i in 1..FRAME_LEN {
        bytes[i] ^= bytes[i - 1];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_command_codes() {
        assert_eq!(RtsCommand::My.code(), 0x1);
        assert_eq!(RtsCommand::Up.code(), 0x2);
        assert_eq!(RtsCommand::Down.code(), 0x4);
        assert_eq!(RtsCommand::Prog.code(), 0x8);
        assert_eq!(RtsCommand::try_from(Command::Stop).unwrap(), RtsCommand::My);
        assert!(RtsCommand::try_from(Command::Select).is_err());
    }

    #[test]
    fn calculates_checksum_from_unobfuscated_nibbles() {
        let bytes = [0xA0, 0x20, 0x00, 0xA7, 0x56, 0x34, 0x12];
        assert_eq!(checksum(bytes), 0x2);
    }

    #[test]
    fn obfuscates_in_place_using_previous_obfuscated_byte() {
        let mut bytes = [0xA0, 0x22, 0x00, 0xA7, 0x56, 0x34, 0x12];
        obfuscate(&mut bytes);
        assert_eq!(bytes, [0xA0, 0x82, 0x82, 0x25, 0x73, 0x47, 0x55]);
    }

    #[test]
    fn encodes_frame_byte_order_checksum_and_obfuscation() {
        let frame = RtsFrame::encode(RtsCommand::Up, 0x00A7, 0x123456).unwrap();
        assert_eq!(frame.bytes(), [0xA0, 0x82, 0x82, 0x25, 0x73, 0x47, 0x55]);
    }

    #[test]
    fn rejects_invalid_remote_ids() {
        assert!(RtsFrame::encode(RtsCommand::Up, 1, 0).is_err());
        assert!(RtsFrame::encode(RtsCommand::Up, 1, 0x01_00_00_00).is_err());
    }
}
