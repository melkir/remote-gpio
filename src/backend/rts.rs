use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::net::TcpStream;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::watch::{self, Sender};
use tokio::sync::Mutex;

use crate::backend::{RtsOptions, SelectedChannelRx};
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
    radio: Cc1101<File>,
    pigpio: PigpioClient<TcpStream>,
}

#[derive(Debug)]
pub(crate) struct RtsBackend {
    sender: Sender<Channel>,
    selected_rx: SelectedChannelRx,
    options: RtsOptions,
    state: Mutex<RtsStateStore>,
    transmitter_lock: Mutex<()>,
    hardware: Arc<StdMutex<Hardware>>,
}

impl RtsBackend {
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
        let hardware = init_hardware(options.clone()).await?;
        Ok(Self {
            sender,
            selected_rx,
            options,
            state: Mutex::new(state),
            transmitter_lock: Mutex::new(()),
            hardware: Arc::new(StdMutex::new(hardware)),
        })
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
        if command == Command::Prog {
            tracing::warn!("rts execute_on received Prog from a non-CLI path; transmitting anyway");
        }
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
        let hardware = self.hardware.clone();

        tokio::task::spawn_blocking(move || transmit_blocking(hardware, pulses))
            .await
            .context("RTS transmitter task failed")??;

        let mut state = self.state.lock().await;
        state.commit_rolling_code(channel, rolling_code)
    }
}

async fn init_hardware(options: RtsOptions) -> Result<Hardware> {
    tokio::task::spawn_blocking(move || -> Result<Hardware> {
        let spi = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&options.spi_device)
            .with_context(|| format!("opening RTS SPI device {}", options.spi_device))?;
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
        Ok(Hardware { radio, pigpio })
    })
    .await
    .context("RTS hardware init task failed")?
}

fn transmit_blocking(
    hardware: Arc<StdMutex<Hardware>>,
    pulses: Vec<waveform::GpioPulse>,
) -> Result<()> {
    let mut hw = hardware.lock().expect("RTS hardware mutex poisoned");
    hw.pigpio.wave_new()?;
    hw.pigpio.wave_add_generic(&pulses)?;
    let wave_id = hw.pigpio.wave_create()?;

    hw.radio.tx()?;
    let tx_result = (|| -> Result<()> {
        hw.pigpio.wave_tx(wave_id)?;
        while hw.pigpio.wave_busy()? {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
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
