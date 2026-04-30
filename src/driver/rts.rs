use anyhow::{bail, Context, Result};
use std::net::TcpStream;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::watch::{self, Sender};
use tokio::sync::Mutex;

use crate::driver::{RtsOptions, SelectedChannelRx};
use crate::gpio::Channel;
use crate::remote::Command;
use crate::rts::cc1101::Cc1101;
use crate::rts::frame::{RtsCommand, RtsFrame};
use crate::rts::pigpio::PigpioClient;
use crate::rts::state::RtsStateStore;
use crate::rts::waveform;

const MAX_BCM_GPIO: u8 = 31;

#[derive(Debug)]
struct Hardware {
    radio: Cc1101<Spi>,
    pigpio: PigpioClient<TcpStream>,
}

#[cfg(target_os = "linux")]
type Spi = spidev::Spidev;

#[cfg(not(target_os = "linux"))]
type Spi = std::fs::File;

#[derive(Debug)]
pub(crate) struct RtsDriver {
    sender: Sender<Channel>,
    selected_rx: SelectedChannelRx,
    options: RtsOptions,
    state: Mutex<RtsStateStore>,
    transmitter_lock: Mutex<()>,
    transmitter: Arc<dyn RtsTransmitter>,
}

impl RtsDriver {
    pub(crate) async fn new(options: RtsOptions) -> Result<Self> {
        if options.gdo0_gpio > MAX_BCM_GPIO {
            bail!(
                "RTS GDO0 GPIO {} is out of BCM range (0..={MAX_BCM_GPIO})",
                options.gdo0_gpio
            );
        }
        let state = RtsStateStore::load_or_init_default()?;
        let selected_channel = state.selected_channel();
        let (sender, selected_rx) = watch::channel(selected_channel);
        let transmitter = init_transmitter(options.clone()).await?;
        Ok(Self::from_parts(
            sender,
            selected_rx,
            options,
            state,
            transmitter,
        ))
    }

    fn from_parts(
        sender: Sender<Channel>,
        selected_rx: SelectedChannelRx,
        options: RtsOptions,
        state: RtsStateStore,
        transmitter: Arc<dyn RtsTransmitter>,
    ) -> Self {
        Self {
            sender,
            selected_rx,
            options,
            state: Mutex::new(state),
            transmitter_lock: Mutex::new(()),
            transmitter,
        }
    }

    #[cfg(test)]
    pub(super) async fn new_for_test(
        options: RtsOptions,
        state_path: impl Into<std::path::PathBuf>,
        transmitter: Arc<dyn RtsTransmitter>,
    ) -> Result<Self> {
        let state =
            RtsStateStore::load_or_init(state_path, crate::rts::state::DEFAULT_RESERVE_SIZE)?;
        let selected_channel = state.selected_channel();
        let (sender, selected_rx) = watch::channel(selected_channel);
        Ok(Self::from_parts(
            sender,
            selected_rx,
            options,
            state,
            transmitter,
        ))
    }

    pub(crate) async fn execute(&self, command: Command, channel: Option<Channel>) -> Result<()> {
        match command {
            Command::Select => {
                let channel = channel.unwrap_or_else(|| next_channel(self.selected_channel()));
                self.set_selected_channel(channel).await
            }
            Command::Up | Command::Down | Command::Stop | Command::Prog => {
                let channel = self.selected_channel();
                self.execute_on(channel, command).await
            }
        }
    }

    pub(crate) async fn execute_on(&self, channel: Channel, command: Command) -> Result<()> {
        let rts_command = RtsCommand::try_from(command)?;
        self.transmit(channel, rts_command).await
    }

    pub(crate) fn selected_channel(&self) -> Channel {
        *self.selected_rx.borrow()
    }

    pub(crate) fn subscribe_selected_channel(&self) -> SelectedChannelRx {
        self.selected_rx.clone()
    }

    async fn set_selected_channel(&self, channel: Channel) -> Result<()> {
        {
            let mut state = self.state.lock().await;
            state.set_selected_channel(channel)?;
        }
        self.sender.send(channel)?;
        Ok(())
    }

