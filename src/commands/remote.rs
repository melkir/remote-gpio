use anyhow::{bail, Result};
use std::sync::Arc;
use std::time::Duration;

use crate::backend::{BackendKind, TelisOptions};
use crate::cli::RemoteCommand;
use crate::config::AppConfig;
use crate::gpio::Channel;
use crate::remote::{Command, RemoteControl};

const TELIS_PROG_PRESS: Duration = Duration::from_millis(2500);
const TELIS_RTS_PROG_DELAY: Duration = Duration::from_millis(700);

pub async fn run(command: RemoteCommand, config: &AppConfig) -> Result<()> {
    match command {
        RemoteCommand::Up { channel } => execute(config, Command::Up, channel).await,
        RemoteCommand::Down { channel } => execute(config, Command::Down, channel).await,
        RemoteCommand::Stop { channel } => execute(config, Command::Stop, channel).await,
        RemoteCommand::Select { channel } => execute(config, Command::Select, Some(channel)).await,
        RemoteCommand::Prog { channel } => prog(config, channel).await,
        RemoteCommand::Status => status(config).await,
        RemoteCommand::Watch => watch(config).await,
    }
}

async fn remote_control(config: &AppConfig) -> Result<RemoteControl> {
    RemoteControl::with_backend(config.backend_config()).await
}

async fn execute(config: &AppConfig, command: Command, channel: Option<Channel>) -> Result<()> {
    let remote = remote_control(config).await?;
    match channel {
        Some(channel) if command != Command::Select => {
            remote.execute_on(channel, command).await?;
        }
        _ => {
            remote.execute(command, channel).await?;
        }
    }
    Ok(())
}

async fn prog(config: &AppConfig, channel: Channel) -> Result<()> {
    if config.backend == BackendKind::Rts {
        if let Some(prog_gpio) = config.telis.gpio.prog {
            return assisted_rts_prog(config, channel, prog_gpio).await;
        }
    }
    let remote = remote_control(config).await?;
    remote.execute_on(channel, Command::Prog).await?;
    Ok(())
}

async fn assisted_rts_prog(config: &AppConfig, channel: Channel, prog_gpio: u8) -> Result<()> {
    if prog_gpio > 31 {
        bail!("Telis Prog GPIO {prog_gpio} is out of BCM range (0..=31)");
    }

    let telis = Arc::new(
        RemoteControl::with_backend(crate::backend::BackendConfig {
            kind: BackendKind::Telis,
            rts: config.rts.clone(),
            telis: TelisOptions {
                gpio: config.telis.gpio.clone(),
            },
        })
        .await?,
    );
    let rts = Arc::new(
        RemoteControl::with_backend(crate::backend::BackendConfig {
            kind: BackendKind::Rts,
            rts: config.rts.clone(),
            telis: config.telis.clone(),
        })
        .await?,
    );

    if telis.current_selection() != channel {
        telis.execute(Command::Select, Some(channel)).await?;
    }
    crate::gpio::trigger_output_gpio(prog_gpio, TELIS_PROG_PRESS).await?;
    tokio::time::sleep(TELIS_RTS_PROG_DELAY).await;
    rts.execute_on(channel, Command::Prog).await?;
    Ok(())
}

async fn status(config: &AppConfig) -> Result<()> {
    let remote = remote_control(config).await?;
    println!("{}", remote.current_selection());
    Ok(())
}

async fn watch(config: &AppConfig) -> Result<()> {
    let remote = remote_control(config).await?;
    let mut rx = remote.subscribe_selection();
    println!("{}", *rx.borrow_and_update());
    while rx.changed().await.is_ok() {
        println!("{}", *rx.borrow_and_update());
    }
    Ok(())
}
