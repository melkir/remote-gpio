use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "somfy",
    about = "Somfy Telis 4 remote controller",
    version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (sha ", env!("VERGEN_GIT_SHA"),
        ", built ", env!("VERGEN_BUILD_DATE"), ")"
    ),
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the HTTP/WebSocket server (default)
    Serve,
    /// Install or refresh the systemd unit
    Install {
        /// Override the service user (required when running as root without SUDO_USER)
        #[arg(long)]
        user: Option<String>,
    },
    /// Upgrade to a newer release
    Upgrade {
        #[arg(long, value_enum, default_value_t = UpgradeChannel::Stable)]
        channel: UpgradeChannel,
        /// Pin to a specific tag, e.g. v0.2.0
        #[arg(long)]
        version: Option<String>,
        /// Report if a newer release exists without applying it
        #[arg(long)]
        check: bool,
    },
    /// Run health checks
    Doctor {
        #[arg(long)]
        json: bool,
        #[arg(short, long)]
        verbose: bool,
    },
    /// Disable and remove the systemd unit
    Uninstall,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum UpgradeChannel {
    Stable,
    Main,
}