    async fn transmit(&self, channel: Channel, command: RtsCommand) -> Result<()> {
        let _guard = self.transmitter_lock.lock().await;

        let (rolling_code, remote_id) = {
            let mut state = self.state.lock().await;
            let rolling_code = state.reserve_rolling_code(channel)?;
            let remote_id = state.channel(channel).remote_id;
            (rolling_code, remote_id)
        };

        let frame = RtsFrame::encode(command, rolling_code, remote_id)?;
        let pulses = waveform::build(frame, self.options.gdo0_gpio, self.options.frame_count);
        let pulse_count = pulses.len();
        let total_duration_us: u64 = pulses.iter().map(|pulse| pulse.us_delay as u64).sum();
        tracing::debug!(
            %channel,
            command = ?command,
            rolling_code,
            remote_id,
            frame = %hex::encode(frame.bytes()),
            gpio = self.options.gdo0_gpio,
            frame_count = self.options.frame_count,
            pulse_count,
            total_duration_us,
            "rts waveform prepared"
        );
        let transmission = PreparedTransmission {
            #[cfg(test)]
            channel,
            #[cfg(test)]
            command,
            #[cfg(test)]
            rolling_code,
            #[cfg(test)]
            remote_id,
            pulses,
        };
        let transmitter = self.transmitter.clone();

        tokio::task::spawn_blocking(move || transmitter.transmit(transmission))
            .await
            .context("RTS transmitter task failed")??;

        let mut state = self.state.lock().await;
        state.commit_rolling_code(channel, rolling_code)?;
        tracing::info!(
            %channel,
            command = ?command,
            rolling_code,
            remote_id,
            "rts command transmitted"
        );
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(super) struct PreparedTransmission {
    #[cfg(test)]
    pub(super) channel: Channel,
    #[cfg(test)]
    pub(super) command: RtsCommand,
    #[cfg(test)]
    pub(super) rolling_code: u16,
    #[cfg(test)]
    pub(super) remote_id: u32,
    pub(super) pulses: Vec<waveform::GpioPulse>,
}

pub(super) trait RtsTransmitter: std::fmt::Debug + Send + Sync + 'static {
    fn transmit(&self, transmission: PreparedTransmission) -> Result<()>;
}

#[derive(Debug)]
struct PigpioTransmitter {
    hardware: Arc<StdMutex<Hardware>>,
}

impl RtsTransmitter for PigpioTransmitter {
    fn transmit(&self, transmission: PreparedTransmission) -> Result<()> {
        transmit_blocking(self.hardware.clone(), transmission.pulses)
    }
}

async fn init_transmitter(options: RtsOptions) -> Result<Arc<dyn RtsTransmitter>> {
    tokio::task::spawn_blocking(move || -> Result<Arc<dyn RtsTransmitter>> {
        let spi = open_spi(&options.spi_device)?;
        let mut radio = Cc1101::new(spi);
        radio
            .configure_ook_433_42()
            .context("configuring CC1101 for 433.42 MHz async OOK")?;
        tracing::warn!(
            "CC1101 register set is unvalidated; verify timing with a scope or SDR before pairing"
        );
        let mut pigpio = PigpioClient::connect(&options.pigpiod_addr)
            .with_context(|| format!("connecting to pigpiod at {}", options.pigpiod_addr))?;
        pigpio.set_output(options.gdo0_gpio)?;
        pigpio.write_level(options.gdo0_gpio, false)?;
        pigpio.wave_clear()?;
        Ok(Arc::new(PigpioTransmitter {
            hardware: Arc::new(StdMutex::new(Hardware { radio, pigpio })),
        }))
    })
    .await
    .context("RTS transmitter init task failed")?
}

#[cfg(target_os = "linux")]
fn open_spi(path: &str) -> Result<Spi> {
    use spidev::{SpiModeFlags, SpidevOptions};

    let mut spi =
        spidev::Spidev::open(path).with_context(|| format!("opening RTS SPI device {path}"))?;
    let options = SpidevOptions::new()
        .bits_per_word(8)
        .max_speed_hz(4_000_000)
        .mode(SpiModeFlags::SPI_MODE_0)
        .build();
    spi.configure(&options)
        .with_context(|| format!("configuring RTS SPI device {path}"))?;
    Ok(spi)
}

#[cfg(not(target_os = "linux"))]
fn open_spi(path: &str) -> Result<Spi> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening local RTS SPI test sink {path}"))
}

