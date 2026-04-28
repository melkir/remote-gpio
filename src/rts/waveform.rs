use crate::rts::frame::{RtsFrame, FRAME_LEN};

pub const WAKEUP_HIGH_US: u32 = 9_415;
pub const WAKEUP_LOW_US: u32 = 89_565;
pub const HARDWARE_SYNC_HIGH_US: u32 = 2_560;
pub const HARDWARE_SYNC_LOW_US: u32 = 2_560;
pub const SOFTWARE_SYNC_HIGH_US: u32 = 4_550;
pub const SOFTWARE_SYNC_LOW_US: u32 = 640;
pub const MANCHESTER_HALF_SYMBOL_US: u32 = 640;
pub const INTER_FRAME_GAP_US: u32 = 30_415;
pub const DEFAULT_FRAME_COUNT: usize = 4;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct GpioPulse {
    pub gpio_on: u32,
    pub gpio_off: u32,
    pub us_delay: u32,
}

pub fn build(frame: RtsFrame, gpio: u8, frame_count: usize) -> Vec<GpioPulse> {
    let mask = 1u32 << gpio;
    let mut pulses = Vec::new();
    let bytes = frame.bytes();
    for index in 0..frame_count {
        append_frame(&mut pulses, mask, &bytes, index == 0);
    }
    pulses
}

fn append_frame(
    pulses: &mut Vec<GpioPulse>,
    mask: u32,
    bytes: &[u8; FRAME_LEN],
    first_frame: bool,
) {
    if first_frame {
        high(pulses, mask, WAKEUP_HIGH_US);
        low(pulses, mask, WAKEUP_LOW_US);
    }

    let hardware_syncs = if first_frame { 2 } else { 7 };
    for _ in 0..hardware_syncs {
        high(pulses, mask, HARDWARE_SYNC_HIGH_US);
        low(pulses, mask, HARDWARE_SYNC_LOW_US);
    }

    high(pulses, mask, SOFTWARE_SYNC_HIGH_US);
    low(pulses, mask, SOFTWARE_SYNC_LOW_US);

    for byte in bytes {
        for bit_index in (0..8).rev() {
            let bit = (byte >> bit_index) & 1;
            append_manchester_bit(pulses, mask, bit);
        }
    }

    low(pulses, mask, INTER_FRAME_GAP_US);
}

fn append_manchester_bit(pulses: &mut Vec<GpioPulse>, mask: u32, bit: u8) {
    match bit {
        1 => {
            low(pulses, mask, MANCHESTER_HALF_SYMBOL_US);
            high(pulses, mask, MANCHESTER_HALF_SYMBOL_US);
        }
        _ => {
            high(pulses, mask, MANCHESTER_HALF_SYMBOL_US);
            low(pulses, mask, MANCHESTER_HALF_SYMBOL_US);
        }
    }
}

fn high(pulses: &mut Vec<GpioPulse>, mask: u32, us_delay: u32) {
    pulses.push(GpioPulse {
        gpio_on: mask,
        gpio_off: 0,
        us_delay,
    });
}

fn low(pulses: &mut Vec<GpioPulse>, mask: u32, us_delay: u32) {
    pulses.push(GpioPulse {
        gpio_on: 0,
        gpio_off: mask,
        us_delay,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rts::frame::{RtsCommand, RtsFrame};

    const GPIO: u8 = 18;
    const MASK: u32 = 1 << GPIO;

    fn test_frame() -> RtsFrame {
        RtsFrame::encode(RtsCommand::Up, 0x00A7, 0x123456).unwrap()
    }

    #[test]
    fn first_frame_contains_wakeup_and_two_hardware_sync_cycles() {
        let pulses = build(test_frame(), GPIO, 1);

        assert_eq!(
            &pulses[..8],
            &[
                GpioPulse {
                    gpio_on: MASK,
                    gpio_off: 0,
                    us_delay: WAKEUP_HIGH_US
                },
                GpioPulse {
                    gpio_on: 0,
                    gpio_off: MASK,
                    us_delay: WAKEUP_LOW_US
                },
                GpioPulse {
                    gpio_on: MASK,
                    gpio_off: 0,
                    us_delay: HARDWARE_SYNC_HIGH_US
                },
                GpioPulse {
                    gpio_on: 0,
                    gpio_off: MASK,
                    us_delay: HARDWARE_SYNC_LOW_US
                },
                GpioPulse {
                    gpio_on: MASK,
                    gpio_off: 0,
                    us_delay: HARDWARE_SYNC_HIGH_US
                },
                GpioPulse {
                    gpio_on: 0,
                    gpio_off: MASK,
                    us_delay: HARDWARE_SYNC_LOW_US
                },
                GpioPulse {
                    gpio_on: MASK,
                    gpio_off: 0,
                    us_delay: SOFTWARE_SYNC_HIGH_US
                },
                GpioPulse {
                    gpio_on: 0,
                    gpio_off: MASK,
                    us_delay: SOFTWARE_SYNC_LOW_US
                },
            ]
        );
    }

    #[test]
    fn repeat_frame_omits_wakeup_and_uses_seven_hardware_sync_cycles() {
        let pulses = build(test_frame(), GPIO, 2);
        let repeat = &pulses[first_frame_pulse_count()..];

        assert_eq!(repeat[0].us_delay, HARDWARE_SYNC_HIGH_US);
        assert_eq!(repeat[1].us_delay, HARDWARE_SYNC_LOW_US);
        assert_eq!(repeat[13].us_delay, HARDWARE_SYNC_LOW_US);
        assert_eq!(repeat[14].us_delay, SOFTWARE_SYNC_HIGH_US);
        assert_eq!(repeat[15].us_delay, SOFTWARE_SYNC_LOW_US);
    }

    #[test]
    fn manchester_bits_are_emitted_msb_first() {
        let pulses = build(test_frame(), GPIO, 1);
        let data = &pulses[8..16];

        // First encoded byte is 0xA0: 1,0,1,0...
        assert_eq!(
            data,
            &[
                low_pulse(),
                high_pulse(),
                high_pulse(),
                low_pulse(),
                low_pulse(),
                high_pulse(),
                high_pulse(),
                low_pulse(),
            ]
        );
    }

    #[test]
    fn pulse_count_and_total_duration_match_default_frame_count() {
        let pulses = build(test_frame(), GPIO, DEFAULT_FRAME_COUNT);

        assert_eq!(pulses.len(), 508);
        assert_eq!(total_duration(&pulses), 645_880);
    }

    fn first_frame_pulse_count() -> usize {
        2 + 4 + 2 + (FRAME_LEN * 8 * 2) + 1
    }

    fn total_duration(pulses: &[GpioPulse]) -> u32 {
        pulses.iter().map(|pulse| pulse.us_delay).sum()
    }

    fn high_pulse() -> GpioPulse {
        GpioPulse {
            gpio_on: MASK,
            gpio_off: 0,
            us_delay: MANCHESTER_HALF_SYMBOL_US,
        }
    }

    fn low_pulse() -> GpioPulse {
        GpioPulse {
            gpio_on: 0,
            gpio_off: MASK,
            us_delay: MANCHESTER_HALF_SYMBOL_US,
        }
    }
}
