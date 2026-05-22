//! Percentage-position motion planning.

use std::time::Duration;

use crate::config::{BlindTimingOptions, PositioningOptions};
use crate::core::{Channel, Command};
use crate::positioning::state::{Blind, BLINDS, STATUS_DECREASING, STATUS_INCREASING};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BlindMotionTiming {
    pub open: Duration,
    pub close: Duration,
}

impl From<&BlindTimingOptions> for BlindMotionTiming {
    fn from(value: &BlindTimingOptions) -> Self {
        Self {
            open: Duration::from_millis(value.open_ms),
            close: Duration::from_millis(value.close_ms),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotionTimings {
    pub l1: BlindMotionTiming,
    pub l2: BlindMotionTiming,
    pub l3: BlindMotionTiming,
    pub l4: BlindMotionTiming,
}

impl From<PositioningOptions> for MotionTimings {
    fn from(value: PositioningOptions) -> Self {
        Self {
            l1: (&value.l1).into(),
            l2: (&value.l2).into(),
            l3: (&value.l3).into(),
            l4: (&value.l4).into(),
        }
    }
}

impl MotionTimings {
    pub fn for_channel(&self, channel: Channel) -> BlindMotionTiming {
        match channel {
            Channel::L1 => self.l1,
            Channel::L2 => self.l2,
            Channel::L3 => self.l3,
            Channel::L4 => self.l4,
            Channel::All => self.l1,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MotionRequest {
    pub blind: &'static Blind,
    pub current: u8,
    pub target: u8,
    pub timing: BlindMotionTiming,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DriverStart {
    pub channel: Channel,
    pub command: Command,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BlindMovement {
    pub blind: &'static Blind,
    pub current: u8,
    pub target: u8,
    pub command: Command,
    pub status: u8,
    pub duration: Duration,
    pub stop_at_end: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotionPlan {
    pub starts: Vec<DriverStart>,
    pub movements: Vec<BlindMovement>,
}

pub fn plan_motion(requests: &[MotionRequest]) -> MotionPlan {
    let movements = requests
        .iter()
        .filter_map(|request| movement_for(*request))
        .collect::<Vec<_>>();

    let starts = if can_group_start(&movements) {
        vec![DriverStart {
            channel: Channel::All,
            command: movements[0].command,
        }]
    } else {
        movements
            .iter()
            .map(|movement| DriverStart {
                channel: movement.blind.channel,
                command: movement.command,
            })
            .collect()
    };

    MotionPlan { starts, movements }
}

fn movement_for(request: MotionRequest) -> Option<BlindMovement> {
    let current = request.current.min(100);
    let target = request.target.min(100);
    if current == target {
        return None;
    }

    let (command, status, full_travel) = if target > current {
        (Command::Up, STATUS_INCREASING, request.timing.open)
    } else {
        (Command::Down, STATUS_DECREASING, request.timing.close)
    };
    let delta = current.abs_diff(target) as u128;
    let full_ms = full_travel.as_millis();
    let millis = (full_ms * delta).div_ceil(100);

    Some(BlindMovement {
        blind: request.blind,
        current,
        target,
        command,
        status,
        duration: Duration::from_millis(millis.max(1) as u64),
        stop_at_end: !matches!(target, 0 | 100),
    })
}

fn can_group_start(movements: &[BlindMovement]) -> bool {
    let Some(first) = movements.first() else {
        return false;
    };
    movements.iter().all(|m| m.command == first.command)
        && BLINDS
            .iter()
            .all(|blind| movements.iter().any(|m| m.blind.aid == blind.aid))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(open_ms: u64, close_ms: u64) -> BlindMotionTiming {
        BlindMotionTiming {
            open: Duration::from_millis(open_ms),
            close: Duration::from_millis(close_ms),
        }
    }

    #[test]
    fn partial_open_uses_proportional_blind_timing() {
        let plan = plan_motion(&[MotionRequest {
            blind: &BLINDS[0],
            current: 10,
            target: 60,
            timing: timing(30_000, 20_000),
        }]);

        assert_eq!(
            plan.starts,
            vec![DriverStart {
                channel: Channel::L1,
                command: Command::Up,
            }]
        );
        assert_eq!(plan.movements[0].duration, Duration::from_millis(15_000));
        assert_eq!(plan.movements[0].status, STATUS_INCREASING);
        assert!(plan.movements[0].stop_at_end);
    }

    #[test]
    fn partial_close_uses_close_timing() {
        let plan = plan_motion(&[MotionRequest {
            blind: &BLINDS[1],
            current: 80,
            target: 20,
            timing: timing(30_000, 10_000),
        }]);

        assert_eq!(plan.movements[0].command, Command::Down);
        assert_eq!(plan.movements[0].duration, Duration::from_millis(6_000));
        assert_eq!(plan.movements[0].status, STATUS_DECREASING);
        assert!(plan.movements[0].stop_at_end);
    }

    #[test]
    fn endpoint_targets_do_not_schedule_stop() {
        let plan = plan_motion(&[
            MotionRequest {
                blind: &BLINDS[0],
                current: 20,
                target: 100,
                timing: timing(30_000, 20_000),
            },
            MotionRequest {
                blind: &BLINDS[1],
                current: 80,
                target: 0,
                timing: timing(30_000, 20_000),
            },
        ]);

        assert!(!plan.movements[0].stop_at_end);
        assert!(!plan.movements[1].stop_at_end);
    }

    #[test]
    fn full_batch_with_same_direction_starts_as_group() {
        let requests = BLINDS
            .iter()
            .map(|blind| MotionRequest {
                blind,
                current: 0,
                target: 50,
                timing: timing(20_000, 20_000),
            })
            .collect::<Vec<_>>();

        let plan = plan_motion(&requests);

        assert_eq!(
            plan.starts,
            vec![DriverStart {
                channel: Channel::All,
                command: Command::Up,
            }]
        );
        assert_eq!(plan.movements.len(), 4);
    }

    #[test]
    fn mixed_direction_batch_starts_individually() {
        let plan = plan_motion(&[
            MotionRequest {
                blind: &BLINDS[0],
                current: 0,
                target: 50,
                timing: timing(20_000, 20_000),
            },
            MotionRequest {
                blind: &BLINDS[1],
                current: 90,
                target: 50,
                timing: timing(20_000, 20_000),
            },
        ]);

        assert_eq!(
            plan.starts,
            vec![
                DriverStart {
                    channel: Channel::L1,
                    command: Command::Up,
                },
                DriverStart {
                    channel: Channel::L2,
                    command: Command::Down,
                },
            ]
        );
    }
}
