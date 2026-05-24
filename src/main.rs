//! Somfy CLI: drives blinds on a Raspberry Pi via GPIO or RTS radio.
//!
//! Uses a single-threaded Tokio runtime so HTTP, WebSocket, and HomeKit work
//! share one task queue on resource-constrained hardware.

use anyhow::Result;
use clap::Parser;
use somfy::cli::{Cli, Command, ConfigCommand};
use somfy::commands;
use somfy::config;
use somfy::logging;

/// Single-threaded runtime: blind commands are serialized through the driver layer.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    logging::init();

    // rustls 0.23 requires an explicit crypto provider. Pin to `ring` so
    // `cargo-zigbuild` can cross-compile without pulling `aws-lc-rs`.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();
    let config_path = cli.config;
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            let resolved_config = config::resolve(config_path)?;
            commands::serve::run(resolved_config).await
        }
        Command::Install { user } => {
            let resolved_config = config::resolve(config_path)?;
            commands::install::run(user, &resolved_config)
        }
        Command::Upgrade {
            channel,
            version,
            check,
        } => commands::upgrade::run(channel, version, check).await,
        Command::Doctor { json, verbose } => {
            let resolved_config = config::resolve(config_path)?;
            commands::doctor::run(json, verbose, &resolved_config).await
        }
        Command::Uninstall => commands::uninstall::run().await,
        Command::Restart => commands::restart::run(),
        Command::Remote { command } => commands::remote::run(command, config_path).await,
        Command::Homekit { command } => {
            let resolved_config = config::resolve(config_path)?;
            commands::homekit::run(command, &resolved_config)
        }
        Command::Logs(args) => commands::logs::run(args),
        Command::Config { command } => match command {
            ConfigCommand::Path => {
                let resolved_config = config::resolve(config_path)?;
                commands::config::path(&resolved_config);
                Ok(())
            }
            ConfigCommand::Show => {
                let resolved_config = config::resolve(config_path)?;
                commands::config::show(&resolved_config)
            }
            ConfigCommand::SetDriver { kind } => {
                let resolved_config = config::resolve(config_path)?;
                commands::config::set_driver(&resolved_config, kind)
            }
        },
    }
}
