use anyhow::Result;
use tokio::sync::watch::{self, Sender};
use tokio::sync::Mutex;

use crate::backend::SelectedChannelRx;
use crate::gpio::{Channel, TelisButton};
use crate::remote::Command;

const MAX_SELECT_CYCLES: usize = 8;

#[derive(Debug)]
pub(crate) struct TelisBackend {
    sender: Sender<Channel>,
    selected_rx: SelectedChannelRx,
    transport: TelisGpioTransport,
    execute_lock: Mutex<()>,
}

impl TelisBackend {
    pub(super) async fn new() -> Result<Self> {
        let transport = TelisGpioTransport;
        let selection = transport.select().await?;
        let (sender, selected_rx) = watch::channel(selection);
        Ok(Self {
            sender,
            selected_rx,
            transport,
            execute_lock: Mutex::new(()),
        })
    }

    pub(super) async fn execute(&self, command: Command, channel: Option<Channel>) -> Result<()> {
        let _guard = self.execute_lock.lock().await;
        if let Some(target) = channel {
            self.select_to(target, true).await?;
        }

        match command {
            Command::Up => self.transport.press(TelisButton::Up).await,
            Command::Down => self.transport.press(TelisButton::Down).await,
            Command::My | Command::Stop => self.transport.press(TelisButton::Stop).await,
            Command::Select => {
                if channel.is_none() {
                    self.select_once(true).await.map(|_| ())
                } else {
                    Ok(())
                }
            }
        }
    }

    pub(super) async fn execute_on(&self, channel: Channel, command: Command) -> Result<()> {
        let _guard = self.execute_lock.lock().await;
        self.select_to(channel, true).await?;

        match command {
            Command::Up => self.transport.press(TelisButton::Up).await,
            Command::Down => self.transport.press(TelisButton::Down).await,
            Command::My | Command::Stop => self.transport.press(TelisButton::Stop).await,
            Command::Select => Ok(()),
        }
    }

    pub(super) fn selected_channel(&self) -> Channel {
        *self.selected_rx.borrow()
    }

    pub(super) fn subscribe_selected_channel(&self) -> SelectedChannelRx {
        self.selected_rx.clone()
    }

    async fn select_once(&self, broadcast: bool) -> Result<Channel> {
        let channel = self.transport.select().await?;
        if broadcast {
            self.sender.send(channel)?;
        }
        Ok(channel)
    }

    async fn select_to(&self, target: Channel, broadcast: bool) -> Result<()> {
        self.select_from_to(self.selected_channel(), target, broadcast)
            .await
    }

    async fn select_from_to(
        &self,
        mut current: Channel,
        target: Channel,
        broadcast: bool,
    ) -> Result<()> {
        let mut attempts = 0;
        while current != target {
            if attempts >= MAX_SELECT_CYCLES {
                anyhow::bail!(
                    "LED selection did not reach {target} after {attempts} select cycles"
                );
            }
            current = self.select_once(broadcast).await?;
            attempts += 1;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct TelisGpioTransport;

impl TelisGpioTransport {
    async fn press(&self, button: TelisButton) -> Result<()> {
        crate::gpio::trigger_output(button).await
    }

    async fn select(&self) -> Result<Channel> {
        tokio::spawn(async move {
            if let Err(e) = crate::gpio::trigger_output(TelisButton::Select).await {
                tracing::error!("failed to trigger Telis select button: {e}");
            }
        });

        crate::gpio::watch_inputs().await
    }
}
