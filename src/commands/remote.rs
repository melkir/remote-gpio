use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::Serialize;
use std::path::PathBuf;

use crate::cli::RemoteCommand;
use crate::config;
use crate::core::{Channel, Command};
use crate::service::{BlindService, WirePress};

const SERVICE_BASE_URL: &str = "http://127.0.0.1:5002";

pub async fn run(command: RemoteCommand, config_path: Option<PathBuf>) -> Result<()> {
    match command {
        RemoteCommand::Up { channel } => post_command("up", channel, false, config_path).await,
        RemoteCommand::Down { channel } => post_command("down", channel, false, config_path).await,
        RemoteCommand::Stop { channel } => post_command("stop", channel, false, config_path).await,
        RemoteCommand::Select { channel } => {
            post_command("select", Some(channel), false, config_path).await
        }
        RemoteCommand::Prog { channel, long } => {
            post_command("prog", Some(channel), long, config_path).await
        }
        RemoteCommand::Status => status().await,
        RemoteCommand::Watch => watch().await,
    }
}

#[derive(Serialize)]
struct CommandRequest {
    command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<Channel>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    long: bool,
}

fn ensure_pairing_allowed(config_path: Option<PathBuf>) -> Result<()> {
    let resolved = config::resolve(config_path)?;
    BlindService::ensure_pairing_for_kind(resolved.config.driver, Command::Prog)?;
    Ok(())
}

async fn post_command(
    command: &'static str,
    channel: Option<Channel>,
    long: bool,
    config_path: Option<PathBuf>,
) -> Result<()> {
    if command == "prog" {
        ensure_pairing_allowed(config_path)?;
        let wire = WirePress {
            command: command.to_string(),
            channel,
            long,
        };
        BlindService::parse_wire(wire)?;
    }

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{SERVICE_BASE_URL}/command"))
        .json(&CommandRequest {
            command,
            channel,
            long,
        })
        .send()
        .await
        .context("connecting to somfy service at 127.0.0.1:5002")?;

    if response.status().is_success() {
        return Ok(());
    }

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    bail!("service rejected {command}: HTTP {status}: {}", body.trim());
}

async fn status() -> Result<()> {
    let text = reqwest::get(format!("{SERVICE_BASE_URL}/channel"))
        .await
        .context("connecting to somfy service at 127.0.0.1:5002")?
        .error_for_status()
        .context("reading selected channel from somfy service")?
        .text()
        .await?;
    println!("{}", text.trim());
    Ok(())
}

async fn watch() -> Result<()> {
    let response = reqwest::get(format!("{SERVICE_BASE_URL}/events"))
        .await
        .context("connecting to somfy service at 127.0.0.1:5002")?
        .error_for_status()
        .context("opening somfy service event stream")?;
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(index) = buffer.find('\n') {
            let line = buffer[..index].trim_end_matches('\r').to_string();
            buffer.drain(..=index);
            if let Some(data) = line.strip_prefix("data:") {
                println!("{}", data.trim());
            }
        }
    }
    Ok(())
}
