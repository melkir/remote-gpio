//! Fake controllers and positioning presets for unit tests.

use std::collections::HashMap;
use std::sync::Arc;

use crate::config::{BlindTimingOptions, DriverConfig, PositioningOptions};
use crate::controller::BlindController;

/// Per-channel open/close timings with the same duration on L1–L4.
pub fn uniform_positioning(ms: u64) -> PositioningOptions {
    let timing = BlindTimingOptions {
        open_ms: ms,
        close_ms: ms,
    };
    PositioningOptions {
        l1: timing.clone(),
        l2: timing.clone(),
        l3: timing.clone(),
        l4: timing,
    }
}

/// L1 timing override only; other channels keep defaults (tests that drive blind 1 / aid 2).
pub fn uniform_positioning_l1_ms(ms: u64) -> PositioningOptions {
    PositioningOptions {
        l1: BlindTimingOptions {
            open_ms: ms,
            close_ms: ms,
        },
        ..PositioningOptions::default()
    }
}

/// Initial current positions for the four HomeKit blind accessories (aids 2–5).
pub fn four_blind_positions(current: u8) -> HashMap<u64, u8> {
    HashMap::from([(2, current), (3, current), (4, current), (5, current)])
}

/// Fake driver, uniform timings, four blinds at 100% — typical HomeKit motion tests.
pub async fn fake_four_blinds(ms: u64) -> Arc<BlindController> {
    fake_controller(uniform_positioning(ms), four_blind_positions(100)).await
}

/// Fake driver with explicit positioning config and initial current positions.
pub async fn fake_controller(
    positioning: PositioningOptions,
    positions: HashMap<u64, u8>,
) -> Arc<BlindController> {
    Arc::new(
        BlindController::with_driver_and_positions_for_test(
            DriverConfig::fake(),
            positioning,
            positions,
        )
        .await
        .unwrap(),
    )
}
