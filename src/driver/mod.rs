use anyhow::Result;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;
use tokio::sync::watch::Receiver;

use crate::gpio::{Channel, GpioOptions};
use crate::remote::Command;

mod fake;
mod rts;
mod telis;

use fake::FakeDriver;
use rts::RtsDriver;
pub(crate) use rts::PIGPIOD_ADDR;
use telis::TelisDriver;

pub type SelectedChannelRx = Receiver<Channel>;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CommandOutcome {
    pub inferred_position: Option<u8>,
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DriverKind {
    Fake,
    Telis,
    Rts,
}

impl DriverKind {
    pub fn default_for_target() -> Self {
        if cfg!(all(
            target_os = "linux",
            any(target_arch = "arm", target_arch = "aarch64")
        )) {
            Self::Telis
        } else {
            Self::Fake
        }
    }
}

impl fmt::Display for DriverKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fake => write!(f, "fake"),
            Self::Telis => write!(f, "telis"),
            Self::Rts => write!(f, "rts"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RtsOptions {
    pub spi_device: String,
    pub gpio: RtsGpioOptions,
}

impl Default for RtsOptions {
    fn default() -> Self {
        Self {
            spi_device: "/dev/spidev0.0".to_string(),
            gpio: RtsGpioOptions::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RtsGpioOptions {
    pub gdo0: u8,
}

impl Default for RtsGpioOptions {
    fn default() -> Self {
        Self { gdo0: 18 }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TelisOptions {
    pub gpio: TelisGpioOptions,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct TelisGpioOptions {
    pub up: u8,
    pub stop: u8,
    pub down: u8,
    pub select: u8,
    pub led1: u8,
    pub led2: u8,
    pub led3: u8,
    pub led4: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prog: Option<u8>,
}

impl Default for TelisGpioOptions {
    fn default() -> Self {
        Self {
            up: 26,
            stop: 19,
            down: 13,
            select: 6,
            led1: 21,
            led2: 20,
            led3: 16,
            led4: 12,
            prog: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverConfig {
    pub kind: DriverKind,
    pub gpio: GpioOptions,
    pub rts: RtsOptions,
    pub telis: TelisOptions,
}

#[cfg(test)]
impl DriverConfig {
    pub(crate) fn fake() -> Self {
        Self {
            kind: DriverKind::Fake,
            gpio: GpioOptions::default(),
            rts: RtsOptions::default(),
            telis: TelisOptions::default(),
        }
    }
}

#[cfg(test)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProtocolOperation {
    TelisSelection(Channel),
    FakeCommand { channel: Channel, command: Command },
}

#[derive(Debug)]
pub(crate) struct CommandRouter {
    executor: DriverExecutor,
}

#[derive(Debug)]
enum DriverExecutor {
    Fake(FakeDriver),
    Telis(TelisDriver),
    Rts(Box<RtsDriver>),
}

impl CommandRouter {
    pub async fn new(config: DriverConfig) -> Result<Self> {
        let executor = match config.kind {
            DriverKind::Fake => DriverExecutor::Fake(FakeDriver::new(Channel::L1)),
            DriverKind::Telis => {
                DriverExecutor::Telis(TelisDriver::new(config.gpio, config.telis).await?)
            }
            DriverKind::Rts => DriverExecutor::Rts(Box::new(RtsDriver::new(config.rts).await?)),
        };

        Ok(Self { executor })
    }

    pub async fn execute(&self, command: Command, channel: Option<Channel>) -> Result<()> {
        match &self.executor {
            DriverExecutor::Fake(driver) => driver.execute(command, channel).await,
            DriverExecutor::Telis(driver) => driver.execute(command, channel).await,
            DriverExecutor::Rts(driver) => driver.execute(command, channel).await,
        }
    }

    pub async fn execute_on(&self, channel: Channel, command: Command) -> Result<()> {
        match &self.executor {
            DriverExecutor::Fake(driver) => driver.execute_on(channel, command).await,
            DriverExecutor::Telis(driver) => driver.execute_on(channel, command).await,
            DriverExecutor::Rts(driver) => driver.execute_on(channel, command).await,
        }
    }

    pub fn selected_channel(&self) -> Channel {
        match &self.executor {
            DriverExecutor::Fake(driver) => driver.selected_channel(),
            DriverExecutor::Telis(driver) => driver.selected_channel(),
            DriverExecutor::Rts(driver) => driver.selected_channel(),
        }
    }

    pub fn subscribe_selected_channel(&self) -> SelectedChannelRx {
        match &self.executor {
            DriverExecutor::Fake(driver) => driver.subscribe_selected_channel(),
            DriverExecutor::Telis(driver) => driver.subscribe_selected_channel(),
            DriverExecutor::Rts(driver) => driver.subscribe_selected_channel(),
        }
    }

    #[cfg(test)]
    pub(crate) fn operations(&self) -> Vec<ProtocolOperation> {
        match &self.executor {
            DriverExecutor::Fake(driver) => driver.operations(),
            #[allow(unreachable_patterns)]
            _ => unreachable!("fake driver variant was not compiled"),
        }
    }
}

pub fn infer_position(command: Command) -> Option<u8> {
    match command {
        Command::Up => Some(100),
        Command::Down => Some(0),
        Command::Stop | Command::Select | Command::Prog => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_inference_only_tracks_directional_extremes() {
        assert_eq!(infer_position(Command::Up), Some(100));
        assert_eq!(infer_position(Command::Down), Some(0));
        assert_eq!(infer_position(Command::Stop), None);
        assert_eq!(infer_position(Command::Select), None);
        assert_eq!(infer_position(Command::Prog), None);
    }

    #[tokio::test]
    async fn rts_prog_transmits_pairing_waveform_without_changing_selection() {
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
        let state_path = dir.path().join(crate::rts::state::STATE_FILE);
        let rts_driver = rts::RtsDriver::new_for_test(
            RtsOptions::default(),
            &state_path,
            Arc::new(RecordingTransmitter {
                events: events.clone(),
            }),
        )
        .await
        .unwrap();
        let router = CommandRouter {
            executor: DriverExecutor::Rts(Box::new(rts_driver)),
        };

        router.execute_on(Channel::L3, Command::Prog).await.unwrap();

        assert_eq!(
            *events.lock().expect("recording events mutex"),
            vec![Event::RtsTransmit(Channel::L3, RtsCommand::Prog)]
        );
        let state: crate::rts::state::RtsState =
            serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(state.selected_channel, Channel::L1);
        assert_eq!(
            state.channels.get(&Channel::L3).unwrap().reserved_until,
            1 + crate::rts::state::DEFAULT_RESERVE_SIZE
        );
        assert_eq!(state.channels.get(&Channel::L1).unwrap().reserved_until, 1);
    }
}
