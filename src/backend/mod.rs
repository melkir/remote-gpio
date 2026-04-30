use anyhow::Result;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;
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
use fake::FakeBackend;
#[cfg(feature = "rts")]
use rts::RtsBackend;
#[cfg(feature = "telis")]
use telis::TelisBackend;
#[cfg(all(feature = "rts", feature = "telis"))]
use telis::TelisProgrammer;

pub type SelectedChannelRx = Receiver<Channel>;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CommandOutcome {
    pub inferred_position: Option<u8>,
}

#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Fake,
    Telis,
    Rts,
}

impl fmt::Display for BackendKind {
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
    pub frame_count: usize,
}

impl Default for RtsOptions {
    fn default() -> Self {
        Self {
            spi_device: "/dev/spidev0.0".to_string(),
            gdo0_gpio: 18,
            pigpiod_addr: "127.0.0.1:8888".to_string(),
            frame_count: crate::rts::waveform::DEFAULT_FRAME_COUNT,
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
pub struct BackendConfig {
    pub kind: BackendKind,
    pub rts: RtsOptions,
    pub telis: TelisOptions,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            kind: BackendKind::Fake,
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
    executor: ModeExecutor,
    #[cfg(all(feature = "rts", feature = "telis"))]
    telis_programmer: Option<TelisProgrammer>,
}

#[derive(Debug)]
enum ModeExecutor {
    #[cfg(feature = "fake")]
    Fake(FakeBackend),
    #[cfg(feature = "telis")]
    Telis(TelisBackend),
    #[cfg(feature = "rts")]
    Rts(Box<RtsBackend>),
}

impl CommandRouter {
    pub async fn new(config: BackendConfig) -> Result<Self> {
        #[cfg(feature = "rts")]
        let has_telis_prog = config.telis.gpio.prog.is_some();
        #[cfg(all(feature = "rts", feature = "telis"))]
        let use_telis_programmer = config.kind == BackendKind::Rts && has_telis_prog;
        #[cfg(all(feature = "rts", feature = "telis"))]
        let telis_programmer_options = config.telis.clone();
        let executor = match config.kind {
            BackendKind::Fake => {
                #[cfg(feature = "fake")]
                {
                    ModeExecutor::Fake(FakeBackend::new(Channel::L1))
                }
                #[cfg(not(feature = "fake"))]
                {
                    anyhow::bail!(
                        "backend \"fake\" was selected, but this binary was built without the \"fake\" feature"
                    )
                }
            }
            BackendKind::Telis => {
                #[cfg(feature = "telis")]
                {
                    ModeExecutor::Telis(TelisBackend::new(config.telis).await?)
                }
                #[cfg(not(feature = "telis"))]
                {
                    anyhow::bail!(
                        "backend \"telis\" was selected, but this binary was built without the \"telis\" feature"
                    )
                }
            }
            BackendKind::Rts => {
                #[cfg(feature = "rts")]
                {
                    if has_telis_prog {
                        #[cfg(not(feature = "telis"))]
                        anyhow::bail!(
                            "telis.gpio.prog is configured, but this binary was built without the \"telis\" feature"
                        );
                    }
                    ModeExecutor::Rts(Box::new(RtsBackend::new(config.rts).await?))
                }
                #[cfg(not(feature = "rts"))]
                {
                    anyhow::bail!(
                        "backend \"rts\" was selected, but this binary was built without the \"rts\" feature"
                    )
                }
            }
        };

        Ok(Self {
            executor,
            #[cfg(all(feature = "rts", feature = "telis"))]
            telis_programmer: use_telis_programmer
                .then(|| TelisProgrammer::new(telis_programmer_options)),
        })
    }

    pub async fn execute(&self, command: Command, channel: Option<Channel>) -> Result<()> {
        let _ = (command, channel);
        match &self.executor {
            #[cfg(feature = "fake")]
            ModeExecutor::Fake(backend) => backend.execute(command, channel).await,
            #[cfg(feature = "telis")]
            ModeExecutor::Telis(backend) => backend.execute(command, channel).await,
            #[cfg(feature = "rts")]
            ModeExecutor::Rts(backend) => {
                if command == Command::Prog {
                    self.program_rts(backend.selected_channel()).await
                } else {
                    backend.execute(command, channel).await
                }
            }
            #[allow(unreachable_patterns)]
            _ => unreachable!("no backend variants were compiled"),
        }
    }

    pub async fn execute_on(&self, channel: Channel, command: Command) -> Result<()> {
        let _ = (channel, command);
        match &self.executor {
            #[cfg(feature = "fake")]
            ModeExecutor::Fake(backend) => backend.execute_on(channel, command).await,
            #[cfg(feature = "telis")]
            ModeExecutor::Telis(backend) => backend.execute_on(channel, command).await,
            #[cfg(feature = "rts")]
            ModeExecutor::Rts(backend) => {
                if command == Command::Prog {
                    self.program_rts(channel).await
                } else {
                    backend.execute_on(channel, command).await
                }
            }
            #[allow(unreachable_patterns)]
            _ => unreachable!("no backend variants were compiled"),
        }
    }

    pub fn selected_channel(&self) -> Channel {
        match &self.executor {
            #[cfg(feature = "fake")]
            ModeExecutor::Fake(backend) => backend.selected_channel(),
            #[cfg(feature = "telis")]
            ModeExecutor::Telis(backend) => backend.selected_channel(),
            #[cfg(feature = "rts")]
            ModeExecutor::Rts(backend) => backend.selected_channel(),
            #[allow(unreachable_patterns)]
            _ => unreachable!("no backend variants were compiled"),
        }
    }

    pub fn subscribe_selected_channel(&self) -> SelectedChannelRx {
        match &self.executor {
            #[cfg(feature = "fake")]
            ModeExecutor::Fake(backend) => backend.subscribe_selected_channel(),
            #[cfg(feature = "telis")]
            ModeExecutor::Telis(backend) => backend.subscribe_selected_channel(),
            #[cfg(feature = "rts")]
            ModeExecutor::Rts(backend) => backend.subscribe_selected_channel(),
            #[allow(unreachable_patterns)]
            _ => unreachable!("no backend variants were compiled"),
        }
    }

    #[cfg(all(test, feature = "fake"))]
    pub(crate) fn operations(&self) -> Vec<ProtocolOperation> {
        match &self.executor {
            ModeExecutor::Fake(backend) => backend.operations(),
            #[allow(unreachable_patterns)]
            _ => unreachable!("fake backend variant was not compiled"),
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
            ModeExecutor::Rts(backend) => backend.execute_on(channel, Command::Prog).await,
            #[allow(unreachable_patterns)]
            _ => unreachable!("RTS programming requires the RTS executor"),
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
}
