//! Command validation and UI-style dispatch for HTTP/WS/CLI.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;

use crate::config::DriverKind;
use crate::controller::BlindController;
use crate::core::{Channel, Command};
use crate::driver::{CommandOutcome, TELIS_PROG_UNAVAILABLE};

/// Validated command ready for dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlRequest {
    Driver {
        command: Command,
        channel: Option<Channel>,
    },
    Position {
        channel: Option<Channel>,
        position: u8,
    },
}

/// HTTP/JSON command body (`POST /command`, WebSocket text, CLI remote POST).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandRequest {
    pub command: String,
    pub channel: Option<Channel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<u8>,
}

impl CommandRequest {
    pub(crate) fn from_control(request: ControlRequest) -> Self {
        match request {
            ControlRequest::Driver { command, channel } => Self {
                command: command.to_string(),
                channel,
                value: None,
            },
            ControlRequest::Position { channel, position } => Self {
                command: "target".to_string(),
                channel,
                value: Some(position),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandError {
    Invalid(String),
    PairingUnavailable,
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(msg) => write!(f, "{msg}"),
            Self::PairingUnavailable => write!(f, "{TELIS_PROG_UNAVAILABLE}"),
        }
    }
}

impl std::error::Error for CommandError {}

fn command_error(err: anyhow::Error) -> CommandError {
    CommandError::Invalid(format!("{err:?}"))
}

/// Validate a command request. Does not touch hardware.
fn parse_command(request: CommandRequest) -> Result<ControlRequest, CommandError> {
    let CommandRequest {
        command,
        channel,
        value,
    } = request;
    if command == "target" {
        let position = target_position_value(value)?;
        return Ok(ControlRequest::Position { channel, position });
    }

    if value.is_some() {
        return Err(CommandError::Invalid(
            "value is only valid with target".to_string(),
        ));
    }

    let cmd = Command::from_str(&command).map_err(|e| CommandError::Invalid(e.to_string()))?;
    let channel = match (cmd, channel) {
        (Command::Prog | Command::ProgLong, Some(channel)) => Some(channel),
        (Command::Prog | Command::ProgLong, None) => {
            return Err(CommandError::Invalid(
                "prog and prog_long require a channel".to_string(),
            ));
        }
        (Command::Select, channel) => channel,
        (Command::Up | Command::Down | Command::Stop, channel) => channel,
    };
    Ok(ControlRequest::Driver {
        command: cmd,
        channel,
    })
}

fn target_position_value(value: Option<u8>) -> Result<u8, CommandError> {
    match value {
        Some(position) if position <= 100 => Ok(position),
        Some(_) => Err(CommandError::Invalid(
            "target position must be between 0 and 100".to_string(),
        )),
        None => Err(CommandError::Invalid("target requires a value".to_string())),
    }
}

/// Reject pairing commands when the active driver cannot transmit them.
fn ensure_pairing_for_kind(kind: DriverKind, command: Command) -> Result<(), CommandError> {
    if matches!(command, Command::Prog | Command::ProgLong) && !kind.supports_pairing() {
        return Err(CommandError::PairingUnavailable);
    }
    Ok(())
}

/// Parse a command request and apply driver pairing rules. Does not touch hardware.
pub(crate) fn validate_command_request(
    kind: DriverKind,
    request: CommandRequest,
) -> Result<ControlRequest, CommandError> {
    let parsed = parse_command(request)?;
    validate_control_request(kind, parsed)
}

/// Apply driver pairing rules to an already-typed request. Does not touch hardware.
pub(crate) fn validate_control_request(
    kind: DriverKind,
    request: ControlRequest,
) -> Result<ControlRequest, CommandError> {
    if let ControlRequest::Driver { command, .. } = request {
        ensure_pairing_for_kind(kind, command)?;
    }
    Ok(request)
}

/// Validate and dispatch a command. `select` changes selection; action commands
/// with an explicit channel target that channel directly.
pub(crate) async fn dispatch_command(
    controller: &Arc<BlindController>,
    request: CommandRequest,
) -> Result<CommandOutcome, CommandError> {
    let parsed = validate_command_request(controller.driver_kind(), request)?;
    dispatch_control_request(controller, parsed).await
}

pub(crate) async fn dispatch_control_request(
    controller: &Arc<BlindController>,
    request: ControlRequest,
) -> Result<CommandOutcome, CommandError> {
    match request {
        ControlRequest::Driver {
            command: cmd,
            channel,
        } => controller
            .execute(cmd, channel)
            .await
            .with_context(|| format!("executing {cmd:?} command"))
            .map_err(command_error),
        ControlRequest::Position { channel, position } => {
            controller
                .set_target_for_channel(channel, position)
                .await
                .with_context(|| format!("executing target position to {position}%"))
                .map_err(command_error)?;
            Ok(CommandOutcome {
                inferred_position: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DriverConfig, DriverKind, PositioningOptions};
    use crate::driver::ProtocolOperation;
    use std::sync::Arc;

    fn parse(command: &str, channel: Option<Channel>) -> Result<ControlRequest, CommandError> {
        parse_command(CommandRequest {
            command: command.to_string(),
            channel,
            value: None,
        })
    }

    #[test]
    fn telis_rejects_pairing() {
        assert!(!DriverKind::Telis.supports_pairing());
        assert!(matches!(
            ensure_pairing_for_kind(DriverKind::Telis, Command::Prog),
            Err(CommandError::PairingUnavailable)
        ));
    }

    #[test]
    fn rts_supports_pairing() {
        assert!(DriverKind::Rts.supports_pairing());
        assert!(ensure_pairing_for_kind(DriverKind::Rts, Command::ProgLong).is_ok());
    }

    #[test]
    fn validate_rejects_telis_pairing_before_dispatch() {
        let err = validate_command_request(
            DriverKind::Telis,
            CommandRequest {
                command: "prog".to_string(),
                channel: Some(Channel::L1),
                value: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::PairingUnavailable));
    }

    #[test]
    fn parse_accepts_valid_commands() {
        for (wire, expected, channel) in [
            ("select", Command::Select, Some(Channel::L2)),
            ("select", Command::Select, None),
            ("up", Command::Up, Some(Channel::L1)),
            ("prog", Command::Prog, Some(Channel::L1)),
            ("prog_long", Command::ProgLong, Some(Channel::L1)),
        ] {
            let req = parse(wire, channel).unwrap();
            assert_eq!(
                req,
                ControlRequest::Driver {
                    command: expected,
                    channel
                },
                "{wire}"
            );
        }
    }

    #[test]
    fn parse_rejects_prog_without_channel() {
        let err = parse("prog", None).unwrap_err();
        assert!(matches!(err, CommandError::Invalid(_)));
        assert!(err.to_string().contains("require a channel"));
    }

    #[test]
    fn command_request_accepts_channel_field() {
        let req: CommandRequest =
            serde_json::from_str(r#"{"command":"up","channel":"L1"}"#).unwrap();

        assert_eq!(req.command, "up");
        assert_eq!(req.channel, Some(Channel::L1));
        assert_eq!(req.value, None);
    }

    #[test]
    fn command_request_rejects_legacy_led_field() {
        let err = serde_json::from_str::<CommandRequest>(r#"{"command":"select","led":"L1"}"#)
            .unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn parse_accepts_target_position_value() {
        for body in [
            r#"{"command":"target","channel":"L1","value":50}"#,
            r#"{"command":"target","value":50}"#,
        ] {
            let req = serde_json::from_str::<CommandRequest>(body).unwrap();
            let parsed = parse_command(req).unwrap();
            assert!(matches!(
                parsed,
                ControlRequest::Position { position: 50, .. }
            ));
        }
    }

    #[test]
    fn parse_preserves_optional_target_channel() {
        let with_channel = serde_json::from_str::<CommandRequest>(
            r#"{"command":"target","channel":"L1","value":50}"#,
        )
        .unwrap();
        let without_channel =
            serde_json::from_str::<CommandRequest>(r#"{"command":"target","value":50}"#).unwrap();

        assert_eq!(
            parse_command(with_channel).unwrap(),
            ControlRequest::Position {
                channel: Some(Channel::L1),
                position: 50,
            }
        );
        assert_eq!(
            parse_command(without_channel).unwrap(),
            ControlRequest::Position {
                channel: None,
                position: 50,
            }
        );
    }

    #[test]
    fn parse_rejects_target_without_position() {
        let req = serde_json::from_str::<CommandRequest>(r#"{"command":"target","channel":"L1"}"#)
            .unwrap();

        assert!(matches!(parse_command(req), Err(CommandError::Invalid(_))));
    }

    #[test]
    fn parse_rejects_value_on_button_command() {
        let req =
            serde_json::from_str::<CommandRequest>(r#"{"command":"up","channel":"L1","value":50}"#)
                .unwrap();

        let err = parse_command(req).unwrap_err();
        assert!(err.to_string().contains("value is only valid"));
    }

    #[test]
    fn command_request_rejects_param_field() {
        let err = serde_json::from_str::<CommandRequest>(r#"{"command":"target","param":50}"#)
            .unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[tokio::test]
    async fn dispatch_target_without_channel_uses_current_selection() {
        let controller = Arc::new(
            BlindController::with_driver(DriverConfig::fake(), PositioningOptions::default())
                .await
                .unwrap(),
        );
        controller
            .execute(Command::Select, Some(Channel::L2))
            .await
            .unwrap();

        dispatch_command(
            &controller,
            CommandRequest {
                command: "target".to_string(),
                channel: None,
                value: Some(50),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            controller.operations(),
            vec![
                ProtocolOperation::TelisSelection(Channel::L2),
                ProtocolOperation::FakeCommand {
                    channel: Channel::L2,
                    command: Command::Down,
                },
            ]
        );
    }
}
