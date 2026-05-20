use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::driver::TelisGpioOptions;

pub const DEFAULT_GPIO_CHIP: &str = "/dev/gpiochip0";
pub const MAX_BCM_GPIO: u8 = 31;

const CHANNELS: [(Channel, &str); 4] = [
    (Channel::L1, "L1"),
    (Channel::L2, "L2"),
    (Channel::L3, "L3"),
    (Channel::L4, "L4"),
];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct GpioOptions {
    pub chip: String,
}

impl Default for GpioOptions {
    fn default() -> Self {
        Self {
            chip: DEFAULT_GPIO_CHIP.to_string(),
        }
    }
}

/// Logical remote target selected by the Telis LED row.
#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Channel {
    L1,
    L2,
    L3,
    L4,
    ALL,
}

impl Channel {
    pub const INDIVIDUALS: [Channel; 4] = [Channel::L1, Channel::L2, Channel::L3, Channel::L4];

    pub fn led_gpio(self, config: &TelisGpioOptions) -> u8 {
        match self {
            Channel::L1 => config.led1,
            Channel::L2 => config.led2,
            Channel::L3 => config.led3,
            Channel::L4 => config.led4,
            Channel::ALL => unreachable!("ALL is not represented by one Telis LED GPIO"),
        }
    }

    /// Advance the Telis selector one step (L1 → L2 → … → ALL → L1).
    pub fn next(self) -> Self {
        match self {
            Channel::L1 => Channel::L2,
            Channel::L2 => Channel::L3,
            Channel::L3 => Channel::L4,
            Channel::L4 => Channel::ALL,
            Channel::ALL => Channel::L1,
        }
    }
}

/// Represents the Telis button GPIO pins driven by the wired driver.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TelisButton {
    Select = 6,
    Down = 13,
    Stop = 19,
    Up = 26,
}

impl FromStr for Channel {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ALL" => Ok(Channel::ALL),
            _ => CHANNELS
                .iter()
                .find(|(_, name)| *name == s)
                .map(|(ch, _)| *ch)
                .ok_or_else(|| anyhow::anyhow!("Invalid channel value: {s}")),
        }
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Channel::ALL => write!(f, "ALL"),
            other => CHANNELS
                .iter()
                .find(|(ch, _)| ch == other)
                .map(|(_, name)| write!(f, "{name}"))
                .unwrap_or_else(|| write!(f, "{other:?}")),
        }
    }
}

pub fn channel_from_gpio(offset: u32, config: &TelisGpioOptions) -> Result<Channel> {
    let gpio = offset as u8;
    for channel in &Channel::INDIVIDUALS {
        if channel.led_gpio(config) == gpio {
            return Ok(*channel);
        }
    }
    Err(anyhow::anyhow!("Invalid channel GPIO value: {offset}"))
}

