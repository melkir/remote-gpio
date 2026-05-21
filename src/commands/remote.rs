use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use std::path::PathBuf;

use crate::cli::RemoteCommand;
use crate::config;
use crate::core::{Channel, Command};
use crate::service::{BlindService, CommandRequest};

const SERVICE_BASE_URL: &str = "http://127.0.0.1:5002";

pub async fn run(command: RemoteCommand, config_path: Option<PathBuf>) -> Result<()> {
    match command {
        RemoteCommand::Up { channel } => post_command("up", channel, config_path).await,
        RemoteCommand::Down { channel } => post_command("down", channel, config_path).await,
        RemoteCommand::Stop { channel } => post_command("stop", channel, config_path).await,
        RemoteCommand::Select { channel } => {
            post_command("select", Some(channel), config_path).await
        }
        RemoteCommand::Prog { channel, long } => {
            let command = if long { "prog_long" } else { "prog" };
            post_command(command, Some(channel), config_path).await
        }
        RemoteCommand::Status => status().await,
        RemoteCommand::Watch => watch().await,
    }
}

fn ensure_pairing_allowed(config_path: Option<PathBuf>) -> Result<()> {
    let resolved = config::resolve(config_path)?;
    BlindService::ensure_pairing_for_kind(resolved.config.driver, Command::Prog)?;
    Ok(())
}

async fn post_command(
    command: &'static str,
    channel: Option<Channel>,
    config_path: Option<PathBuf>,
) -> Result<()> {
    if matches!(command, "prog" | "prog_long") {
        ensure_pairing_allowed(config_path)?;
        // Fast-fail wire rules (channel required) before HTTP;
        // the server runs the same validation again on POST /command.
        BlindService::parse_command(CommandRequest {
            command: command.to_string(),
            channel,
        })?;
    }

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{SERVICE_BASE_URL}/command"))
        .json(&CommandRequest {
            command: command.to_string(),
            channel,
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
