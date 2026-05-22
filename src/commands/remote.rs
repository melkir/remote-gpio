use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use std::path::PathBuf;

use crate::cli::RemoteCommand;
use crate::config::{self, ResolvedConfig};
use crate::core::Channel;
use crate::server::base_url;
use crate::service::{validate_command_request, CommandRequest};

pub async fn run(command: RemoteCommand, config_path: Option<PathBuf>) -> Result<()> {
    let resolved = config::resolve(config_path)?;

    match command {
        RemoteCommand::Up { channel } => post_command("up", channel, &resolved).await,
        RemoteCommand::Down { channel } => post_command("down", channel, &resolved).await,
        RemoteCommand::Stop { channel } => post_command("stop", channel, &resolved).await,
        RemoteCommand::Select { channel } => post_command("select", Some(channel), &resolved).await,
        RemoteCommand::Prog { channel, long } => {
            let cmd = if long { "prog_long" } else { "prog" };
            post_command(cmd, Some(channel), &resolved).await
        }
        RemoteCommand::Status => status().await,
        RemoteCommand::Watch => watch().await,
    }
}

async fn post_command(
    command: &'static str,
    channel: Option<Channel>,
    resolved: &ResolvedConfig,
) -> Result<()> {
    validate_command_request(
        resolved.config.driver,
        CommandRequest {
            command: command.to_string(),
            channel,
        },
    )?;

    let client = reqwest::Client::new();
    let url = format!("{}/command", base_url());
    let response = client
        .post(&url)
        .json(&CommandRequest {
            command: command.to_string(),
            channel,
        })
        .send()
        .await
        .with_context(|| format!("connecting to somfy service at {url}"))?;

    if response.status().is_success() {
        return Ok(());
    }

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    bail!("service rejected {command}: HTTP {status}: {}", body.trim());
}

async fn status() -> Result<()> {
    let url = format!("{}/channel", base_url());
    let text = reqwest::get(&url)
        .await
        .with_context(|| format!("connecting to somfy service at {url}"))?
        .error_for_status()
        .context("reading selected channel from somfy service")?
        .text()
        .await?;
    println!("{}", text.trim());
    Ok(())
}

async fn watch() -> Result<()> {
    let url = format!("{}/events", base_url());
    let response = reqwest::get(&url)
        .await
        .with_context(|| format!("connecting to somfy service at {url}"))?
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
