use anyhow::Result;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use tokio::sync::watch::{self, Receiver, Sender};
use tokio::sync::Mutex;

use crate::gpio::{Channel, TelisButton};
use crate::remote::Command;

pub type SelectedChannelRx = Receiver<Channel>;

#[cfg(feature = "hw")]
const MAX_SELECT_CYCLES: usize = 8;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CommandOutcome {
    pub inferred_position: Option<u8>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ProtocolOperation {
    TelisButton(TelisButton),
    TelisSelection(Channel),
    FakeCommand { channel: Channel, command: Command },
}

#[derive(Debug)]
pub enum ActiveBackend {
    #[cfg(all(feature = "fake", not(feature = "hw")))]
    Fake(FakeBackend),
    #[cfg(feature = "hw")]
    Telis(TelisBackend),
}

impl ActiveBackend {
    pub async fn new() -> Result<Self> {
        #[cfg(feature = "hw")]
        {
            return Ok(Self::Telis(TelisBackend::new().await?));
        }

        #[cfg(all(feature = "fake", not(feature = "hw")))]
        {
            Ok(Self::Fake(FakeBackend::new(Channel::L1)))
        }
    }

    pub async fn execute(&self, command: Command, channel: Option<Channel>) -> Result<()> {
        match self {
            #[cfg(all(feature = "fake", not(feature = "hw")))]
            Self::Fake(backend) => backend.execute(command, channel).await,
            #[cfg(feature = "hw")]
            Self::Telis(backend) => backend.execute(command, channel).await,
        }
    }

    pub async fn execute_on(&self, channel: Channel, command: Command) -> Result<()> {
        match self {
            #[cfg(all(feature = "fake", not(feature = "hw")))]
            Self::Fake(backend) => backend.execute_on(channel, command).await,
            #[cfg(feature = "hw")]
            Self::Telis(backend) => backend.execute_on(channel, command).await,
        }
    }

    pub fn selected_channel(&self) -> Channel {
        match self {
            #[cfg(all(feature = "fake", not(feature = "hw")))]
            Self::Fake(backend) => backend.selected_channel(),
            #[cfg(feature = "hw")]
            Self::Telis(backend) => backend.selected_channel(),
        }
    }

    pub fn subscribe_selected_channel(&self) -> SelectedChannelRx {
        match self {
            #[cfg(all(feature = "fake", not(feature = "hw")))]
            Self::Fake(backend) => backend.subscribe_selected_channel(),
            #[cfg(feature = "hw")]
            Self::Telis(backend) => backend.subscribe_selected_channel(),
        }
    }
}

#[cfg(all(feature = "fake", not(feature = "hw")))]
#[derive(Debug)]
pub struct FakeBackend {
    sender: Sender<Channel>,
    selected_rx: SelectedChannelRx,
    transport: FakeTransport,
    execute_lock: Mutex<()>,
}

#[cfg(all(feature = "fake", not(feature = "hw")))]
impl FakeBackend {
    pub fn new(selected_channel: Channel) -> Self {
        let (sender, selected_rx) = watch::channel(selected_channel);
        Self {
            sender,
            selected_rx,
            transport: FakeTransport::new(),
            execute_lock: Mutex::new(()),
        }
    }

    #[cfg(test)]
    fn operations(&self) -> Vec<ProtocolOperation> {
        self.transport.operations()
    }

    async fn execute(&self, command: Command, channel: Option<Channel>) -> Result<()> {
        let _guard = self.execute_lock.lock().await;
        let target = channel.unwrap_or_else(|| self.selected_channel());
        match command {
            Command::Select => {
                let channel = channel.unwrap_or_else(|| next_channel(self.selected_channel()));
                self.sender.send(channel)?;
                self.transport
                    .record(ProtocolOperation::TelisSelection(channel))
                    .await;
            }
            Command::Up | Command::Down | Command::My | Command::Stop => {
                self.transport.send(target, command).await?;
            }
        }
        Ok(())
    }

    async fn execute_on(&self, channel: Channel, command: Command) -> Result<()> {
        let _guard = self.execute_lock.lock().await;
        self.transport.send(channel, command).await?;
        Ok(())
    }

    fn selected_channel(&self) -> Channel {
        *self.selected_rx.borrow()
    }

    fn subscribe_selected_channel(&self) -> SelectedChannelRx {
        self.selected_rx.clone()
    }
}

#[cfg(all(feature = "fake", not(feature = "hw")))]
#[derive(Clone, Debug, Default)]
struct FakeTransport {
    operations: Arc<StdMutex<Vec<ProtocolOperation>>>,
}

#[cfg(all(feature = "fake", not(feature = "hw")))]
impl FakeTransport {
    fn new() -> Self {
        Self::default()
    }

    async fn send(&self, channel: Channel, command: Command) -> Result<()> {
        self.record(ProtocolOperation::FakeCommand { channel, command })
            .await;
        Ok(())
    }

    async fn record(&self, operation: ProtocolOperation) {
        self.operations
            .lock()
            .expect("fake transport mutex")
            .push(operation);
    }

    #[cfg(test)]
    fn operations(&self) -> Vec<ProtocolOperation> {
        self.operations
            .lock()
            .expect("fake transport mutex")
            .clone()
    }
}

#[cfg(feature = "hw")]
#[derive(Debug)]
pub struct TelisBackend {
    sender: Sender<Channel>,
    selected_rx: SelectedChannelRx,
    transport: TelisGpioTransport,
    execute_lock: Mutex<()>,
}

#[cfg(feature = "hw")]
impl TelisBackend {
    pub async fn new() -> Result<Self> {
        let selection = trigger_select().await?;
        let (sender, selected_rx) = watch::channel(selection);
        Ok(Self {
            sender,
            selected_rx,
            transport: TelisGpioTransport,
            execute_lock: Mutex::new(()),
        })
    }

    async fn execute(&self, command: Command, channel: Option<Channel>) -> Result<()> {
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

    async fn execute_on(&self, channel: Channel, command: Command) -> Result<()> {
        let _guard = self.execute_lock.lock().await;
        self.select_to(channel, true).await?;

        match command {
            Command::Up => self.transport.press(TelisButton::Up).await,
            Command::Down => self.transport.press(TelisButton::Down).await,
            Command::My | Command::Stop => self.transport.press(TelisButton::Stop).await,
            Command::Select => Ok(()),
        }
    }

    fn selected_channel(&self) -> Channel {
        *self.selected_rx.borrow()
    }

    fn subscribe_selected_channel(&self) -> SelectedChannelRx {
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

#[cfg(feature = "hw")]
#[derive(Clone, Debug)]
struct TelisGpioTransport;

#[cfg(feature = "hw")]
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

#[cfg(all(feature = "fake", not(feature = "hw")))]
fn next_channel(channel: Channel) -> Channel {
    match channel {
        Channel::L1 => Channel::L2,
        Channel::L2 => Channel::L3,
        Channel::L3 => Channel::L4,
        Channel::L4 => Channel::ALL,
        Channel::ALL => Channel::L1,
    }
}

pub fn infer_position(command: Command) -> Option<u8> {
    match command {
        Command::Up => Some(100),
        Command::Down => Some(0),
        Command::My | Command::Stop | Command::Select => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(feature = "fake", not(feature = "hw")))]
    #[tokio::test]
    async fn fake_execute_select_updates_and_broadcasts_selection() {
        let backend = FakeBackend::new(Channel::L1);
        let mut rx = backend.subscribe_selected_channel();

        backend
            .execute(Command::Select, Some(Channel::L3))
            .await
            .unwrap();

        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow_and_update(), Channel::L3);
        assert_eq!(backend.selected_channel(), Channel::L3);
        assert_eq!(
            backend.operations(),
            vec![ProtocolOperation::TelisSelection(Channel::L3)]
        );
    }

    #[cfg(all(feature = "fake", not(feature = "hw")))]
    #[tokio::test]
    async fn fake_execute_on_does_not_mutate_or_broadcast_selection() {
        let backend = FakeBackend::new(Channel::L1);
        let rx = backend.subscribe_selected_channel();

        backend.execute_on(Channel::L3, Command::Up).await.unwrap();

        assert_eq!(backend.selected_channel(), Channel::L1);
        assert!(!rx.has_changed().unwrap());
        assert_eq!(
            backend.operations(),
            vec![ProtocolOperation::FakeCommand {
                channel: Channel::L3,
                command: Command::Up
            }]
        );
    }

    #[test]
    fn position_inference_only_tracks_directional_extremes() {
        assert_eq!(infer_position(Command::Up), Some(100));
        assert_eq!(infer_position(Command::Down), Some(0));
        assert_eq!(infer_position(Command::My), None);
        assert_eq!(infer_position(Command::Stop), None);
        assert_eq!(infer_position(Command::Select), None);
    }
}
