use anyhow::Result;
use clap::Parser;
use somfy::cli::{Cli, Command, ServeArgs};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    somfy::logging::init();

    // rustls 0.23 requires an explicit crypto provider. Pin to `ring` so
    // `cargo-zigbuild` can cross-compile without pulling `aws-lc-rs`.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve(ServeArgs::default())) {
        Command::Serve(args) => somfy::commands::serve::run(args).await,
        Command::Install { user, backend } => somfy::commands::install::run(user, backend),
        Command::Upgrade {
            channel,
            version,
            check,
        } => somfy::commands::upgrade::run(channel, version, check).await,
        Command::Doctor { json, verbose } => somfy::commands::doctor::run(json, verbose).await,
        Command::Uninstall => somfy::commands::uninstall::run().await,
        Command::Restart => somfy::commands::restart::run(),
        Command::Homekit { command } => somfy::commands::homekit::run(command),
        Command::Rts { command, options } => somfy::commands::rts::run(command, options).await,
    }
}
