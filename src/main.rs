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
    let resolved_config = somfy::config::resolve(cli.config)?;
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => somfy::commands::serve::run(resolved_config).await,
        Command::Install { user } => somfy::commands::install::run(user, &resolved_config),
        Command::Upgrade {
            channel,
            version,
            check,
        } => somfy::commands::upgrade::run(channel, version, check).await,
        Command::Doctor { json, verbose } => {
            somfy::commands::doctor::run(json, verbose, &resolved_config).await
        }
        Command::Uninstall => somfy::commands::uninstall::run().await,
        Command::Restart => somfy::commands::restart::run(),
        Command::Remote { command } => {
            somfy::commands::remote::run(command, &resolved_config.config).await
        }
        Command::Homekit { command } => somfy::commands::homekit::run(command),
        Command::Logs(args) => somfy::commands::logs::run(args),
        Command::Config { command } => match command {
            somfy::cli::ConfigCommand::Path => {
                somfy::commands::config::path(&resolved_config);
                Ok(())
            }
            somfy::cli::ConfigCommand::Show => somfy::commands::config::show(&resolved_config),
            somfy::cli::ConfigCommand::Validate => {
                somfy::commands::config::validate(&resolved_config)
            }
        },
    }
}
