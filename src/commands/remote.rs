use anyhow::{bail, Context, Result};
use futures_util::StreamExt;

use crate::cli::RemoteCommand;
use crate::config::ResolvedConfig;
use crate::core::Command;
use crate::server::base_url;
use crate::service::{validate_control_request, CommandRequest, ControlRequest};

pub async fn run(command: RemoteCommand, resolved: &ResolvedConfig) -> Result<()> {
    let request = match command {
        RemoteCommand::Up { channel } => ControlRequest::Driver {
            command: Command::Up,
            channel,
        },
        RemoteCommand::Down { channel } => ControlRequest::Driver {
            command: Command::Down,
            channel,
        },
        RemoteCommand::Stop { channel } => ControlRequest::Driver {
            command: Command::Stop,
            channel,
        },
        RemoteCommand::Select { channel } => ControlRequest::Driver {
            command: Command::Select,
            channel: Some(channel),
        },
        RemoteCommand::Prog { channel, long } => ControlRequest::Driver {
            command: if long {
                Command::ProgLong
            } else {
                Command::Prog
            },
            channel: Some(channel),
        },
        RemoteCommand::Target { position, channel } => {
            ControlRequest::Position { channel, position }
        }
        RemoteCommand::Status => return status().await,
        RemoteCommand::Watch => return watch().await,
    };
    post_control(request, resolved).await
}

async fn post_control(request: ControlRequest, resolved: &ResolvedConfig) -> Result<()> {
    let request = validate_control_request(resolved.config.driver, request)?;
    let payload = CommandRequest::from_control(request);

    let client = reqwest::Client::new();
    let url = format!("{}/command", base_url());
    let response = client
        .post(&url)
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("connecting to somfy service at {url}"))?;

    if response.status().is_success() {
        return Ok(());
    }

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    bail!(
        "service rejected {}: HTTP {status}: {}",
        payload.command,
        body.trim()
    );
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
