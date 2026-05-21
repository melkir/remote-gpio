use anyhow::Result;
use std::str::FromStr;

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
    fn from_str_valid() {
        assert_eq!(Command::from_str("up").unwrap(), Command::Up);
        assert_eq!(Command::from_str("down").unwrap(), Command::Down);
        assert_eq!(Command::from_str("stop").unwrap(), Command::Stop);
        assert_eq!(Command::from_str("select").unwrap(), Command::Select);
        assert_eq!(Command::from_str("prog").unwrap(), Command::Prog);
    }

    #[test]
    fn from_str_invalid() {
        assert!(Command::from_str("UP").is_err());
        assert!(Command::from_str("toggle").is_err());
        assert!(Command::from_str("").is_err());
    }
}
