//! Hardware driver abstraction (`fake`, `telis`, `rts`).

use anyhow::Result;
use tokio::sync::watch::{self, Receiver, Sender};

use crate::config::DriverConfig;
use crate::core::{Channel, Command};

mod fake;
mod rts;
mod telis;

/// Shown when `prog` is requested while the Telis driver is selected.
pub const TELIS_PROG_UNAVAILABLE: &str = "prog is not available with the Telis driver; set driver = \"rts\" in config.toml (somfy config set-driver rts) to pair over RF — see docs/HARDWARE.md#pairing";

use fake::FakeDriver;
use rts::RtsDriver;
pub(crate) use rts::{pigpiod_addr_list, pigpiod_addrs, PIGPIOD_PORT};
use telis::TelisDriver;

pub type SelectedChannelRx = Receiver<Channel>;

/// The channel-selection watch pair every driver keeps.
///
/// Drivers differ in how a selection is *made* — stepping a physical selector,
/// reading persisted RTS state, or plain memory — but not in how it is
/// published, so the sender/receiver pair and its accessors live here instead of
/// being repeated in each driver.
#[derive(Debug)]
pub(crate) struct Selection {
    sender: Sender<Channel>,
    rx: SelectedChannelRx,
}

impl Selection {
    pub(crate) fn new(channel: Channel) -> Self {
        let (sender, rx) = watch::channel(channel);
        Self { sender, rx }
    }

    pub(crate) fn get(&self) -> Channel {
        *self.rx.borrow()
    }

    pub(crate) fn subscribe(&self) -> SelectedChannelRx {
        self.rx.clone()
    }

    /// Publish a new selection to every subscriber.
    ///
    /// Infallible: `send` only fails once every receiver is gone, and this
    /// struct holds one for its whole life.
    pub(crate) fn set(&self, channel: Channel) {
        self.sender.send_replace(channel);
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CommandOutcome {
    pub inferred_position: Option<u8>,
}

#[cfg(test)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProtocolOperation {
    TelisSelection(Channel),
    FakeCommand { channel: Channel, command: Command },
}

#[derive(Debug)]
pub(crate) enum CommandRouter {
    Fake(FakeDriver),
    Telis(TelisDriver),
    Rts(Box<RtsDriver>),
}

impl CommandRouter {
    pub async fn new(config: DriverConfig) -> Result<Self> {
        Ok(match config {
            DriverConfig::Fake => Self::Fake(FakeDriver::new(Channel::L1)),
            DriverConfig::Telis { gpio, telis } => {
                Self::Telis(TelisDriver::new(gpio, telis).await?)
            }
            DriverConfig::Rts { rts } => Self::Rts(Box::new(RtsDriver::new(rts).await?)),
        })
    }

    /// UI-style command dispatch. Telis/Fake may use `channel` to select or target
    /// before acting; RTS uses persisted selection for directional commands (use
    /// [`Self::execute_on`] to transmit on a specific channel without selecting).
    pub async fn execute(&self, command: Command, channel: Option<Channel>) -> Result<()> {
        match self {
            Self::Fake(driver) => driver.execute(command, channel).await,
            Self::Telis(driver) => driver.execute(command, channel).await,
            Self::Rts(driver) => driver.execute(command, channel).await,
        }
    }

    /// Send `command` on `channel`. Native for RTS (addressed RF); Telis selects
    /// the physical LED row first; Fake records the target channel directly.
    pub async fn execute_on(&self, channel: Channel, command: Command) -> Result<()> {
        match self {
            Self::Fake(driver) => driver.execute_on(channel, command).await,
            Self::Telis(driver) => driver.execute_on(channel, command).await,
            Self::Rts(driver) => driver.execute_on(channel, command).await,
        }
    }

    fn selection(&self) -> &Selection {
        match self {
            Self::Fake(driver) => driver.selection(),
            Self::Telis(driver) => driver.selection(),
            Self::Rts(driver) => driver.selection(),
        }
    }

    pub fn selected_channel(&self) -> Channel {
        self.selection().get()
    }

    pub fn subscribe_selected_channel(&self) -> SelectedChannelRx {
        self.selection().subscribe()
    }

    #[cfg(test)]
    pub(crate) fn operations(&self) -> Vec<ProtocolOperation> {
        match self {
            Self::Fake(driver) => driver.operations(),
            #[allow(unreachable_patterns)]
            _ => unreachable!("operations() requires the fake driver"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rts::state::{RtsState, DEFAULT_RESERVE_SIZE, STATE_FILE};

    #[tokio::test]
    async fn rts_prog_transmits_pairing_waveform_without_changing_selection() {
        use crate::config::RtsOptions;
        use crate::rts::frame::RtsCommand;
        use std::sync::{Arc, Mutex as StdMutex};

        #[derive(Copy, Clone, Debug, PartialEq, Eq)]
        enum Event {
            RtsTransmit(Channel, RtsCommand),
        }

        #[derive(Debug)]
        struct RecordingTransmitter {
            events: Arc<StdMutex<Vec<Event>>>,
        }

        impl rts::RtsTransmitter for RecordingTransmitter {
            fn transmit(&self, transmission: rts::PreparedTransmission) -> Result<()> {
                self.events
                    .lock()
                    .expect("recording transmitter mutex")
                    .push(Event::RtsTransmit(
                        transmission.channel,
                        transmission.command,
                    ));
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(StdMutex::new(Vec::new()));
        let state_path = dir.path().join(STATE_FILE);
        let rts_driver = rts::RtsDriver::new_for_test(
            RtsOptions::default(),
            &state_path,
            Arc::new(RecordingTransmitter {
                events: events.clone(),
            }),
        )
        .await
        .unwrap();
        let router = CommandRouter::Rts(Box::new(rts_driver));

        router.execute_on(Channel::L3, Command::Prog).await.unwrap();

        assert_eq!(
            *events.lock().expect("recording events mutex"),
            vec![Event::RtsTransmit(Channel::L3, RtsCommand::Prog)]
        );
        let state: RtsState =
            serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(state.selected_channel, Channel::L1);
        assert_eq!(
            state.channels.get(&Channel::L3).unwrap().reserved_until,
            1 + DEFAULT_RESERVE_SIZE
        );
        assert_eq!(state.channels.get(&Channel::L1).unwrap().reserved_until, 1);
    }
}
