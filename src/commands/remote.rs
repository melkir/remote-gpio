use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::Serialize;

use crate::cli::RemoteCommand;
use crate::gpio::Channel;

const SERVICE_BASE_URL: &str = "http://127.0.0.1:5002";

pub async fn run(command: RemoteCommand) -> Result<()> {
    match command {
        RemoteCommand::Up { channel } => post_command("up", channel).await,
        RemoteCommand::Down { channel } => post_command("down", channel).await,
        RemoteCommand::Stop { channel } => post_command("stop", channel).await,
        RemoteCommand::Select { channel } => post_command("select", Some(channel)).await,
        RemoteCommand::Prog { channel } => post_command("prog", Some(channel)).await,
        RemoteCommand::Status => status().await,
        RemoteCommand::Watch => watch().await,
    }
}

#[derive(Serialize)]
struct CommandRequest {
    command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<Channel>,
}

async fn post_command(command: &'static str, channel: Option<Channel>) -> Result<()> {
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{SERVICE_BASE_URL}/command"))
        .json(&CommandRequest { command, channel })
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
