use anyhow::{bail, Result};
use std::str::FromStr;

use tokio::sync::broadcast;
use tokio::sync::watch::{self, Receiver, Sender};
use tokio::sync::Mutex;

use crate::gpio::{trigger_output, watch_inputs, Channel, TelisButton};

const MAX_SELECT_CYCLES: usize = 8;

pub type SelectedChannelRx = Receiver<Channel>;

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
    /// Sender for broadcasting channel selection changes to all subscribers.
    sender: Sender<Channel>,
    /// Current channel selector state. This is a `watch` channel so new UI clients
    /// immediately receive the current selection.
    selection_rx: SelectedChannelRx,
    /// Fan-out of completed Up/Down commands. This is a transient event stream
    /// used to mirror inferred blind position into HomeKit.
    position_tx: broadcast::Sender<PositionUpdate>,
    /// Serializes the select-cycle + GPIO pulse + position broadcast as a
    /// single critical section. Without this, concurrent callers (REST, WS,
    /// HAP) could interleave their `select()` cycles between another
    /// caller's target check and its Up/Down pulse, sending the command to
    /// the wrong channel — and the post-completion broadcast could announce a
    /// different channel again.
    execute_lock: Mutex<()>,
}

impl RemoteControl {
    /// Creates a new RemoteControl instance and initializes the channel state
    pub async fn new() -> Result<Self> {
        let selection = Self::trigger_select().await?;
        let (sender, receiver) = watch::channel::<Channel>(selection);
        let (position_tx, _) = broadcast::channel(64);
        Ok(Self {
            sender,
            selection_rx: receiver,
            position_tx,
            execute_lock: Mutex::new(()),
        })
    }

    /// Return the latest known channel selector state.
    pub fn current_selection(&self) -> Channel {
        *self.selection_rx.borrow()
    }

    /// Subscribe to channel selector changes. New subscribers can immediately read
    /// the latest selection from the returned receiver.
    pub fn subscribe_selection(&self) -> SelectedChannelRx {
        self.selection_rx.clone()
    }

    /// Subscribe to position updates emitted after every successful Up/Down.
    pub fn subscribe_positions(&self) -> broadcast::Receiver<PositionUpdate> {
        self.position_tx.subscribe()
    }

    /// Triggers the select button and returns the new channel selection.
    pub async fn select(&self) -> Result<Channel> {
        let channel = Self::trigger_select().await?;
        self.sender.send(channel)?;
        Ok(channel)
    }

    /// Triggers the up button command
    pub async fn up(&self) -> Result<()> {
        trigger_output(TelisButton::Up).await
    }

    /// Triggers the down button command
    pub async fn down(&self) -> Result<()> {
        trigger_output(TelisButton::Down).await
    }

    /// Triggers the Telis middle button. This is `My` for RTS and stop while
    /// the blind is moving.
    pub async fn my(&self) -> Result<()> {
        trigger_output(TelisButton::Stop).await
    }

    /// Cycle to `channel` (if specified), then run `command`. Single entry point
    /// shared by REST, WebSocket, and HAP transports.
    ///
    /// `Select` with `channel=Some` is a no-op after the cycle; `Select` with
    /// `channel=None` triggers exactly one cycle tick.
    ///
    /// Holds `execute_lock` end-to-end so the channel selected at `up()`/`down()`
    /// time is the same one captured by the broadcast — concurrent callers
    /// queue rather than interleave.
    pub async fn execute(&self, channel: Option<Channel>, command: Command) -> Result<()> {
        let _guard = self.execute_lock.lock().await;
        if let Some(target) = channel {
            let mut attempts = 0;
            while self.current_selection() != target {
                if attempts >= MAX_SELECT_CYCLES {
                    bail!("LED selection did not reach {target} after {attempts} select cycles");
                }
                self.select().await?;
                attempts += 1;
            }
        }
        let outcome = match command {
            Command::Up => self.up().await,
            Command::Down => self.down().await,
            Command::My | Command::Stop => self.my().await,
            Command::Select => {
                if channel.is_none() {
                    self.select().await.map(|_| ())
                } else {
                    Ok(())
                }
            }
        };
        if outcome.is_ok() {
            let position = match command {
                Command::Up => Some(100u8),
                Command::Down => Some(0u8),
                _ => None,
            };
            if let Some(pos) = position {
                let _ = self.position_tx.send(PositionUpdate {
                    channel: self.current_selection(),
                    position: pos,
                });
            }
        }
        outcome
    }

    /// Internal helper to trigger the select button and wait for channel selection.
    async fn trigger_select() -> Result<Channel> {
        // Run button press and input watching concurrently
        tokio::spawn(async move {
            trigger_output(TelisButton::Select).await.unwrap();
        });

        watch_inputs().await
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
