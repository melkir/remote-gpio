use anyhow::Result;
use serde::Deserialize;
use std::convert::TryFrom;
use std::str::FromStr;
use std::time::Duration;

/// Represents the input GPIO pins for LED selection
#[derive(Copy, Clone, Debug, Deserialize, PartialEq, Eq)]
pub enum Input {
    L1 = 21,
    L2 = 20,
    L3 = 16,
    L4 = 12,
    ALL,
}

/// Represents the output GPIO pins for button commands
#[derive(Debug)]
pub enum Output {
    Select = 6,
    Down = 13,
    Stop = 19,
    Up = 26,
}

impl FromStr for Input {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "L1" => Ok(Input::L1),
            "L2" => Ok(Input::L2),
            "L3" => Ok(Input::L3),
            "L4" => Ok(Input::L4),
            _ => Err(anyhow::anyhow!("Invalid input value: {}", s)),
        }
    }
}

impl TryFrom<u32> for Input {
    type Error = anyhow::Error;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            21 => Ok(Input::L1),
            20 => Ok(Input::L2),
            16 => Ok(Input::L3),
            12 => Ok(Input::L4),
            _ => Err(anyhow::anyhow!("Invalid input value: {}", value)),
        }
    }
}

impl std::fmt::Display for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Input::L1 => write!(f, "L1"),
            Input::L2 => write!(f, "L2"),
            Input::L3 => write!(f, "L3"),
            Input::L4 => write!(f, "L4"),
            Input::ALL => write!(f, "ALL"),
        }
    }
}

#[cfg(feature = "hw")]
mod hw {
    use super::*;
    use anyhow::Context;
    use futures::StreamExt;
    use gpiocdev::line::EdgeDetection;
    use gpiocdev::tokio::AsyncRequest;
    use gpiocdev::{line::Value, Request};
    use std::time::Instant;

    /// Monitors GPIO inputs for LED selection changes
    /// Returns the selected LED input or ALL if multiple inputs are detected
    pub async fn watch_inputs() -> Result<Input> {
        let offsets = [
            Input::L1 as u32,
            Input::L2 as u32,
            Input::L3 as u32,
            Input::L4 as u32,
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

        // Collect events within the timeout period
        while event_count < 16 && start_time.elapsed() < timeout_duration {
            if let Some(Ok(event)) = events.next().await {
                last_event = Some(event.offset);
                event_count += 1;
            } else {
                break;
            }
        }

        // Return ALL if multiple events detected, otherwise return the last event
        if event_count < 16 {
            Input::try_from(last_event.unwrap())
        } else {
            Ok(Input::ALL)
        }
    }

    /// Triggers an output GPIO pin for button commands
    pub async fn trigger_output(output: Output) -> Result<()> {
        tracing::debug!("Triggering output: {:?}", output);
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
    const LEDS: [Input; 5] = [Input::L1, Input::L2, Input::L3, Input::L4, Input::ALL];

    pub async fn watch_inputs() -> Result<Input> {
        tokio::time::sleep(Duration::from_millis(60)).await;
        let idx = LED_INDEX.fetch_add(1, Ordering::Relaxed) % LEDS.len() as u8;
        Ok(LEDS[idx as usize])
    }

    pub async fn trigger_output(output: Output) -> Result<()> {
        tracing::debug!("Fake triggering output: {:?}", output);
        tokio::time::sleep(Duration::from_millis(60)).await;
        Ok(())
    }
}

pub use hw::{trigger_output, watch_inputs};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_from_str_valid() {
        assert_eq!(Input::from_str("L1").unwrap(), Input::L1);
        assert_eq!(Input::from_str("L2").unwrap(), Input::L2);
        assert_eq!(Input::from_str("L3").unwrap(), Input::L3);
        assert_eq!(Input::from_str("L4").unwrap(), Input::L4);
    }

    #[test]
    fn input_from_str_invalid() {
        assert!(Input::from_str("L5").is_err());
        assert!(Input::from_str("ALL").is_err());
        assert!(Input::from_str("").is_err());
    }

    #[test]
    fn input_try_from_u32_valid() {
        assert_eq!(Input::try_from(21u32).unwrap(), Input::L1);
        assert_eq!(Input::try_from(20u32).unwrap(), Input::L2);
        assert_eq!(Input::try_from(16u32).unwrap(), Input::L3);
        assert_eq!(Input::try_from(12u32).unwrap(), Input::L4);
    }

    #[test]
    fn input_try_from_u32_invalid() {
        assert!(Input::try_from(0u32).is_err());
        assert!(Input::try_from(99u32).is_err());
    }

    #[test]
    fn input_display_round_trip() {
        for v in [Input::L1, Input::L2, Input::L3, Input::L4] {
            let s = v.to_string();
            assert_eq!(Input::from_str(&s).unwrap(), v);
        }
        assert_eq!(Input::ALL.to_string(), "ALL");
    }
}
