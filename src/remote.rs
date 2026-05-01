use anyhow::Result;
use std::str::FromStr;

use tokio::sync::broadcast;

use crate::driver::{infer_position, CommandOutcome, CommandRouter, SelectedChannelRx};
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
    Stop,
    Select,
    Prog,
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
            _ => Err(anyhow::anyhow!("Invalid command: {}", s)),
        }
    }
}

/// RemoteControl manages the state and operations of the remote control system.
/// It handles channel selection and button commands while maintaining the current state.
#[derive(Debug)]
pub struct RemoteControl {
    router: CommandRouter,
    /// Fan-out of completed Up/Down commands. This is a transient event stream
    /// used to mirror inferred blind position into HomeKit.
    position_tx: broadcast::Sender<PositionUpdate>,
}

impl RemoteControl {
    pub async fn with_driver(config: crate::driver::DriverConfig) -> Result<Self> {
        let router = CommandRouter::new(config).await?;
        let (position_tx, _) = broadcast::channel(64);
        Ok(Self {
            router,
            position_tx,
        })
    }

    /// Return the latest known channel selector state.
    pub fn current_selection(&self) -> Channel {
        self.router.selected_channel()
    }

    /// Subscribe to channel selector changes. New subscribers can immediately read
    /// the latest selection from the returned receiver.
    pub fn subscribe_selection(&self) -> SelectedChannelRx {
        self.router.subscribe_selected_channel()
    }

    /// Subscribe to position updates emitted after every successful Up/Down.
    pub fn subscribe_positions(&self) -> broadcast::Receiver<PositionUpdate> {
        self.position_tx.subscribe()
    }

    /// Run a UI command against driver state. Directional commands target the
    /// selected channel; `Select` optionally targets a specific channel.
    ///
    /// `Select` with `channel=Some` is a no-op after the cycle; `Select` with
    /// `channel=None` triggers exactly one cycle tick.
    pub async fn execute(
        &self,
        command: Command,
        channel: Option<Channel>,
    ) -> Result<CommandOutcome> {
        self.router.execute(command, channel).await?;
        let target = self.current_selection();
        Ok(self.complete_command(target, command))
    }

    /// Run a command directly on `channel`. RTS can do this without changing
    /// public selection state; Telis may update selection because targeting a
    /// channel requires moving the physical selector.
    pub async fn execute_on(&self, channel: Channel, command: Command) -> Result<CommandOutcome> {
        self.router.execute_on(channel, command).await?;
        Ok(self.complete_command(channel, command))
    }

    #[cfg(all(test, feature = "fake"))]
    pub(crate) fn operations(&self) -> Vec<crate::driver::ProtocolOperation> {
        self.router.operations()
    }

    fn complete_command(&self, channel: Channel, command: Command) -> CommandOutcome {
        let inferred_position = infer_position(command);
        if let Some(position) = inferred_position {
            for &target in fan_out_channels(channel) {
                let _ = self.position_tx.send(PositionUpdate {
                    channel: target,
                    position,
                });
            }
        }
        CommandOutcome { inferred_position }
    }
}

fn fan_out_channels(channel: Channel) -> &'static [Channel] {
    match channel {
        Channel::ALL => &[
            Channel::L1,
            Channel::L2,
            Channel::L3,
            Channel::L4,
            Channel::ALL,
        ],
        Channel::L1 => &[Channel::L1],
        Channel::L2 => &[Channel::L2],
        Channel::L3 => &[Channel::L3],
        Channel::L4 => &[Channel::L4],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_from_str_valid() {
        assert_eq!(Command::from_str("up").unwrap(), Command::Up);
        assert_eq!(Command::from_str("down").unwrap(), Command::Down);
        assert_eq!(Command::from_str("stop").unwrap(), Command::Stop);
        assert_eq!(Command::from_str("select").unwrap(), Command::Select);
        assert_eq!(Command::from_str("prog").unwrap(), Command::Prog);
    }

    #[test]
    fn command_from_str_invalid() {
        assert!(Command::from_str("UP").is_err());
        assert!(Command::from_str("toggle").is_err());
        assert!(Command::from_str("").is_err());
    }

    #[test]
    fn fan_out_targets_only_self_for_single_channels() {
        assert_eq!(fan_out_channels(Channel::L1), &[Channel::L1]);
        assert_eq!(fan_out_channels(Channel::L4), &[Channel::L4]);
    }

    #[test]
    fn fan_out_targets_all_paired_channels_for_all() {
        assert_eq!(
            fan_out_channels(Channel::ALL),
            &[
                Channel::L1,
                Channel::L2,
                Channel::L3,
                Channel::L4,
                Channel::ALL,
            ]
        );
    }
}
