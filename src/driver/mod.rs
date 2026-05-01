use anyhow::Result;
use clap::ValueEnum;
#[cfg(all(feature = "rts", feature = "telis"))]
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::fmt;
#[cfg(all(feature = "rts", feature = "telis"))]
use std::sync::Arc;
use tokio::sync::watch::Receiver;

use crate::gpio::Channel;
use crate::remote::Command;

#[cfg(feature = "fake")]
mod fake;
#[cfg(feature = "rts")]
mod rts;
#[cfg(feature = "telis")]
mod telis;

#[cfg(feature = "fake")]
use fake::FakeDriver;
#[cfg(feature = "rts")]
pub(crate) use rts::require_loopback as require_pigpiod_loopback;
#[cfg(feature = "rts")]
use rts::RtsDriver;
#[cfg(feature = "telis")]
use telis::TelisDriver;
#[cfg(all(feature = "rts", feature = "telis"))]
use telis::TelisProgrammer;

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
#[serde(default)]
pub struct RtsOptions {
    pub spi_device: String,
    pub gdo0_gpio: u8,
    pub pigpiod_addr: String,
}

impl Default for RtsOptions {
    fn default() -> Self {
        Self {
            spi_device: "/dev/spidev0.0".to_string(),
            gdo0_gpio: 18,
            pigpiod_addr: "127.0.0.1:8888".to_string(),
        }
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
    pub rts: RtsOptions,
    pub telis: TelisOptions,
}

impl Default for DriverConfig {
    fn default() -> Self {
        Self {
            kind: DriverKind::Fake,
            rts: RtsOptions::default(),
            telis: TelisOptions::default(),
        }
    }
}

#[cfg(all(test, feature = "fake"))]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProtocolOperation {
    TelisSelection(Channel),
    FakeCommand { channel: Channel, command: Command },
}

#[derive(Debug)]
pub(crate) struct CommandRouter {
    executor: DriverExecutor,
    #[cfg(all(feature = "rts", feature = "telis"))]
    telis_programmer: Option<Arc<dyn Programmer>>,
}

#[derive(Debug)]
enum DriverExecutor {
    #[cfg(feature = "fake")]
    Fake(FakeDriver),
    #[cfg(feature = "telis")]
    Telis(TelisDriver),
    #[cfg(feature = "rts")]
    Rts(Box<RtsDriver>),
}

impl CommandRouter {
    pub async fn new(config: DriverConfig) -> Result<Self> {
        #[cfg(feature = "rts")]
        let has_telis_prog = config.telis.gpio.prog.is_some();
        #[cfg(all(feature = "rts", feature = "telis"))]
        let use_telis_programmer = config.kind == DriverKind::Rts && has_telis_prog;
        #[cfg(all(feature = "rts", feature = "telis"))]
        let telis_programmer_options = config.telis.clone();
        let executor = match config.kind {
            DriverKind::Fake => {
                #[cfg(feature = "fake")]
                {
                    DriverExecutor::Fake(FakeDriver::new(Channel::L1))
                }
                #[cfg(not(feature = "fake"))]
                {
                    anyhow::bail!(
                        "driver \"fake\" was selected, but this binary was built without the \"fake\" feature"
                    )
                }
            }
            DriverKind::Telis => {
                #[cfg(feature = "telis")]
                {
                    DriverExecutor::Telis(TelisDriver::new(config.telis).await?)
                }
                #[cfg(not(feature = "telis"))]
                {
                    anyhow::bail!(
                        "driver \"telis\" was selected, but this binary was built without the \"telis\" feature"
                    )
                }
            }
            DriverKind::Rts => {
                #[cfg(feature = "rts")]
                {
                    if has_telis_prog {
                        #[cfg(not(feature = "telis"))]
                        anyhow::bail!(
                            "telis.gpio.prog is configured, but this binary was built without the \"telis\" feature"
                        );
                    }
                    DriverExecutor::Rts(Box::new(RtsDriver::new(config.rts).await?))
                }
                #[cfg(not(feature = "rts"))]
                {
                    anyhow::bail!(
                        "driver \"rts\" was selected, but this binary was built without the \"rts\" feature"
                    )
                }
            }
        };

        Ok(Self {
            executor,
            #[cfg(all(feature = "rts", feature = "telis"))]
            telis_programmer: use_telis_programmer
                .then(|| Arc::new(TelisProgrammer::new(telis_programmer_options)) as Arc<_>),
        })
    }

    pub async fn execute(&self, command: Command, channel: Option<Channel>) -> Result<()> {
        let _ = (command, channel);
        match &self.executor {
            #[cfg(feature = "fake")]
            DriverExecutor::Fake(driver) => driver.execute(command, channel).await,
            #[cfg(feature = "telis")]
            DriverExecutor::Telis(driver) => driver.execute(command, channel).await,
            #[cfg(feature = "rts")]
            DriverExecutor::Rts(driver) => {
                if command == Command::Prog {
                    self.program_rts(driver.selected_channel()).await
                } else {
                    driver.execute(command, channel).await
                }
            }
            #[allow(unreachable_patterns)]
            _ => unreachable!("no driver variants were compiled"),
        }
    }

