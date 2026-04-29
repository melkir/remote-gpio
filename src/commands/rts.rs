use anyhow::{bail, Result};
use serde::Serialize;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

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
        RtsCliCommand::Prog {
            channel,
            with_telis: true,
            telis_gpio,
            telis_press_ms,
            telis_delay_ms,
        } => {
            prog_with_telis(
                channel,
                telis_gpio,
                Duration::from_millis(telis_press_ms),
                Duration::from_millis(telis_delay_ms),
                options,
            )
            .await
        }
        RtsCliCommand::Prog {
            channel,
            with_telis: false,
            ..
        } => execute_on(channel, Command::Prog, options).await,
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

async fn prog_with_telis(
    channel: Channel,
    telis_gpio: u8,
    telis_press: Duration,
    telis_delay: Duration,
    options: RtsArgs,
) -> Result<()> {
    if telis_gpio > 31 {
        bail!("Telis Prog GPIO {telis_gpio} is out of BCM range (0..=31)");
    }

    let rts_options = options.into();
    let telis = Arc::new(
        RemoteControl::with_backend(BackendConfig {
            kind: BackendKind::Telis,
            rts: RtsOptions::default(),
        })
        .await?,
    );
    let rts = Arc::new(
        RemoteControl::with_backend(BackendConfig {
            kind: BackendKind::Rts,
            rts: rts_options,
        })
        .await?,
    );
    let telis = GpioTelisProgRemote {
        remote_control: telis,
        prog_gpio: telis_gpio,
        prog_press: telis_press,
    };
    let rts = RtsProgRemote {
        remote_control: rts,
    };

    run_prog_sequence(&telis, &rts, channel, telis_delay).await
}

type ProgFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

trait TelisProgRemote {
    fn selected_channel(&self) -> Channel;
    fn select_to<'a>(&'a self, channel: Channel) -> ProgFuture<'a>;
    fn press_prog<'a>(&'a self) -> ProgFuture<'a>;
}

trait RtsProgTransmitter {
    fn transmit_prog<'a>(&'a self, channel: Channel) -> ProgFuture<'a>;
}

async fn run_prog_sequence(
    telis: &impl TelisProgRemote,
    rts: &impl RtsProgTransmitter,
    channel: Channel,
    telis_delay: Duration,
) -> Result<()> {
    if telis.selected_channel() != channel {
        telis.select_to(channel).await?;
    }
    telis.press_prog().await?;
    tokio::time::sleep(telis_delay).await;
    rts.transmit_prog(channel).await
}

struct GpioTelisProgRemote {
    remote_control: Arc<RemoteControl>,
    prog_gpio: u8,
    prog_press: Duration,
}

impl TelisProgRemote for GpioTelisProgRemote {
    fn selected_channel(&self) -> Channel {
        self.remote_control.current_selection()
    }

    fn select_to<'a>(&'a self, channel: Channel) -> ProgFuture<'a> {
        Box::pin(async move {
            self.remote_control
                .execute(Command::Select, Some(channel))
                .await?;
            Ok(())
        })
    }

    fn press_prog<'a>(&'a self) -> ProgFuture<'a> {
        Box::pin(
            async move { crate::gpio::trigger_output_gpio(self.prog_gpio, self.prog_press).await },
        )
    }
}

struct RtsProgRemote {
    remote_control: Arc<RemoteControl>,
}