pub fn button_gpio(button: TelisButton, config: &TelisGpioOptions) -> u8 {
    match button {
        TelisButton::Select => config.select,
        TelisButton::Down => config.down,
        TelisButton::Stop => config.stop,
        TelisButton::Up => config.up,
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use anyhow::Context;
    use futures_util::StreamExt;
    use gpiocdev::line::EdgeDetection;
    use gpiocdev::tokio::AsyncRequest;
    use gpiocdev::{line::Value, Request};
    use std::time::Duration;

    /// Monitors GPIO inputs for LED selection changes
    /// Returns the selected LED input or ALL if multiple inputs are detected
    pub async fn watch_inputs(chip: &str, config: &TelisGpioOptions) -> Result<Channel> {
        let offsets: Vec<u32> = Channel::INDIVIDUALS
            .iter()
            .map(|ch| ch.led_gpio(config) as u32)
            .collect();

        let req = Request::builder()
            .on_chip(chip)
            .with_lines(&offsets)
            .as_input()
            .with_edge_detection(EdgeDetection::BothEdges)
            .request()
            .context("Failed to request GPIO lines")?;

        let areq = AsyncRequest::new(req);
        let mut events = areq.edge_events();

        let timeout_duration = Duration::from_millis(300);
        let mut last_event = None;
        let mut event_count = 0;

        // Threshold: 4 inputs × 2 edges (rising + falling) × 2 transitions = 16 events.
        // When all LEDs are lit (Channel::ALL), every input toggles, producing many edges.
        const ALL_EVENTS_THRESHOLD: u32 = 16;

        let deadline = tokio::time::Instant::now() + timeout_duration;

        while event_count < ALL_EVENTS_THRESHOLD {
            match tokio::time::timeout_at(deadline, events.next()).await {
                Ok(Some(Ok(event))) => {
                    last_event = Some(event.offset);
                    event_count += 1;
                }
                Ok(Some(Err(err))) => return Err(err).context("reading GPIO edge event"),
                Ok(None) => break,
                Err(_) => break,
            }
        }

        if event_count < ALL_EVENTS_THRESHOLD {
            let gpio = last_event
                .ok_or_else(|| anyhow::anyhow!("Timed out waiting for Telis LED GPIO edge"))?;
            channel_from_gpio(gpio, config)
        } else {
            Ok(Channel::ALL)
        }
    }

    /// Triggers a Telis button GPIO pin.
    pub async fn trigger_output(
        chip: &str,
        output: TelisButton,
        config: &TelisGpioOptions,
    ) -> Result<()> {
        tracing::debug!("Triggering Telis button: {:?}", output);
        trigger_output_gpio(chip, button_gpio(output, config), Duration::from_millis(60)).await
    }

    /// Triggers one active-low GPIO output for `duration`.
    pub async fn trigger_output_gpio(chip: &str, gpio: u8, duration: Duration) -> Result<()> {
        tracing::debug!("Triggering GPIO{gpio} for {:?}", duration);
        let offset = gpio as u32;
        let mut value = Value::Active;

        let req = Request::builder()
            .on_chip(chip)
            .with_line(offset)
            .as_output(value)
            .as_active_low()
            .request()
            .context("Failed to request output line")?;

        tokio::time::sleep(duration).await;

        value = value.not();
        req.set_lone_value(value)
            .context("Failed to set output value")?;

        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::time::Duration;

    use super::*;

    static LED_INDEX: AtomicU8 = AtomicU8::new(0);
    const LEDS: [Channel; 5] = [
        Channel::L1,
        Channel::L2,
        Channel::L3,
        Channel::L4,
        Channel::ALL,
    ];

    pub async fn watch_inputs(_chip: &str, _config: &TelisGpioOptions) -> Result<Channel> {
        tokio::time::sleep(Duration::from_millis(60)).await;
        let idx = LED_INDEX.fetch_add(1, Ordering::Relaxed) % LEDS.len() as u8;
        Ok(LEDS[idx as usize])
    }

    pub async fn trigger_output(
        _chip: &str,
        output: TelisButton,
        config: &TelisGpioOptions,
    ) -> Result<()> {
        tracing::debug!("Fake triggering Telis button: {:?}", output);
        let _ = button_gpio(output, config);
        tokio::time::sleep(Duration::from_millis(60)).await;
        Ok(())
    }

    pub async fn trigger_output_gpio(_chip: &str, gpio: u8, duration: Duration) -> Result<()> {
        tracing::debug!("Fake triggering GPIO{gpio} for {:?}", duration);
        tokio::time::sleep(duration).await;
        Ok(())
    }
}

pub use platform::{trigger_output, trigger_output_gpio, watch_inputs};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::TelisGpioOptions;

    #[test]
    fn channel_from_str_valid() {
        for (ch, name) in CHANNELS {
            assert_eq!(Channel::from_str(name).unwrap(), ch);
        }
        assert_eq!(Channel::from_str("ALL").unwrap(), Channel::ALL);
    }

    #[test]
    fn channel_from_str_invalid() {
        assert!(Channel::from_str("L5").is_err());
        assert!(Channel::from_str("").is_err());
    }

    #[test]
    fn channel_from_gpio_uses_config_pins() {
        let config = TelisGpioOptions::default();
        assert_eq!(
            channel_from_gpio(config.led1 as u32, &config).unwrap(),
            Channel::L1
        );
        assert_eq!(
            channel_from_gpio(config.led4 as u32, &config).unwrap(),
            Channel::L4
        );
        assert!(channel_from_gpio(99, &config).is_err());
    }

    #[test]
    fn channel_display_round_trip() {
        for ch in [
            Channel::L1,
            Channel::L2,
            Channel::L3,
            Channel::L4,
            Channel::ALL,
        ] {
            let s = ch.to_string();
            assert_eq!(Channel::from_str(&s).unwrap(), ch);
        }
    }
}
