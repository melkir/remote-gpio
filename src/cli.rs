use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use crate::driver::DriverKind;
use crate::gpio::Channel;

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
    /// Configuration file to read
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the HTTP/SSE/WebSocket server (default)
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
    /// Operate the configured remote driver
    Remote {
        #[command(subcommand)]
        command: RemoteCommand,
    },
    /// Inspect or reset HomeKit pairing state
    Homekit {
        #[command(subcommand)]
        command: HomekitCommand,
    },
    /// Read service logs
    Logs(LogsArgs),
    /// Inspect configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum UpgradeChannel {
    Stable,
    Nightly,
}

#[derive(Subcommand, Debug)]
pub enum RemoteCommand {
    /// Raise the selected or provided channel
    Up { channel: Option<Channel> },
    /// Lower the selected or provided channel
    Down { channel: Option<Channel> },
    /// Send the middle-button stop/favorite command
    Stop { channel: Option<Channel> },
    /// Select a channel
    Select { channel: Channel },
    /// Send the programming command for a channel
    Prog { channel: Channel },
    /// Print current selected channel
    Status,
    /// Watch selected channel changes
    Watch,
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

#[derive(Clone, Debug, Parser)]
pub struct LogsArgs {
    /// Follow logs
    #[arg(short, long)]
    pub follow: bool,
    /// Include debug-level service logs while following
    #[arg(long)]
    pub debug: bool,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Print the resolved config file path
    Path,
    /// Print the resolved configuration
    Show,
    /// Switch the active driver, restart the service, and run any new-driver prereqs
    SetDriver {
        #[arg(value_enum)]
        kind: DriverKind,
    },
}