impl RtsProgTransmitter for RtsProgRemote {
    fn transmit_prog<'a>(&'a self, channel: Channel) -> ProgFuture<'a> {
        Box::pin(async move {
            self.remote_control
                .execute_on(channel, Command::Prog)
                .await?;
            Ok(())
        })
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum ProgOperation {
        Select(Channel),
        TelisProg,
        RtsProg(Channel),
    }

    #[derive(Clone, Debug)]
    struct FakeTelisProgRemote {
        selected: Arc<Mutex<Channel>>,
        operations: Arc<Mutex<Vec<ProgOperation>>>,
    }

    impl FakeTelisProgRemote {
        fn new(selected: Channel, operations: Arc<Mutex<Vec<ProgOperation>>>) -> Self {
            Self {
                selected: Arc::new(Mutex::new(selected)),
                operations,
            }
        }
    }

    impl TelisProgRemote for FakeTelisProgRemote {
        fn selected_channel(&self) -> Channel {
            *self.selected.lock().expect("selected mutex")
        }

        fn select_to<'a>(&'a self, channel: Channel) -> ProgFuture<'a> {
            Box::pin(async move {
                *self.selected.lock().expect("selected mutex") = channel;
                self.operations
                    .lock()
                    .expect("operations mutex")
                    .push(ProgOperation::Select(channel));
                Ok(())
            })
        }

        fn press_prog<'a>(&'a self) -> ProgFuture<'a> {
            Box::pin(async move {
                self.operations
                    .lock()
                    .expect("operations mutex")
                    .push(ProgOperation::TelisProg);
                Ok(())
            })
        }
    }

    #[derive(Clone, Debug)]
    struct FakeRtsProgRemote {
        operations: Arc<Mutex<Vec<ProgOperation>>>,
    }

    impl RtsProgTransmitter for FakeRtsProgRemote {
        fn transmit_prog<'a>(&'a self, channel: Channel) -> ProgFuture<'a> {
            Box::pin(async move {
                self.operations
                    .lock()
                    .expect("operations mutex")
                    .push(ProgOperation::RtsProg(channel));
                Ok(())
            })
        }
    }

    #[cfg(feature = "fake")]
    struct RemoteControlFakeTelisProgRemote {
        remote_control: Arc<RemoteControl>,
        operations: Arc<Mutex<Vec<ProgOperation>>>,
    }

    #[cfg(feature = "fake")]
    impl RemoteControlFakeTelisProgRemote {
        async fn new(operations: Arc<Mutex<Vec<ProgOperation>>>) -> Self {
            Self {
                remote_control: Arc::new(
                    RemoteControl::with_backend(crate::backend::BackendConfig {
                        kind: crate::backend::BackendKind::Fake,
                        rts: crate::backend::RtsOptions::default(),
                    })
                    .await
                    .unwrap(),
                ),
                operations,
            }
        }
    }

    #[cfg(feature = "fake")]
    impl TelisProgRemote for RemoteControlFakeTelisProgRemote {
        fn selected_channel(&self) -> Channel {
            self.remote_control.current_selection()
        }

        fn select_to<'a>(&'a self, channel: Channel) -> ProgFuture<'a> {
            Box::pin(async move {
                self.remote_control
                    .execute(Command::Select, Some(channel))
                    .await?;
                Ok(())
            })
        }

        fn press_prog<'a>(&'a self) -> ProgFuture<'a> {
            Box::pin(async move {
                self.operations
                    .lock()
                    .expect("operations mutex")
                    .push(ProgOperation::TelisProg);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn prog_with_telis_selects_channel_before_telis_and_rts_prog() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let telis = FakeTelisProgRemote::new(Channel::L1, operations.clone());
        let rts = FakeRtsProgRemote {
            operations: operations.clone(),
        };

        run_prog_sequence(&telis, &rts, Channel::L3, Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(
            *operations.lock().expect("operations mutex"),
            vec![
                ProgOperation::Select(Channel::L3),
                ProgOperation::TelisProg,
                ProgOperation::RtsProg(Channel::L3),
            ]
        );
    }

    #[tokio::test]
    async fn prog_with_telis_skips_select_when_channel_is_already_selected() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let telis = FakeTelisProgRemote::new(Channel::L3, operations.clone());
        let rts = FakeRtsProgRemote {
            operations: operations.clone(),
        };

        run_prog_sequence(&telis, &rts, Channel::L3, Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(
            *operations.lock().expect("operations mutex"),
            vec![
                ProgOperation::TelisProg,
                ProgOperation::RtsProg(Channel::L3)
            ]
        );
    }

    #[cfg(feature = "fake")]
    #[tokio::test]
    async fn prog_with_telis_uses_remote_control_selection_before_rts_prog() {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let telis = RemoteControlFakeTelisProgRemote::new(operations.clone()).await;
        let rts = FakeRtsProgRemote {
            operations: operations.clone(),
        };

        run_prog_sequence(&telis, &rts, Channel::L3, Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(telis.remote_control.current_selection(), Channel::L3);
        assert_eq!(
            telis.remote_control.operations(),
            vec![crate::backend::ProtocolOperation::TelisSelection(
                Channel::L3
            )]
        );
        assert_eq!(
            *operations.lock().expect("operations mutex"),
            vec![
                ProgOperation::TelisProg,
                ProgOperation::RtsProg(Channel::L3)
            ]
        );
    }
}
