use anyhow::Result;
use clap::ValueEnum;
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

pub type SelectedChannelRx = Receiver<Channel>;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CommandOutcome {
    pub inferred_position: Option<u8>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendConfig {
    pub kind: BackendKind,
    pub rts: RtsOptions,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            kind: BackendKind::Fake,
            rts: RtsOptions::default(),
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
pub(crate) enum ActiveBackend {
    #[cfg(feature = "fake")]
    Fake(FakeBackend),
    #[cfg(feature = "telis")]
    Telis(TelisBackend),
    #[cfg(feature = "rts")]
    Rts(RtsBackend),
}

impl ActiveBackend {
    pub async fn new(config: BackendConfig) -> Result<Self> {
        match config.kind {
            BackendKind::Fake => {
                #[cfg(feature = "fake")]
                {
                    Ok(Self::Fake(FakeBackend::new(Channel::L1)))
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
                    Ok(Self::Telis(TelisBackend::new().await?))
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
                    Ok(Self::Rts(RtsBackend::new(config.rts).await?))
                }
                #[cfg(not(feature = "rts"))]
                {
                    anyhow::bail!(
                        "backend \"rts\" was selected, but this binary was built without the \"rts\" feature"
                    )
                }
            }
        }
    }

    pub async fn execute(&self, command: Command, channel: Option<Channel>) -> Result<()> {
        let _ = (command, channel);
        match self {
            #[cfg(feature = "fake")]
            Self::Fake(backend) => backend.execute(command, channel).await,
            #[cfg(feature = "telis")]
            Self::Telis(backend) => backend.execute(command, channel).await,
            #[cfg(feature = "rts")]
            Self::Rts(backend) => backend.execute(command, channel).await,
            #[allow(unreachable_patterns)]
            _ => unreachable!("no backend variants were compiled"),
        }
    }

    pub async fn execute_on(&self, channel: Channel, command: Command) -> Result<()> {
        let _ = (channel, command);
        match self {
            #[cfg(feature = "fake")]
            Self::Fake(backend) => backend.execute_on(channel, command).await,
            #[cfg(feature = "telis")]
            Self::Telis(backend) => backend.execute_on(channel, command).await,
            #[cfg(feature = "rts")]
            Self::Rts(backend) => backend.execute_on(channel, command).await,
            #[allow(unreachable_patterns)]
            _ => unreachable!("no backend variants were compiled"),
        }
    }

    pub fn selected_channel(&self) -> Channel {
        match self {
            #[cfg(feature = "fake")]
            Self::Fake(backend) => backend.selected_channel(),
            #[cfg(feature = "telis")]
            Self::Telis(backend) => backend.selected_channel(),
            #[cfg(feature = "rts")]
            Self::Rts(backend) => backend.selected_channel(),
            #[allow(unreachable_patterns)]
            _ => unreachable!("no backend variants were compiled"),
        }
    }

    pub fn subscribe_selected_channel(&self) -> SelectedChannelRx {
        match self {
            #[cfg(feature = "fake")]
            Self::Fake(backend) => backend.subscribe_selected_channel(),
            #[cfg(feature = "telis")]
            Self::Telis(backend) => backend.subscribe_selected_channel(),
            #[cfg(feature = "rts")]
            Self::Rts(backend) => backend.subscribe_selected_channel(),
            #[allow(unreachable_patterns)]
            _ => unreachable!("no backend variants were compiled"),
        }
    }

    #[cfg(all(test, feature = "fake"))]
    pub(crate) fn operations(&self) -> Vec<ProtocolOperation> {
        match self {
            Self::Fake(backend) => backend.operations(),
            #[allow(unreachable_patterns)]
            _ => unreachable!("fake backend variant was not compiled"),
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
