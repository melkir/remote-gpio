use anyhow::Result;
use tokio::sync::{broadcast, Mutex};

use crate::driver::{infer_position, CommandOutcome, CommandRouter, SelectedChannelRx};

pub use crate::core::{Channel, Command};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PositionUpdate {
    pub channel: Channel,
    pub position: u8,
}

/// Driver-agnostic control of channel selection, button presses, and position events.
#[derive(Debug)]
pub struct BlindController {
    router: CommandRouter,
    operation_lock: Mutex<()>,
    /// Fan-out of completed Up/Down commands. This is a transient event stream
    /// used to mirror inferred blind position into HomeKit.
    position_tx: broadcast::Sender<PositionUpdate>,
}

impl BlindController {
    pub async fn with_driver(config: crate::config::DriverConfig) -> Result<Self> {
        let router = CommandRouter::new(config).await?;
        let (position_tx, _) = broadcast::channel(64);
        Ok(Self {
            router,
            operation_lock: Mutex::new(()),
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
        let _guard = self.operation_lock.lock().await;
        self.router.execute(command, channel).await?;
        let target = self.current_selection();
        Ok(self.complete_command(target, command))
    }

    /// Run a client command as one logical operation. `Select` changes the
    /// selected channel; action commands with an explicit channel target that
    /// channel directly without changing logical selection when the driver
    /// supports that distinction.
    pub async fn execute_client_command(
        &self,
        command: Command,
        channel: Option<Channel>,
    ) -> Result<CommandOutcome> {
        let _guard = self.operation_lock.lock().await;
        if command == Command::Select {
            self.router.execute(command, channel).await?;
            let target = self.current_selection();
            return Ok(self.complete_command(target, command));
        }

        if let Some(channel) = channel {
            self.router.execute_on(channel, command).await?;
            return Ok(self.complete_command(channel, command));
        }

        self.router.execute(command, None).await?;
        let target = self.current_selection();
        Ok(self.complete_command(target, command))
    }

    /// Run a command directly on `channel`. RTS can do this without changing
    /// public selection state; Telis may update selection because targeting a
    /// channel requires moving the physical selector.
    pub async fn execute_on(&self, channel: Channel, command: Command) -> Result<CommandOutcome> {
        let _guard = self.operation_lock.lock().await;
        self.router.execute_on(channel, command).await?;
        Ok(self.complete_command(channel, command))
    }

    #[cfg(test)]
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

    #[tokio::test]
    async fn client_command_with_channel_targets_without_selection() {
        let controller = BlindController::with_driver(crate::config::DriverConfig::fake())
            .await
            .unwrap();

        controller
            .execute_client_command(Command::Up, Some(Channel::L3))
            .await
            .unwrap();

        assert_eq!(controller.current_selection(), Channel::L1);
        assert_eq!(
            controller.operations(),
            vec![crate::driver::ProtocolOperation::FakeCommand {
                channel: Channel::L3,
                command: Command::Up,
            }]
        );
    }
}