    pub async fn execute_on(&self, channel: Channel, command: Command) -> Result<()> {
        let _ = (channel, command);
        match &self.executor {
            #[cfg(feature = "fake")]
            DriverExecutor::Fake(driver) => driver.execute_on(channel, command).await,
            #[cfg(feature = "telis")]
            DriverExecutor::Telis(driver) => driver.execute_on(channel, command).await,
            #[cfg(feature = "rts")]
            DriverExecutor::Rts(driver) => {
                if command == Command::Prog {
                    self.program_rts(channel).await
                } else {
                    driver.execute_on(channel, command).await
                }
            }
            #[allow(unreachable_patterns)]
            _ => unreachable!("no driver variants were compiled"),
        }
    }

    pub fn selected_channel(&self) -> Channel {
        match &self.executor {
            #[cfg(feature = "fake")]
            DriverExecutor::Fake(driver) => driver.selected_channel(),
            #[cfg(feature = "telis")]
            DriverExecutor::Telis(driver) => driver.selected_channel(),
            #[cfg(feature = "rts")]
            DriverExecutor::Rts(driver) => driver.selected_channel(),
            #[allow(unreachable_patterns)]
            _ => unreachable!("no driver variants were compiled"),
        }
    }

    pub fn subscribe_selected_channel(&self) -> SelectedChannelRx {
        match &self.executor {
            #[cfg(feature = "fake")]
            DriverExecutor::Fake(driver) => driver.subscribe_selected_channel(),
            #[cfg(feature = "telis")]
            DriverExecutor::Telis(driver) => driver.subscribe_selected_channel(),
            #[cfg(feature = "rts")]
            DriverExecutor::Rts(driver) => driver.subscribe_selected_channel(),
            #[allow(unreachable_patterns)]
            _ => unreachable!("no driver variants were compiled"),
        }
    }

    #[cfg(all(test, feature = "fake"))]
    pub(crate) fn operations(&self) -> Vec<ProtocolOperation> {
        match &self.executor {
            DriverExecutor::Fake(driver) => driver.operations(),
            #[allow(unreachable_patterns)]
            _ => unreachable!("fake driver variant was not compiled"),
        }
    }

    #[cfg(feature = "rts")]
    async fn program_rts(&self, channel: Channel) -> Result<()> {
        #[cfg(feature = "telis")]
        if let Some(programmer) = &self.telis_programmer {
            tracing::info!(%channel, "starting Telis-assisted RTS programming");
            programmer.program(channel).await?;
            tracing::info!(%channel, "Telis-assisted RTS programming handoff complete");
        }

        match &self.executor {
            DriverExecutor::Rts(driver) => driver.execute_on(channel, Command::Prog).await,
            #[allow(unreachable_patterns)]
            _ => unreachable!("RTS programming requires the RTS executor"),
        }
    }
}

#[cfg(all(feature = "rts", feature = "telis"))]
trait Programmer: fmt::Debug + Send + Sync + 'static {
    fn program(&self, channel: Channel) -> BoxFuture<'_, Result<()>>;
}

#[cfg(all(feature = "rts", feature = "telis"))]
impl Programmer for TelisProgrammer {
    fn program(&self, channel: Channel) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move { TelisProgrammer::program(self, channel).await })
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

    #[cfg(all(feature = "rts", feature = "telis"))]
    #[tokio::test]
    async fn rts_prog_runs_telis_programmer_before_pairing_waveform() {
        use crate::rts::frame::RtsCommand;
        use std::sync::{Arc, Mutex as StdMutex};

        #[derive(Copy, Clone, Debug, PartialEq, Eq)]
        enum Event {
            TelisProg(Channel),
            RtsTransmit(Channel, RtsCommand),
        }

        #[derive(Debug)]
        struct RecordingProgrammer {
            events: Arc<StdMutex<Vec<Event>>>,
        }

        impl Programmer for RecordingProgrammer {
            fn program(&self, channel: Channel) -> BoxFuture<'_, Result<()>> {
                Box::pin(async move {
                    self.events
                        .lock()
                        .expect("recording programmer mutex")
                        .push(Event::TelisProg(channel));
                    Ok(())
                })
            }
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
            RtsOptions {
                gdo0_gpio: 18,
                ..RtsOptions::default()
            },
            &state_path,
            Arc::new(RecordingTransmitter {
                events: events.clone(),
            }),
        )
        .await
        .unwrap();
        let router = CommandRouter {
            executor: DriverExecutor::Rts(Box::new(rts_driver)),
            telis_programmer: Some(Arc::new(RecordingProgrammer {
                events: events.clone(),
            })),
        };

        router.execute_on(Channel::L3, Command::Prog).await.unwrap();

        assert_eq!(
            *events.lock().expect("recording events mutex"),
            vec![
                Event::TelisProg(Channel::L3),
                Event::RtsTransmit(Channel::L3, RtsCommand::Prog),
            ]
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
