use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;
use std::str::FromStr;

use crate::driver::TelisGpioOptions;

pub const MAX_BCM_GPIO: u8 = 31;

/// Logical remote target selected by the Telis LED row.
#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Channel {
    L1 = 21,
    L2 = 20,
    L3 = 16,
    L4 = 12,
    ALL,
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
            "L1" => Ok(Channel::L1),
            "L2" => Ok(Channel::L2),
            "L3" => Ok(Channel::L3),
            "L4" => Ok(Channel::L4),
            "ALL" => Ok(Channel::ALL),
            _ => Err(anyhow::anyhow!("Invalid channel value: {}", s)),
        }
    }
}

impl TryFrom<u32> for Channel {
    type Error = anyhow::Error;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            21 => Ok(Channel::L1),
            20 => Ok(Channel::L2),
            16 => Ok(Channel::L3),
            12 => Ok(Channel::L4),
            _ => Err(anyhow::anyhow!("Invalid channel value: {}", value)),
        }
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Channel::L1 => write!(f, "L1"),
            Channel::L2 => write!(f, "L2"),
            Channel::L3 => write!(f, "L3"),
            Channel::L4 => write!(f, "L4"),
            Channel::ALL => write!(f, "ALL"),
        }
    }
}

pub fn channel_gpio(channel: Channel, config: &TelisGpioOptions) -> u8 {
    match channel {
        Channel::L1 => config.led1,
        Channel::L2 => config.led2,
        Channel::L3 => config.led3,
        Channel::L4 => config.led4,
        Channel::ALL => unreachable!("ALL is not represented by one Telis LED GPIO"),
    }
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
    use std::time::Instant;

    /// Monitors GPIO inputs for LED selection changes
    /// Returns the selected LED input or ALL if multiple inputs are detected
    pub async fn watch_inputs(config: &TelisGpioOptions) -> Result<Channel> {
        let offsets = [
            config.led1 as u32,
            config.led2 as u32,
            config.led3 as u32,
            config.led4 as u32,
        ];

        // Request multiple input lines with edge detection
        let req = Request::builder()
            .on_chip("/dev/gpiochip0")
            .with_lines(&offsets)
            .as_input()
            .with_edge_detection(EdgeDetection::BothEdges)
            .request()
            .context("Failed to request GPIO lines")?;

        let areq = AsyncRequest::new(req);
        let mut events = areq.edge_events();

        let start_time = Instant::now();
        let timeout_duration = Duration::from_millis(300);
        let mut last_event = None;
        let mut event_count = 0;

        // Threshold: 4 inputs × 2 edges (rising + falling) × 2 transitions = 16 events.
        // When all LEDs are lit (Channel::ALL), every input toggles, producing many edges.
        const ALL_EVENTS_THRESHOLD: u32 = 16;

        // Collect events within the timeout period
        while event_count < ALL_EVENTS_THRESHOLD && start_time.elapsed() < timeout_duration {
            if let Some(Ok(event)) = events.next().await {
                last_event = Some(event.offset);
                event_count += 1;
            } else {
                break;
            }
        }

        // Return ALL if multiple events detected, otherwise return the last event
        if event_count < ALL_EVENTS_THRESHOLD {
            channel_from_gpio(last_event.unwrap(), config)
        } else {
            Ok(Channel::ALL)
        }
    }

    fn channel_from_gpio(value: u32, config: &TelisGpioOptions) -> Result<Channel> {
        match value as u8 {
            gpio if gpio == config.led1 => Ok(Channel::L1),
            gpio if gpio == config.led2 => Ok(Channel::L2),
            gpio if gpio == config.led3 => Ok(Channel::L3),
            gpio if gpio == config.led4 => Ok(Channel::L4),
            _ => Err(anyhow::anyhow!("Invalid channel GPIO value: {}", value)),
        }
    }

    /// Triggers a Telis button GPIO pin.
    pub async fn trigger_output(output: TelisButton, config: &TelisGpioOptions) -> Result<()> {
        tracing::debug!("Triggering Telis button: {:?}", output);
        trigger_output_gpio(button_gpio(output, config), Duration::from_millis(60)).await
    }

    /// Triggers one active-low GPIO output for `duration`.
    pub async fn trigger_output_gpio(gpio: u8, duration: Duration) -> Result<()> {
        tracing::debug!("Triggering GPIO{gpio} for {:?}", duration);
        let offset = gpio as u32;
        let mut value = Value::Active;

        // Request the output line
        let req = Request::builder()
            .on_chip("/dev/gpiochip0")
            .with_line(offset)
            .as_output(value)
            .as_active_low()
            .request()
            .context("Failed to request output line")?;

        // Hold the button for minimum detection time
        tokio::time::sleep(duration).await;

        // Release the button
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

    pub async fn watch_inputs(_config: &TelisGpioOptions) -> Result<Channel> {
        tokio::time::sleep(Duration::from_millis(60)).await;
        let idx = LED_INDEX.fetch_add(1, Ordering::Relaxed) % LEDS.len() as u8;
        Ok(LEDS[idx as usize])
    }

    pub async fn trigger_output(output: TelisButton, config: &TelisGpioOptions) -> Result<()> {
        tracing::debug!("Fake triggering Telis button: {:?}", output);
        let _ = button_gpio(output, config);
        tokio::time::sleep(Duration::from_millis(60)).await;
        Ok(())
    }

    pub async fn trigger_output_gpio(gpio: u8, duration: Duration) -> Result<()> {
        tracing::debug!("Fake triggering GPIO{gpio} for {:?}", duration);
        tokio::time::sleep(duration).await;
        Ok(())
    }
}

pub use platform::{trigger_output, trigger_output_gpio, watch_inputs};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_from_str_valid() {
        assert_eq!(Channel::from_str("L1").unwrap(), Channel::L1);
        assert_eq!(Channel::from_str("L2").unwrap(), Channel::L2);
        assert_eq!(Channel::from_str("L3").unwrap(), Channel::L3);
        assert_eq!(Channel::from_str("L4").unwrap(), Channel::L4);
        assert_eq!(Channel::from_str("ALL").unwrap(), Channel::ALL);
    }

    #[test]
    fn channel_from_str_invalid() {
        assert!(Channel::from_str("L5").is_err());
        assert!(Channel::from_str("").is_err());
    }

    #[test]
    fn channel_try_from_u32_valid() {
        assert_eq!(Channel::try_from(21u32).unwrap(), Channel::L1);
        assert_eq!(Channel::try_from(20u32).unwrap(), Channel::L2);
        assert_eq!(Channel::try_from(16u32).unwrap(), Channel::L3);
        assert_eq!(Channel::try_from(12u32).unwrap(), Channel::L4);
    }

    #[test]
    fn channel_try_from_u32_invalid() {
        assert!(Channel::try_from(0u32).is_err());
        assert!(Channel::try_from(99u32).is_err());
    }

    #[test]
    fn channel_display_round_trip() {
        for v in [
            Channel::L1,
            Channel::L2,
            Channel::L3,
            Channel::L4,
            Channel::ALL,
        ] {
            let s = v.to_string();
            assert_eq!(Channel::from_str(&s).unwrap(), v);
        }
    }
}
