use anyhow::Result;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::watch::{self, Sender};
use tokio::sync::Mutex;

#[cfg(test)]
use crate::backend::ProtocolOperation;
use crate::backend::SelectedChannelRx;
use crate::gpio::Channel;
use crate::remote::Command;

#[derive(Debug)]
pub(crate) struct FakeBackend {
    sender: Sender<Channel>,
    selected_rx: SelectedChannelRx,
    transport: FakeTransport,
    execute_lock: Mutex<()>,
}

impl FakeBackend {
    pub(super) fn new(selected_channel: Channel) -> Self {
        let (sender, selected_rx) = watch::channel(selected_channel);
        Self {
            sender,
            selected_rx,
            transport: FakeTransport::new(),
            execute_lock: Mutex::new(()),
        }
    }

    #[cfg(test)]
    pub(super) fn operations(&self) -> Vec<ProtocolOperation> {
        self.transport.operations()
    }

    pub(super) async fn execute(&self, command: Command, channel: Option<Channel>) -> Result<()> {
        let _guard = self.execute_lock.lock().await;
        let target = channel.unwrap_or_else(|| self.selected_channel());
        match command {
            Command::Select => {
                let channel = channel.unwrap_or_else(|| next_channel(self.selected_channel()));
                self.sender.send(channel)?;
                self.transport.record_selection(channel).await;
            }
            Command::Up | Command::Down | Command::Stop | Command::Prog => {
                self.transport.send(target, command).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn execute_on(&self, channel: Channel, command: Command) -> Result<()> {
        let _guard = self.execute_lock.lock().await;
        self.transport.send(channel, command).await?;
        Ok(())
    }

    pub(super) fn selected_channel(&self) -> Channel {
        *self.selected_rx.borrow()
    }

    pub(super) fn subscribe_selected_channel(&self) -> SelectedChannelRx {
        self.selected_rx.clone()
    }
}

#[derive(Clone, Debug, Default)]
struct FakeTransport {
    operations: Arc<StdMutex<Vec<FakeOperation>>>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum FakeOperation {
    Selection(Channel),
    Command { channel: Channel, command: Command },
}

impl FakeTransport {
    fn new() -> Self {
        Self::default()
    }

    async fn send(&self, channel: Channel, command: Command) -> Result<()> {
        self.record(FakeOperation::Command { channel, command })
            .await;
        Ok(())
    }

    async fn record_selection(&self, channel: Channel) {
        self.record(FakeOperation::Selection(channel)).await;
    }

    async fn record(&self, operation: FakeOperation) {
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
            .iter()
            .map(|op| match *op {
                FakeOperation::Selection(channel) => ProtocolOperation::TelisSelection(channel),
                FakeOperation::Command { channel, command } => {
                    ProtocolOperation::FakeCommand { channel, command }
                }
            })
            .collect()
    }
}

fn next_channel(channel: Channel) -> Channel {
    match channel {
        Channel::L1 => Channel::L2,
        Channel::L2 => Channel::L3,
        Channel::L3 => Channel::L4,
        Channel::L4 => Channel::ALL,
        Channel::ALL => Channel::L1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn execute_select_updates_and_broadcasts_selection() {
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

    #[tokio::test]
    async fn execute_on_does_not_mutate_or_broadcast_selection() {
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
}