fn transmit_blocking(
    hardware: Arc<StdMutex<Hardware>>,
    pulses: Vec<waveform::GpioPulse>,
) -> Result<()> {
    let mut hw = hardware.lock().expect("RTS hardware mutex poisoned");
    hw.pigpio.wave_new()?;
    hw.pigpio.wave_add_generic(&pulses)?;
    let wave_id = hw.pigpio.wave_create()?;
    tracing::debug!(wave_id, "pigpio wave created");

    hw.radio.tx()?;
    let tx_result = (|| -> Result<()> {
        hw.pigpio.wave_tx(wave_id)?;
        tracing::debug!(wave_id, "pigpio wave transmit started");
        while hw.pigpio.wave_busy()? {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        tracing::debug!(wave_id, "pigpio wave transmit completed");
        Ok(())
    })();
    let delete_result = hw.pigpio.wave_delete(wave_id);
    let idle_result = hw.radio.idle();

    tx_result?;
    delete_result?;
    idle_result?;
    Ok(())
}

fn next_channel(channel: Channel) -> Channel {
    match channel {
        Channel::L1 => Channel::L2,
        Channel::L2 => Channel::L3,
        Channel::L3 => Channel::L4,
        Channel::L4 => Channel::ALL,
        Channel::ALL => Channel::L1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[derive(Debug, Default)]
    struct RecordingTransmitter {
        transmissions: StdMutex<Vec<PreparedTransmission>>,
    }

    impl RecordingTransmitter {
        fn transmissions(&self) -> Vec<PreparedTransmission> {
            self.transmissions
                .lock()
                .expect("recording transmitter mutex")
                .clone()
        }
    }

    impl RtsTransmitter for RecordingTransmitter {
        fn transmit(&self, transmission: PreparedTransmission) -> Result<()> {
            self.transmissions
                .lock()
                .expect("recording transmitter mutex")
                .push(transmission);
            Ok(())
        }
    }

    #[tokio::test]
    async fn execute_on_transmits_waveform_and_reserves_rolling_code() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join(crate::rts::state::STATE_FILE);
        let transmitter = Arc::new(RecordingTransmitter::default());
        let driver = RtsDriver::new_for_test(
            RtsOptions {
                gdo0_gpio: 18,
                frame_count: waveform::DEFAULT_FRAME_COUNT,
                ..RtsOptions::default()
            },
            &state_path,
            transmitter.clone(),
        )
        .await
        .unwrap();

        driver.execute_on(Channel::L3, Command::Up).await.unwrap();

        let transmissions = transmitter.transmissions();
        assert_eq!(transmissions.len(), 1);
        assert_eq!(transmissions[0].channel, Channel::L3);
        assert_eq!(transmissions[0].command, RtsCommand::Up);
        assert_eq!(transmissions[0].rolling_code, 1);
        assert!(transmissions[0].remote_id > 0);
        assert_eq!(transmissions[0].pulses.len(), 508);

        let state: crate::rts::state::RtsState =
            serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(state.selected_channel, Channel::L1);
        assert_eq!(
            state.channels.get(&Channel::L3).unwrap().reserved_until,
            1 + crate::rts::state::DEFAULT_RESERVE_SIZE
        );
    }

    #[tokio::test]
    async fn select_updates_persisted_rts_selection_without_transmitting() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join(crate::rts::state::STATE_FILE);
        let transmitter = Arc::new(RecordingTransmitter::default());
        let driver =
            RtsDriver::new_for_test(RtsOptions::default(), &state_path, transmitter.clone())
                .await
                .unwrap();

        driver
            .execute(Command::Select, Some(Channel::L4))
            .await
            .unwrap();

        assert_eq!(driver.selected_channel(), Channel::L4);
        assert!(transmitter.transmissions().is_empty());
        let state: crate::rts::state::RtsState =
            serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(state.selected_channel, Channel::L4);
    }
}
