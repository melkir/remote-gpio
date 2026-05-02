use anyhow::Result;
use futures_util::future::BoxFuture;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch::{self, Sender};
use tokio::sync::Mutex;

use crate::driver::{SelectedChannelRx, TelisOptions};
use crate::gpio::{Channel, GpioOptions, TelisButton};
use crate::remote::Command;

const MAX_SELECT_CYCLES: usize = 8;
const PROG_PRESS: Duration = Duration::from_millis(2500);
const RTS_PROG_DELAY: Duration = Duration::from_millis(700);

#[derive(Debug)]
pub(crate) struct TelisDriver {
    sender: Sender<Channel>,
    selected_rx: SelectedChannelRx,
    transport: Arc<dyn TelisTransport>,
    prog_gpio: Option<u8>,
    execute_lock: Mutex<()>,
}

impl TelisDriver {
    pub(crate) async fn new(gpio: GpioOptions, options: TelisOptions) -> Result<Self> {
        let prog_gpio = options.gpio.prog;
        let transport = Arc::new(GpioTelisTransport { gpio, options });
        Self::with_transport(transport, prog_gpio).await
    }

    async fn with_transport(
        transport: Arc<dyn TelisTransport>,
        prog_gpio: Option<u8>,
    ) -> Result<Self> {
        let selection = transport.select().await?;
        let (sender, selected_rx) = watch::channel(selection);
        Ok(Self {
            sender,
            selected_rx,
            transport,
            prog_gpio,
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
            .prog_gpio
            .ok_or_else(|| anyhow::anyhow!("telis.gpio.prog is required for prog"))?;
        tracing::info!(%channel, prog_gpio, "pressing Telis Prog");
        self.transport.press_gpio(prog_gpio, PROG_PRESS).await?;
        tokio::time::sleep(RTS_PROG_DELAY).await;
        tracing::info!(%channel, prog_gpio, "Telis Prog press complete");
        Ok(())
    }
}

trait TelisTransport: std::fmt::Debug + Send + Sync + 'static {
    fn press(&self, button: TelisButton) -> BoxFuture<'_, Result<()>>;
    fn press_gpio(&self, gpio: u8, duration: Duration) -> BoxFuture<'_, Result<()>>;
    fn select(&self) -> BoxFuture<'_, Result<Channel>>;
}

#[derive(Debug)]
struct GpioTelisTransport {
    gpio: GpioOptions,
    options: TelisOptions,
}

impl TelisTransport for GpioTelisTransport {
    fn press(&self, button: TelisButton) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            crate::gpio::trigger_output(&self.gpio.chip, button, &self.options.gpio).await
        })
    }

    fn press_gpio(&self, gpio: u8, duration: Duration) -> BoxFuture<'_, Result<()>> {
        Box::pin(
            async move { crate::gpio::trigger_output_gpio(&self.gpio.chip, gpio, duration).await },
        )
    }

    fn select(&self) -> BoxFuture<'_, Result<Channel>> {
        Box::pin(async move {
            let chip = self.gpio.chip.clone();
            let options = self.options.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    crate::gpio::trigger_output(&chip, TelisButton::Select, &options.gpio).await
                {
                    tracing::error!("failed to trigger Telis select button: {e}");
                }
            });

            crate::gpio::watch_inputs(&self.gpio.chip, &self.options.gpio).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    enum Event {
        Button(TelisButton),
        Gpio(u8),
    }

    #[derive(Debug)]
    struct RecordingTransport {
        selections: StdMutex<Vec<Channel>>,
        events: StdMutex<Vec<Event>>,
    }

    impl RecordingTransport {
        fn new(selections: Vec<Channel>) -> Self {
            Self {
                selections: StdMutex::new(selections),
                events: StdMutex::new(Vec::new()),
            }
        }

        fn events(&self) -> Vec<Event> {
            self.events
                .lock()
                .expect("recording transport events mutex")
                .clone()
        }
    }

    impl TelisTransport for RecordingTransport {
        fn press(&self, button: TelisButton) -> BoxFuture<'_, Result<()>> {
            Box::pin(async move {
                self.events
                    .lock()
                    .expect("recording transport events mutex")
                    .push(Event::Button(button));
                Ok(())
            })
        }

        fn press_gpio(&self, gpio: u8, _duration: Duration) -> BoxFuture<'_, Result<()>> {
            Box::pin(async move {
                self.events
                    .lock()
                    .expect("recording transport events mutex")
                    .push(Event::Gpio(gpio));
                Ok(())
            })
        }

        fn select(&self) -> BoxFuture<'_, Result<Channel>> {
            Box::pin(async move {
                self.events
                    .lock()
                    .expect("recording transport events mutex")
                    .push(Event::Button(TelisButton::Select));
                let channel = self
                    .selections
                    .lock()
                    .expect("recording transport selections mutex")
                    .remove(0);
                Ok(channel)
            })
        }
    }

    #[tokio::test]
    async fn execute_on_prog_selects_target_channel_then_presses_prog_gpio() {
        let transport = Arc::new(RecordingTransport::new(vec![
            Channel::L1,
            Channel::L2,
            Channel::L3,
        ]));
        let driver = TelisDriver::with_transport(transport.clone(), Some(5))
            .await
            .unwrap();

        driver.execute_on(Channel::L3, Command::Prog).await.unwrap();

        assert_eq!(
            transport.events(),
            vec![
                Event::Button(TelisButton::Select),
                Event::Button(TelisButton::Select),
                Event::Button(TelisButton::Select),
                Event::Gpio(5),
            ]
        );
    }
}
