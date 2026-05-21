//! Application service: wire-format validation and UI-style command dispatch.

use anyhow::{Context, Result};
use std::str::FromStr;
use std::sync::Arc;

use crate::controller::BlindController;
use crate::core::{Channel, Command};
use crate::driver::{CommandOutcome, DriverKind, SelectedChannelRx, TELIS_PROG_UNAVAILABLE};

/// Parsed HTTP/WebSocket/CLI press ready for dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PressRequest {
    pub command: Command,
    pub channel: Option<Channel>,
}

/// Wire-format command body (HTTP JSON / WebSocket text).
#[derive(Debug, Clone)]
pub struct WirePress {
    pub command: String,
    pub channel: Option<Channel>,
    pub long: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PressError {
    Invalid(String),
    PairingUnavailable,
}

impl std::fmt::Display for PressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(msg) => write!(f, "{msg}"),
            Self::PairingUnavailable => write!(f, "{TELIS_PROG_UNAVAILABLE}"),
        }
    }
}

impl std::error::Error for PressError {}

fn press_error(err: anyhow::Error) -> PressError {
    PressError::Invalid(format!("{err:?}"))
}

/// Central dispatch for REST, WebSocket, and in-process callers.
#[derive(Debug)]
pub struct BlindService {
    controller: Arc<BlindController>,
    kind: DriverKind,
}

impl BlindService {
    pub fn new(controller: Arc<BlindController>, kind: DriverKind) -> Self {
        Self { controller, kind }
    }

    pub fn driver_kind(&self) -> DriverKind {
        self.kind
    }

    pub fn current_selection(&self) -> Channel {
        self.controller.current_selection()
    }

    pub fn subscribe_selection(&self) -> SelectedChannelRx {
        self.controller.subscribe_selection()
    }

    /// Validate a wire-format press. Does not touch hardware.
    pub fn parse_wire(wire: WirePress) -> Result<PressRequest, PressError> {
        let WirePress {
            command,
            channel,
            long,
        } = wire;
        let mut cmd =
            Command::from_str(&command).map_err(|e| PressError::Invalid(e.to_string()))?;
        if long {
            match cmd {
                Command::Prog => cmd = Command::ProgLong,
                Command::ProgLong => {}
                _ => {
                    return Err(PressError::Invalid(
                        "`long` is only valid with prog".to_string(),
                    ));
                }
            }
        }
        let channel = match (cmd, channel) {
            (Command::Prog | Command::ProgLong, Some(channel)) => Some(channel),
            (Command::Prog | Command::ProgLong, None) => {
                return Err(PressError::Invalid("prog requires a channel".to_string()));
            }
            (Command::Select, channel) => channel,
            (Command::Up | Command::Down | Command::Stop, channel) => channel,
        };
        Ok(PressRequest {
            command: cmd,
            channel,
        })
    }

    /// Reject pairing commands when the active driver cannot transmit them.
    pub fn ensure_pairing_allowed(&self, command: Command) -> Result<(), PressError> {
        Self::ensure_pairing_for_kind(self.kind, command)
    }

    pub fn ensure_pairing_for_kind(kind: DriverKind, command: Command) -> Result<(), PressError> {
        if matches!(command, Command::Prog | Command::ProgLong) && !kind.supports_pairing() {
            return Err(PressError::PairingUnavailable);
        }
        Ok(())
    }

    /// UI-style dispatch: optional channel selects before directional commands.
    pub async fn press(&self, request: PressRequest) -> Result<CommandOutcome, PressError> {
        self.ensure_pairing_allowed(request.command)?;
        let PressRequest {
            command: cmd,
            channel,
        } = request;
        if cmd == Command::Select {
            return self
                .controller
                .execute(cmd, channel)
                .await
                .context("executing select command")
                .map_err(press_error);
        }

        if let Some(channel) = channel {
            self.controller
                .execute(Command::Select, Some(channel))
                .await
                .context("selecting channel before command")
                .map_err(press_error)?;
        }
        self.controller
            .execute(cmd, None)
            .await
            .with_context(|| format!("executing {cmd:?} command"))
            .map_err(press_error)
    }

    pub async fn press_wire(&self, wire: WirePress) -> Result<CommandOutcome, PressError> {
        let request = Self::parse_wire(wire)?;
        self.press(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::BlindController;
    use crate::driver::{DriverKind, ProtocolOperation};

    fn parse(
        command: &str,
        channel: Option<Channel>,
        long: bool,
    ) -> Result<PressRequest, PressError> {
        BlindService::parse_wire(WirePress {
            command: command.to_string(),
            channel,
            long,
        })
    }

    #[test]
    fn telis_rejects_pairing() {
        assert!(!DriverKind::Telis.supports_pairing());
        assert!(matches!(
            BlindService::ensure_pairing_for_kind(DriverKind::Telis, Command::Prog),
            Err(PressError::PairingUnavailable)
        ));
    }

    #[test]
    fn rts_supports_pairing() {
        assert!(DriverKind::Rts.supports_pairing());
        assert!(BlindService::ensure_pairing_for_kind(DriverKind::Rts, Command::ProgLong).is_ok());
    }

    #[test]
    fn parse_accepts_select_with_channel() {
        let req = parse("select", Some(Channel::L2), false).unwrap();
        assert_eq!(req.command, Command::Select);
        assert_eq!(req.channel, Some(Channel::L2));
    }

    #[test]
    fn parse_accepts_select_without_channel() {
        let req = parse("select", None, false).unwrap();
        assert_eq!(req.command, Command::Select);
        assert_eq!(req.channel, None);
    }

    #[test]
    fn parse_accepts_directional_channel() {
        let req = parse("up", Some(Channel::L1), false).unwrap();
        assert_eq!(req.command, Command::Up);
        assert_eq!(req.channel, Some(Channel::L1));
    }

    #[test]
    fn parse_rejects_prog_without_channel() {
        let err = parse("prog", None, false).unwrap_err();
        assert!(matches!(err, PressError::Invalid(_)));
        assert!(err.to_string().contains("requires a channel"));
    }

    #[test]
    fn parse_accepts_prog_with_channel() {
        let req = parse("prog", Some(Channel::L1), false).unwrap();
        assert_eq!(req.command, Command::Prog);
        assert_eq!(req.channel, Some(Channel::L1));
    }

    #[test]
    fn parse_promotes_prog_with_long_to_prog_long() {
        let req = parse("prog", Some(Channel::L1), true).unwrap();
        assert_eq!(req.command, Command::ProgLong);
        assert_eq!(req.channel, Some(Channel::L1));
    }

    #[test]
    fn parse_rejects_long_on_non_prog_commands() {
        let err = parse("up", Some(Channel::L1), true).unwrap_err();
        assert!(err.to_string().contains("only valid with prog"));
    }

    #[tokio::test]
    async fn press_with_channel_selects_then_executes() {
        let controller = Arc::new(
            BlindController::with_driver(crate::driver::DriverConfig::fake())
                .await
                .unwrap(),
        );
        let blinds = Arc::new(BlindService::new(controller.clone(), DriverKind::Fake));

        blinds
            .press(PressRequest {
                command: Command::Up,
                channel: Some(Channel::L3),
            })
            .await
            .unwrap();

        assert_eq!(controller.current_selection(), Channel::L3);
        assert_eq!(
            controller.operations(),
            vec![
                ProtocolOperation::TelisSelection(Channel::L3),
                ProtocolOperation::FakeCommand {
                    channel: Channel::L3,
                    command: Command::Up,
                },
            ]
        );
    }
}
