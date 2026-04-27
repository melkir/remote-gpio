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
    /// Restart the systemd service
    Restart,
    /// Inspect or reset HomeKit pairing state
    Homekit {
        #[command(subcommand)]
        command: HomekitCommand,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum UpgradeChannel {
    Stable,
    Nightly,
}

#[derive(Subcommand, Debug)]
pub enum HomekitCommand {
    /// Show HomeKit identity, pairing status, and pairing QR when unpaired
    Status {
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
        /// Print only the X-HM:// setup URI
        #[arg(long)]
        uri_only: bool,
    },
    /// Regenerate the HomeKit identity and remove all pairings
    Reset,
    /// List paired HomeKit controllers
    Pairings {
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Remove one paired HomeKit controller by identifier
    Unpair {
        /// Controller identifier from `somfy homekit pairings`
        identifier: String,
    },
}
