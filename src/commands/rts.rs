use anyhow::Result;
use serde::Serialize;

use crate::backend::{BackendConfig, BackendKind, RtsOptions};
use crate::cli::{DumpFormat, RtsArgs, RtsCliCommand, RtsCommandArg};
use crate::gpio::Channel;
use crate::remote::{Command, RemoteControl};
use crate::rts::frame::{RtsCommand, RtsFrame};
use crate::rts::state::RtsStateStore;
use crate::rts::waveform;

pub async fn run(command: RtsCliCommand, options: RtsArgs) -> Result<()> {
    match command {
        RtsCliCommand::Dump {
            channel,
            command,
            format,
        } => dump(channel, command, format, options),
        RtsCliCommand::Send { channel, command } => {
            execute_on(channel, command.into(), options).await
        }
        RtsCliCommand::Prog { channel } => execute_on(channel, Command::Prog, options).await,
    }
}

fn dump(
    channel: Channel,
    command: RtsCommandArg,
    format: DumpFormat,
    options: RtsArgs,
) -> Result<()> {
    let rts_options: RtsOptions = options.into();
    let state = RtsStateStore::load_or_init_default()?;
    let channel_state = state.channel(channel);
    let rolling_code = state.next_on_wire(channel);
    let command: RtsCommand = Command::from(command).try_into()?;
    let frame = RtsFrame::encode(command, rolling_code, channel_state.remote_id)?;
    let pulses = waveform::build(frame, rts_options.gdo0_gpio, rts_options.frame_count);
    let response = DumpResponse {
        channel,
        command: command.code(),
        rolling_code,
        remote_id: channel_state.remote_id,
        frame: frame.bytes(),
        gpio: rts_options.gdo0_gpio,
        frame_count: rts_options.frame_count,
        pulse_count: pulses.len(),
        total_duration_us: pulses.iter().map(|pulse| pulse.us_delay as u64).sum(),
    };

    match format {
        DumpFormat::Json => println!("{}", serde_json::to_string_pretty(&response)?),
    }
    Ok(())
}

async fn execute_on(channel: Channel, command: Command, options: RtsArgs) -> Result<()> {
    let remote_control = RemoteControl::with_backend(BackendConfig {
        kind: BackendKind::Rts,
        rts: options.into(),
    })
    .await?;
    remote_control.execute_on(channel, command).await?;
    Ok(())
}

impl From<RtsCommandArg> for Command {
    fn from(command: RtsCommandArg) -> Self {
        match command {
            RtsCommandArg::Up => Self::Up,
            RtsCommandArg::Down => Self::Down,
            RtsCommandArg::My => Self::My,
        }
    }
}

#[derive(Serialize)]
struct DumpResponse {
    channel: Channel,
    command: u8,
    rolling_code: u16,
    remote_id: u32,
    frame: [u8; crate::rts::frame::FRAME_LEN],
    gpio: u8,
    frame_count: usize,
    pulse_count: usize,
    total_duration_us: u64,
}
