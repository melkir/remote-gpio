use clap::{Parser, Subcommand, ValueEnum};

use crate::backend::BackendKind;
use crate::backend::RtsOptions;
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
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the HTTP/SSE/WebSocket server (default)
    Serve(ServeArgs),
    /// Install or refresh the systemd unit
    Install {
        /// Override the service user (required when running as root without SUDO_USER)
        #[arg(long)]
        user: Option<String>,
        /// Backend to write into the systemd unit
        #[arg(long, value_enum, default_value_t = BackendKind::Fake)]
        backend: BackendKind,
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
    /// Inspect or transmit RTS frames
    Rts {
        #[command(subcommand)]
        command: RtsCliCommand,
        #[command(flatten)]
        options: RtsArgs,
    },
}

#[derive(Clone, Debug, Parser)]
pub struct ServeArgs {
    /// Active backend implementation
    #[arg(long, env = "SOMFY_BACKEND", value_enum, default_value_t = BackendKind::Fake)]
    pub backend: BackendKind,
    #[command(flatten)]
    pub rts: RtsArgs,
}

#[derive(Clone, Debug, Parser)]
pub struct RtsArgs {
    /// RTS SPI device for CC1101
    #[arg(long, env = "SOMFY_RTS_SPI_DEVICE", default_value = "/dev/spidev0.0")]
    pub rts_spi_device: String,
    /// BCM GPIO connected to CC1101 GDO0
    #[arg(long, env = "SOMFY_RTS_GDO0_GPIO", default_value_t = 18)]
    pub rts_gdo0_gpio: u8,
    /// pigpiod socket address
    #[arg(long, env = "SOMFY_PIGPIOD_ADDR", default_value = "127.0.0.1:8888")]
    pub pigpiod_addr: String,
    /// RTS frame count per command press
    #[arg(long, env = "SOMFY_RTS_FRAME_COUNT", default_value_t = 4)]
    pub rts_frame_count: usize,
}

impl From<RtsArgs> for RtsOptions {
    fn from(args: RtsArgs) -> Self {
        Self {
            spi_device: args.rts_spi_device,
            gdo0_gpio: args.rts_gdo0_gpio,
            pigpiod_addr: args.pigpiod_addr,
            frame_count: args.rts_frame_count,
        }
    }
}

impl Default for ServeArgs {
    fn default() -> Self {
        Self {
            backend: BackendKind::Fake,
            rts: RtsArgs::default(),
        }
    }
}

impl Default for RtsArgs {
    fn default() -> Self {
        Self {
            rts_spi_device: "/dev/spidev0.0".to_string(),
            rts_gdo0_gpio: 18,
            pigpiod_addr: "127.0.0.1:8888".to_string(),
            rts_frame_count: 4,
        }
    }
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

#[derive(Subcommand, Debug)]
pub enum RtsCliCommand {
    /// Print an RTS frame and waveform summary without transmitting
    Dump {
        channel: Channel,
        command: RtsCommandArg,
        #[arg(long, value_enum, default_value_t = DumpFormat::Json)]
        format: DumpFormat,
    },
    /// Transmit an RTS command
    Send {
        channel: Channel,
        command: RtsCommandArg,
    },
    /// Transmit the RTS programming command for a channel
    Prog {
        channel: Channel,
        /// Also press the wired Telis Prog button first
        #[arg(long)]
        with_telis: bool,
        /// BCM GPIO wired to the Telis Prog button
        #[arg(long, default_value_t = 5, requires = "with_telis")]
        telis_gpio: u8,
        /// How long to hold the wired Telis Prog button
        #[arg(long, default_value_t = 2500, requires = "with_telis")]
        telis_press_ms: u64,
        /// Delay between releasing Telis Prog and transmitting RTS Prog
        #[arg(long, default_value_t = 700, requires = "with_telis")]
        telis_delay_ms: u64,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum RtsCommandArg {
    Up,
    Down,
    My,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum DumpFormat {
    Json,
}
