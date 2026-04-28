use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;
use std::str::FromStr;
use std::time::Duration;

/// Logical remote target selected by the Telis LED row.
#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum Channel {
    L1 = 21,
    L2 = 20,
    L3 = 16,
    L4 = 12,
    ALL,
}

/// Represents the Telis button GPIO pins driven by the wired backend.
#[derive(Debug)]
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

#[cfg(feature = "hw")]
mod hw {
    use super::*;
    use anyhow::Context;
    use futures_util::StreamExt;
    use gpiocdev::line::EdgeDetection;
    use gpiocdev::tokio::AsyncRequest;
    use gpiocdev::{line::Value, Request};
    use std::time::Instant;

    /// Monitors GPIO inputs for LED selection changes
    /// Returns the selected LED input or ALL if multiple inputs are detected
    pub async fn watch_inputs() -> Result<Channel> {
        let offsets = [
            Channel::L1 as u32,
            Channel::L2 as u32,
            Channel::L3 as u32,
            Channel::L4 as u32,
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
            Channel::try_from(last_event.unwrap())
        } else {
            Ok(Channel::ALL)
        }
    }

    /// Triggers a Telis button GPIO pin.
    pub async fn trigger_output(output: TelisButton) -> Result<()> {
        tracing::debug!("Triggering Telis button: {:?}", output);
        let offset = output as u32;
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
        tokio::time::sleep(Duration::from_millis(60)).await;

        // Release the button
        value = value.not();
        req.set_lone_value(value)
            .context("Failed to set output value")?;

        Ok(())
    }
}

#[cfg(all(feature = "fake", not(feature = "hw")))]
mod hw {
    use super::*;
    use std::sync::atomic::{AtomicU8, Ordering};

    static LED_INDEX: AtomicU8 = AtomicU8::new(0);
    const LEDS: [Channel; 5] = [
        Channel::L1,
        Channel::L2,
        Channel::L3,
        Channel::L4,
        Channel::ALL,
    ];

    pub async fn watch_inputs() -> Result<Channel> {
        tokio::time::sleep(Duration::from_millis(60)).await;
        let idx = LED_INDEX.fetch_add(1, Ordering::Relaxed) % LEDS.len() as u8;
        Ok(LEDS[idx as usize])
    }

    pub async fn trigger_output(output: TelisButton) -> Result<()> {
        tracing::debug!("Fake triggering Telis button: {:?}", output);
        tokio::time::sleep(Duration::from_millis(60)).await;
        Ok(())
    }
}

pub use hw::{trigger_output, watch_inputs};

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
