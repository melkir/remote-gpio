use anyhow::{bail, Result};
use std::str::FromStr;

use tokio::sync::broadcast;
use tokio::sync::watch::{self, Receiver, Sender};
use tokio::sync::Mutex;

use crate::gpio::{trigger_output, watch_inputs, Input, Output};

const MAX_SELECT_CYCLES: usize = 8;

pub type LedSelectionRx = Receiver<Input>;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PositionUpdate {
    pub led: Input,
    pub position: u8,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Up,
    Down,
    Stop,
    Select,
}

impl FromStr for Command {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "up" => Ok(Command::Up),
            "down" => Ok(Command::Down),
            "stop" => Ok(Command::Stop),
            "select" => Ok(Command::Select),
            _ => Err(anyhow::anyhow!("Invalid command: {}", s)),
        }
    }
}

/// RemoteControl manages the state and operations of the remote control system.
/// It handles LED selection and button commands while maintaining the current state.
#[derive(Debug)]
pub struct RemoteControl {
    /// Sender for broadcasting LED state changes to all subscribers
    sender: Sender<Input>,
    /// Current LED selector state. This is a `watch` channel so new UI clients
    /// immediately receive the current selection.
    selection_rx: LedSelectionRx,
    /// Fan-out of completed Up/Down commands. This is a transient event stream
    /// used to mirror inferred blind position into HomeKit.
    position_tx: broadcast::Sender<PositionUpdate>,
    /// Serializes the select-cycle + GPIO pulse + position broadcast as a
    /// single critical section. Without this, concurrent callers (REST, WS,
    /// HAP) could interleave their `select()` cycles between another
    /// caller's target check and its Up/Down pulse, sending the command to
    /// the wrong LED — and the post-completion broadcast could announce a
    /// different LED again.
    execute_lock: Mutex<()>,
}

impl RemoteControl {
    /// Creates a new RemoteControl instance and initializes the LED state
    pub async fn new() -> Result<Self> {
        let selection = Self::trigger_select().await?;
        let (sender, receiver) = watch::channel::<Input>(selection);
        let (position_tx, _) = broadcast::channel(64);
        Ok(Self {
            sender,
            selection_rx: receiver,
            position_tx,
            execute_lock: Mutex::new(()),
        })
    }

    /// Return the latest known LED selector state.
    pub fn current_selection(&self) -> Input {
        *self.selection_rx.borrow()
    }

    /// Subscribe to LED selector changes. New subscribers can immediately read
    /// the latest selection from the returned receiver.
    pub fn subscribe_selection(&self) -> LedSelectionRx {
        self.selection_rx.clone()
    }

    /// Subscribe to position updates emitted after every successful Up/Down.
    pub fn subscribe_positions(&self) -> broadcast::Receiver<PositionUpdate> {
        self.position_tx.subscribe()
    }

    /// Triggers the select button and returns the new LED selection
    pub async fn select(&self) -> Result<Input> {
        let led = Self::trigger_select().await?;
        self.sender.send(led)?;
        Ok(led)
    }

    /// Triggers the up button command
    pub async fn up(&self) -> Result<()> {
        trigger_output(Output::Up).await
    }

    /// Triggers the down button command
    pub async fn down(&self) -> Result<()> {
        trigger_output(Output::Down).await
    }

    /// Triggers the stop button command
    pub async fn stop(&self) -> Result<()> {
        trigger_output(Output::Stop).await
    }

    /// Cycle to `led` (if specified), then run `command`. Single entry point
    /// shared by REST, WebSocket, and HAP transports.
    ///
    /// `Select` with `led=Some` is a no-op after the cycle; `Select` with
    /// `led=None` triggers exactly one cycle tick.
    ///
    /// Holds `execute_lock` end-to-end so the LED selected at `up()`/`down()`
    /// time is the same one captured by the broadcast — concurrent callers
    /// queue rather than interleave.
    pub async fn execute(&self, led: Option<Input>, command: Command) -> Result<()> {
        let _guard = self.execute_lock.lock().await;
        if let Some(target) = led {
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
            Command::Stop => self.stop().await,
            Command::Select => {
                if led.is_none() {
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
                    led: self.current_selection(),
                    position: pos,
                });
            }
        }
        outcome
    }

    /// Internal helper to trigger the select button and wait for LED selection
    async fn trigger_select() -> Result<Input> {
        // Run button press and input watching concurrently
        tokio::spawn(async move {
            trigger_output(Output::Select).await.unwrap();
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
