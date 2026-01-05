use anyhow::{Ok, Result};

use tokio::sync::watch::{self, Receiver, Sender};

use crate::gpio::{trigger_output, watch_inputs, Input, Output};

/// RemoteControl manages the state and operations of the remote control system.
/// It handles LED selection and button commands while maintaining the current state.
#[derive(Debug)]
pub struct RemoteControl {
    /// Sender for broadcasting LED state changes to all subscribers
    sender: Sender<Input>,
    /// Receiver for monitoring LED state changes
    pub receiver: Receiver<Input>,
}

impl RemoteControl {
    /// Creates a new RemoteControl instance and initializes the LED state
    pub async fn new() -> Result<Self> {
        let selection = Self::trigger_select().await?;
        let (sender, receiver) = watch::channel::<Input>(selection);
        Ok(Self { sender, receiver })
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

    /// Internal helper to trigger the select button and wait for LED selection
    async fn trigger_select() -> Result<Input> {
        // Run button press and input watching concurrently
        tokio::spawn(async move {
            trigger_output(Output::Select).await.unwrap();
        });

        watch_inputs().await
    }
}
