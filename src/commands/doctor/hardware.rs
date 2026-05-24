use std::net::TcpStream;
use std::time::Duration;

use super::check::{read_write_file, readable_file, Check};
use super::Status;
use crate::config::RtsOptions;
use crate::driver::{pigpiod_addr_list, pigpiod_addrs, PIGPIOD_PORT};
use crate::gpio::{GpioOptions, MAX_BCM_GPIO};
use crate::persist;
use crate::rts::state::{RtsState, SCHEMA_VERSION, STATE_FILE};

pub fn gpio_chip(options: &GpioOptions) -> Check {
    readable_file("gpio_chip_accessible", "GPIO", &options.chip)
}

pub fn fake_gpio_skipped() -> Check {
    Check::new("gpio_chip_accessible", "GPIO")
        .skipped()
        .detail("fake driver selected")
}

pub fn rts_checks(options: &RtsOptions) -> Vec<Check> {
    vec![
        read_write_file("rts_spi_device", "RTS SPI", &options.spi_device),
        rts_gdo0(options.gpio.gdo0),
        pigpiod(),
        Check::new("pigpiod_localhost_only", "pigpiod local").detail(format!(
            "loopback only ({}, port {PIGPIOD_PORT})",
            pigpiod_addr_list()
        )),
        rts_state_file(),
    ]
}

fn rts_gdo0(gpio: u8) -> Check {
    if gpio <= MAX_BCM_GPIO {
        Check::new("rts_gdo0_gpio", "RTS GDO0").detail(format!("BCM{gpio}"))
    } else {
        Check::new("rts_gdo0_gpio", "RTS GDO0")
            .status(Status::Blocking)
            .detail(format!("BCM{gpio} out of range (0..={MAX_BCM_GPIO})"))
    }
}

fn pigpiod() -> Check {
    let connected = pigpiod_addrs().into_iter().find_map(|addr| {
        TcpStream::connect_timeout(&addr, Duration::from_millis(500))
            .ok()
            .map(|_| addr.to_string())
    });
    match connected {
        Some(addr) => Check::new("pigpiod", "pigpiod").detail(addr),
        None => Check::new("pigpiod", "pigpiod")
            .status(Status::Blocking)
            .detail(format!("{}: Connection refused", pigpiod_addr_list())),
    }
}

fn rts_state_file() -> Check {
    let path = persist::state_dir().join(STATE_FILE);
    let display = path.display().to_string();
    if !path.exists() {
        return Check::new("rts_state_file", "RTS state")
            .status(Status::Advisory)
            .detail(format!("{display} not yet created"));
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<RtsState>(&text) {
            Ok(state) if state.schema_version == SCHEMA_VERSION => {
                Check::new("rts_state_file", "RTS state").detail(display)
            }
            Ok(state) => Check::new("rts_state_file", "RTS state")
                .status(Status::Blocking)
                .detail(format!(
                    "{display}: schema_version {} unsupported (expected {})",
                    state.schema_version, SCHEMA_VERSION
                )),
            Err(e) => Check::new("rts_state_file", "RTS state")
                .status(Status::Blocking)
                .detail(format!("{display}: parse error: {e}")),
        },
        Err(e) => Check::new("rts_state_file", "RTS state")
            .status(Status::Blocking)
            .detail(format!("{display}: {e}")),
    }
}
