use anyhow::Result;
use std::str::FromStr;

use tokio::sync::broadcast;

use crate::backend::{infer_position, ActiveBackend, CommandOutcome, SelectedChannelRx};
use crate::gpio::Channel;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PositionUpdate {
    pub channel: Channel,
    pub position: u8,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Up,
    Down,
    My,
    Stop,
    Select,
}

impl FromStr for Command {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "up" => Ok(Command::Up),
            "down" => Ok(Command::Down),
            "my" => Ok(Command::My),
            "stop" => Ok(Command::Stop),
            "select" => Ok(Command::Select),
            _ => Err(anyhow::anyhow!("Invalid command: {}", s)),
        }
    }
}

/// RemoteControl manages the state and operations of the remote control system.
/// It handles channel selection and button commands while maintaining the current state.
#[derive(Debug)]
pub struct RemoteControl {
    backend: ActiveBackend,
    /// Fan-out of completed Up/Down commands. This is a transient event stream
    /// used to mirror inferred blind position into HomeKit.
    position_tx: broadcast::Sender<PositionUpdate>,
}

impl RemoteControl {
    /// Creates a new RemoteControl instance and initializes the channel state
    pub async fn new() -> Result<Self> {
        let backend = ActiveBackend::new().await?;
        let (position_tx, _) = broadcast::channel(64);
        Ok(Self {
            backend,
            position_tx,
        })
    }

    /// Return the latest known channel selector state.
    pub fn current_selection(&self) -> Channel {
        self.backend.selected_channel()
    }

    /// Subscribe to channel selector changes. New subscribers can immediately read
    /// the latest selection from the returned receiver.
    pub fn subscribe_selection(&self) -> SelectedChannelRx {
        self.backend.subscribe_selected_channel()
    }

    /// Subscribe to position updates emitted after every successful Up/Down.
    pub fn subscribe_positions(&self) -> broadcast::Receiver<PositionUpdate> {
        self.position_tx.subscribe()
    }

    /// Run a UI command against backend state. Directional commands target the
    /// selected channel; `Select` optionally targets a specific channel.
    ///
    /// `Select` with `channel=Some` is a no-op after the cycle; `Select` with
    /// `channel=None` triggers exactly one cycle tick.
    pub async fn execute(
        &self,
        command: Command,
        channel: Option<Channel>,
    ) -> Result<CommandOutcome> {
        self.backend.execute(command, channel).await?;
        let target = self.current_selection();
        Ok(self.complete_command(target, command))
    }

    /// Run a command directly on `channel` without consulting or mutating the
    /// public selected-channel state. Used by HomeKit and CLI-style callers.
    pub async fn execute_on(&self, channel: Channel, command: Command) -> Result<CommandOutcome> {
        self.backend.execute_on(channel, command).await?;
        Ok(self.complete_command(channel, command))
    }

    fn complete_command(&self, channel: Channel, command: Command) -> CommandOutcome {
        let inferred_position = infer_position(command);
        if let Some(position) = inferred_position {
            let _ = self.position_tx.send(PositionUpdate { channel, position });
        }
        CommandOutcome { inferred_position }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_from_str_valid() {
        assert_eq!(Command::from_str("up").unwrap(), Command::Up);
        assert_eq!(Command::from_str("down").unwrap(), Command::Down);
        assert_eq!(Command::from_str("my").unwrap(), Command::My);
        assert_eq!(Command::from_str("stop").unwrap(), Command::Stop);
        assert_eq!(Command::from_str("select").unwrap(), Command::Select);
    }

    #[test]
    fn command_from_str_invalid() {
        assert!(Command::from_str("UP").is_err());
        assert!(Command::from_str("toggle").is_err());
        assert!(Command::from_str("").is_err());
    }
}
