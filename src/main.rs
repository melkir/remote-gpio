use anyhow::Result;
use clap::Parser;
use somfy::cli::{Cli, Command};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    somfy::logging::init();

    // rustls 0.23 requires an explicit crypto provider. Pin to `ring` so
    // `cargo-zigbuild` can cross-compile without pulling `aws-lc-rs`.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();
    let config_path = cli.config;
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            let resolved_config = somfy::config::resolve(config_path)?;
            somfy::commands::serve::run(resolved_config).await
        }
        Command::Install { user } => {
            let resolved_config = somfy::config::resolve(config_path)?;
            somfy::commands::install::run(user, &resolved_config)
        }
        Command::Upgrade {
            channel,
            version,
            check,
        } => somfy::commands::upgrade::run(channel, version, check).await,
        Command::Doctor { json, verbose } => {
            let resolved_config = somfy::config::resolve(config_path)?;
            somfy::commands::doctor::run(json, verbose, &resolved_config).await
        }
        Command::Uninstall => somfy::commands::uninstall::run().await,
        Command::Restart => somfy::commands::restart::run(),
        Command::Remote { command } => somfy::commands::remote::run(command).await,
        Command::Homekit { command } => somfy::commands::homekit::run(command),
        Command::Logs(args) => somfy::commands::logs::run(args),
        Command::Config { command } => match command {
            somfy::cli::ConfigCommand::Path => {
                let resolved_config = somfy::config::resolve(config_path)?;
                somfy::commands::config::path(&resolved_config);
                Ok(())
            }
            somfy::cli::ConfigCommand::Show => {
                let resolved_config = somfy::config::resolve(config_path)?;
                somfy::commands::config::show(&resolved_config)
            }
            somfy::cli::ConfigCommand::SetDriver { kind } => {
                let resolved_config = somfy::config::resolve(config_path)?;
                somfy::commands::config::set_driver(&resolved_config, kind)
            }
        },
    }
}
