use anyhow::{Context, Result};
use futures::StreamExt;
use gpiocdev::line::EdgeDetection;
use gpiocdev::tokio::AsyncRequest;
use gpiocdev::{line::Value, Request};
use serde::Deserialize;
use std::convert::TryFrom;
use std::str::FromStr;
use std::time::{Duration, Instant};

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

impl Output {
    /// Converts a string command to an Output enum variant
    pub fn from_str(value: &str) -> Option<Output> {
        match value {
            "select" => Some(Output::Select),
            "up" => Some(Output::Up),
            "stop" => Some(Output::Stop),
            "down" => Some(Output::Down),
            _ => None,
        }
    }
}

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
