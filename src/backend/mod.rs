use anyhow::Result;
use tokio::sync::watch::Receiver;

use crate::gpio::Channel;
use crate::remote::Command;

#[cfg(all(feature = "fake", not(feature = "hw")))]
mod fake;
#[cfg(feature = "hw")]
mod telis;

#[cfg(all(feature = "fake", not(feature = "hw")))]
use fake::FakeBackend;
#[cfg(feature = "hw")]
use telis::TelisBackend;

pub type SelectedChannelRx = Receiver<Channel>;

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
pub(crate) enum ActiveBackend {
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

    #[test]
    fn position_inference_only_tracks_directional_extremes() {
        assert_eq!(infer_position(Command::Up), Some(100));
        assert_eq!(infer_position(Command::Down), Some(0));
        assert_eq!(infer_position(Command::My), None);
        assert_eq!(infer_position(Command::Stop), None);
        assert_eq!(infer_position(Command::Select), None);
    }
}
