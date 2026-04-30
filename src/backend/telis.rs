use anyhow::Result;
use std::time::Duration;
use tokio::sync::watch::{self, Sender};
use tokio::sync::Mutex;

use crate::backend::{SelectedChannelRx, TelisOptions};
use crate::gpio::{Channel, TelisButton};
use crate::remote::Command;

const MAX_SELECT_CYCLES: usize = 8;
const PROG_PRESS: Duration = Duration::from_millis(2500);
const RTS_PROG_DELAY: Duration = Duration::from_millis(700);

#[derive(Debug)]
pub(crate) struct TelisBackend {
    sender: Sender<Channel>,
    selected_rx: SelectedChannelRx,
    transport: TelisGpioTransport,
    execute_lock: Mutex<()>,
}

impl TelisBackend {
    pub(crate) async fn new(options: TelisOptions) -> Result<Self> {
        let transport = TelisGpioTransport { options };
        let selection = transport.select().await?;
        let (sender, selected_rx) = watch::channel(selection);
        Ok(Self {
            sender,
            selected_rx,
            transport,
            execute_lock: Mutex::new(()),
        })
    }

    pub(crate) async fn execute(&self, command: Command, channel: Option<Channel>) -> Result<()> {
        let _guard = self.execute_lock.lock().await;
        if let Some(target) = channel {
            self.select_to(target, true).await?;
        }

        match command {
            Command::Up => self.transport.press(TelisButton::Up).await,
            Command::Down => self.transport.press(TelisButton::Down).await,
            Command::Stop => self.transport.press(TelisButton::Stop).await,
            Command::Prog => self.press_prog(self.selected_channel()).await,
            Command::Select => {
                if channel.is_none() {
                    self.select_once(true).await.map(|_| ())
                } else {
                    Ok(())
                }
            }
        }
    }

    pub(crate) async fn execute_on(&self, channel: Channel, command: Command) -> Result<()> {
        let _guard = self.execute_lock.lock().await;
        self.select_to(channel, true).await?;

        match command {
            Command::Up => self.transport.press(TelisButton::Up).await,
            Command::Down => self.transport.press(TelisButton::Down).await,
            Command::Stop => self.transport.press(TelisButton::Stop).await,
            Command::Prog => self.press_prog(channel).await,
            Command::Select => Ok(()),
        }
    }

    #[cfg(feature = "rts")]
    pub(crate) async fn program(&self, channel: Channel) -> Result<()> {
        let _guard = self.execute_lock.lock().await;
        self.select_to(channel, true).await?;
        self.press_prog(channel).await
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

    async fn press_prog(&self, channel: Channel) -> Result<()> {
        let prog_gpio = self
            .transport
            .options
            .gpio
            .prog
            .ok_or_else(|| anyhow::anyhow!("telis.gpio.prog is required for prog"))?;
        tracing::info!(%channel, prog_gpio, "pressing Telis Prog");
        crate::gpio::trigger_output_gpio(prog_gpio, PROG_PRESS).await?;
        tokio::time::sleep(RTS_PROG_DELAY).await;
        tracing::info!(%channel, prog_gpio, "Telis Prog press complete");
        Ok(())
    }
}

#[cfg(feature = "rts")]
#[derive(Clone, Debug)]
pub(crate) struct TelisProgrammer {
    options: TelisOptions,
}

#[cfg(feature = "rts")]
impl TelisProgrammer {
    pub(crate) fn new(options: TelisOptions) -> Self {
        Self { options }
    }

    pub(crate) async fn program(&self, channel: Channel) -> Result<()> {
        let telis = TelisBackend::new(self.options.clone()).await?;
        telis.program(channel).await
    }
}

#[derive(Clone, Debug)]
struct TelisGpioTransport {
    options: TelisOptions,
}

impl TelisGpioTransport {
    async fn press(&self, button: TelisButton) -> Result<()> {
        crate::gpio::trigger_output(button, &self.options.gpio).await
    }

    async fn select(&self) -> Result<Channel> {
        let options = self.options.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::gpio::trigger_output(TelisButton::Select, &options.gpio).await {
                tracing::error!("failed to trigger Telis select button: {e}");
            }
        });

        crate::gpio::watch_inputs(&self.options.gpio).await
    }
}
