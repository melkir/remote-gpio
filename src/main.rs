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
    let resolved = config::resolve(cli.config)?;
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => commands::serve::run(&resolved).await,
        Command::Install { user } => commands::install::run(user, &resolved),
        Command::Upgrade {
            channel,
            version,
            check,
        } => commands::upgrade::run(channel, version, check).await,
        Command::Doctor { json, verbose } => commands::doctor::run(json, verbose, &resolved).await,
        Command::Uninstall => commands::uninstall::run().await,
        Command::Restart => commands::restart::run(),
        Command::Remote { command } => commands::remote::run(command, &resolved).await,
        Command::Homekit { command } => commands::homekit::run(command, &resolved),
        Command::Logs(args) => commands::logs::run(args),
        Command::Config { command } => match command {
            ConfigCommand::Path => {
                commands::config::path(&resolved);
                Ok(())
            }
            ConfigCommand::Show => commands::config::show(&resolved),
            ConfigCommand::SetDriver { kind } => commands::config::set_driver(&resolved, kind),
            ConfigCommand::SetPositioning {
                channel,
                open,
                close,
            } => commands::config::set_positioning(&resolved, channel, open, close),
        },
    }
}
