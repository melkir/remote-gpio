//! Command validation and UI-style dispatch for HTTP/WS/CLI.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::config::DriverKind;
use crate::controller::BlindController;
use crate::core::{Channel, Command};
use crate::driver::{CommandOutcome, TELIS_PROG_UNAVAILABLE};

/// Parsed command ready for dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedCommandRequest {
    pub command: Command,
    pub channel: Option<Channel>,
}

/// HTTP/JSON command body (`POST /command`, WebSocket text, CLI remote POST).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRequest {
    pub command: String,
    pub channel: Option<Channel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
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
pub fn parse_command(request: CommandRequest) -> Result<ParsedCommandRequest, CommandError> {
    let CommandRequest { command, channel } = request;
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
    Ok(ParsedCommandRequest {
        command: cmd,
        channel,
    })
}

/// Reject pairing commands when the active driver cannot transmit them.
pub fn ensure_pairing_for_kind(kind: DriverKind, command: Command) -> Result<(), CommandError> {
    if matches!(command, Command::Prog | Command::ProgLong) && !kind.supports_pairing() {
        return Err(CommandError::PairingUnavailable);
    }
    Ok(())
}

/// Parse a wire request and apply driver pairing rules. Does not touch hardware.
pub fn validate_command_request(
    kind: DriverKind,
    request: CommandRequest,
) -> Result<ParsedCommandRequest, CommandError> {
    let parsed = parse_command(request)?;
    ensure_pairing_for_kind(kind, parsed.command)?;
    Ok(parsed)
}

/// Validate and dispatch a command. `select` changes selection; action commands
/// with an explicit channel target that channel directly.
pub async fn dispatch_command(
    controller: &BlindController,
    request: CommandRequest,
) -> Result<CommandOutcome, CommandError> {
    let parsed = validate_command_request(controller.driver_kind(), request)?;
    let ParsedCommandRequest {
        command: cmd,
        channel,
    } = parsed;
    controller
        .execute(cmd, channel)
        .await
        .with_context(|| format!("executing {cmd:?} command"))
        .map_err(command_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DriverKind;

    fn parse(
        command: &str,
        channel: Option<Channel>,
    ) -> Result<ParsedCommandRequest, CommandError> {
        parse_command(CommandRequest {
            command: command.to_string(),
            channel,
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
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::PairingUnavailable));
    }

    #[test]
    fn parse_accepts_select_with_channel() {
        let req = parse("select", Some(Channel::L2)).unwrap();
        assert_eq!(req.command, Command::Select);
        assert_eq!(req.channel, Some(Channel::L2));
    }

    #[test]
    fn parse_accepts_select_without_channel() {
        let req = parse("select", None).unwrap();
        assert_eq!(req.command, Command::Select);
        assert_eq!(req.channel, None);
    }

    #[test]
    fn parse_accepts_directional_channel() {
        let req = parse("up", Some(Channel::L1)).unwrap();
        assert_eq!(req.command, Command::Up);
        assert_eq!(req.channel, Some(Channel::L1));
    }

    #[test]
    fn parse_rejects_prog_without_channel() {
        let err = parse("prog", None).unwrap_err();
        assert!(matches!(err, CommandError::Invalid(_)));
        assert!(err.to_string().contains("require a channel"));
    }

    #[test]
    fn parse_accepts_prog_with_channel() {
        let req = parse("prog", Some(Channel::L1)).unwrap();
        assert_eq!(req.command, Command::Prog);
        assert_eq!(req.channel, Some(Channel::L1));
    }

    #[test]
    fn parse_accepts_prog_long_with_channel() {
        let req = parse("prog_long", Some(Channel::L1)).unwrap();
        assert_eq!(req.command, Command::ProgLong);
        assert_eq!(req.channel, Some(Channel::L1));
    }

    #[test]
    fn command_request_accepts_channel_field() {
        let req: CommandRequest =
            serde_json::from_str(r#"{"command":"up","channel":"L1"}"#).unwrap();

        assert_eq!(req.command, "up");
        assert_eq!(req.channel, Some(Channel::L1));
    }

    #[test]
    fn command_request_accepts_prog_long() {
        let req: CommandRequest =
            serde_json::from_str(r#"{"command":"prog_long","channel":"L1"}"#).unwrap();
        assert_eq!(req.command, "prog_long");
        assert_eq!(req.channel, Some(Channel::L1));
    }

    #[test]
    fn command_request_rejects_legacy_led_field() {
        let err = serde_json::from_str::<CommandRequest>(r#"{"command":"select","led":"L1"}"#)
            .unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }
}
