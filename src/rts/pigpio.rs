use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::rts::waveform::GpioPulse;

const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);

const CMD_MODES: u32 = 0;
const CMD_WRITE: u32 = 4;
const CMD_WVCLR: u32 = 27;
const CMD_WVAG: u32 = 28;
const CMD_WVBSY: u32 = 32;
const CMD_WVHLT: u32 = 33;
const CMD_WVCRE: u32 = 49;
const CMD_WVDEL: u32 = 50;
const CMD_WVTX: u32 = 51;
const CMD_WVNEW: u32 = 53;

const PI_OUTPUT: u32 = 1;

#[derive(Debug)]
pub struct PigpioClient<S> {
    stream: S,
}

impl PigpioClient<TcpStream> {
    pub fn connect(addr: impl ToSocketAddrs) -> Result<Self> {
        let stream = TcpStream::connect(addr).context("connecting to pigpiod")?;
        // Each command is a 16-byte write followed by a read; Nagle would add up
        // to ~40ms of artificial delay per round-trip.
        stream
            .set_nodelay(true)
            .context("enabling TCP_NODELAY on pigpiod socket")?;
        stream
            .set_read_timeout(Some(SOCKET_TIMEOUT))
            .context("setting pigpiod read timeout")?;
        stream
            .set_write_timeout(Some(SOCKET_TIMEOUT))
            .context("setting pigpiod write timeout")?;
        Ok(Self::new(stream))
    }
}

impl<S: Read + Write> PigpioClient<S> {
    pub fn new(stream: S) -> Self {
        Self { stream }
    }

    #[cfg(test)]
    pub fn into_inner(self) -> S {
        self.stream
    }

    pub fn set_output(&mut self, gpio: u8) -> Result<()> {
        self.command(CMD_MODES, gpio as u32, PI_OUTPUT).map(|_| ())
    }

    pub fn write_level(&mut self, gpio: u8, high: bool) -> Result<()> {
        self.command(CMD_WRITE, gpio as u32, u32::from(high))
            .map(|_| ())
    }

    pub fn wave_clear(&mut self) -> Result<()> {
        self.command(CMD_WVCLR, 0, 0).map(|_| ())
    }

    pub fn wave_new(&mut self) -> Result<()> {
        self.command(CMD_WVNEW, 0, 0).map(|_| ())
    }

    pub fn wave_add_generic(&mut self, pulses: &[GpioPulse]) -> Result<()> {
        let mut extension = Vec::with_capacity(pulses.len() * 12);
        for pulse in pulses {
            extension.extend_from_slice(&pulse.gpio_on.to_le_bytes());
            extension.extend_from_slice(&pulse.gpio_off.to_le_bytes());
            extension.extend_from_slice(&pulse.us_delay.to_le_bytes());
        }
        self.command_ext(CMD_WVAG, pulses.len() as u32, 0, &extension)
            .map(|_| ())
    }

    pub fn wave_create(&mut self) -> Result<u32> {
        Ok(self.command(CMD_WVCRE, 0, 0)? as u32)
    }

    pub fn wave_tx(&mut self, wave_id: u32) -> Result<()> {
        self.command(CMD_WVTX, wave_id, 0).map(|_| ())
    }

    pub fn wave_busy(&mut self) -> Result<bool> {
        Ok(self.command(CMD_WVBSY, 0, 0)? != 0)
    }

    pub fn wave_delete(&mut self, wave_id: u32) -> Result<()> {
        self.command(CMD_WVDEL, wave_id, 0).map(|_| ())
    }

    pub fn wave_halt(&mut self) -> Result<()> {
        self.command(CMD_WVHLT, 0, 0).map(|_| ())
    }

    fn command(&mut self, command: u32, p1: u32, p2: u32) -> Result<i32> {
        self.command_ext(command, p1, p2, &[])
    }

    fn command_ext(&mut self, command: u32, p1: u32, p2: u32, extension: &[u8]) -> Result<i32> {
        let mut request = Vec::with_capacity(16 + extension.len());
        request.extend_from_slice(&command.to_le_bytes());
        request.extend_from_slice(&p1.to_le_bytes());
        request.extend_from_slice(&p2.to_le_bytes());
        request.extend_from_slice(&(extension.len() as u32).to_le_bytes());
        request.extend_from_slice(extension);
        self.stream.write_all(&request)?;

        // pigpiod replies with a 16-byte header. Some commands (SPI/I²C reads,
        // BSPIX, etc.) follow that with `result` extension bytes; none of the
        // commands we issue do, so a fixed 16-byte read is correct. Adding any
        // extension-returning command requires draining those bytes here.
        let mut response = [0u8; 16];
        self.stream.read_exact(&mut response)?;
        if response[0..12] != request[0..12] {
            let echoed_cmd = u32::from_le_bytes(
                response[0..4]
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("pigpiod response header too short"))?,
            );
            bail!(
                "pigpiod reply did not match request: sent {} ({}), got command field {} ({})",
                command_name(command),
                command,
                command_name(echoed_cmd),
                echoed_cmd,
            );
        }
        let result = i32::from_le_bytes(
            response[12..16]
                .try_into()
                .map_err(|_| anyhow::anyhow!("pigpiod response header too short"))?,
        );
        if result < 0 {
            let (name, description) = pi_error(result);
            bail!(
                "pigpiod {} ({}) failed: {} ({}) {}",
                command_name(command),
                command,
                name,
                result,
                description,
            );
        }
        Ok(result)
    }
}

fn command_name(command: u32) -> &'static str {
    match command {
        CMD_MODES => "MODES",
        CMD_WRITE => "WRITE",
        CMD_WVCLR => "WVCLR",
        CMD_WVAG => "WVAG",
        CMD_WVBSY => "WVBSY",
        CMD_WVHLT => "WVHLT",
        CMD_WVCRE => "WVCRE",
        CMD_WVDEL => "WVDEL",
        CMD_WVTX => "WVTX",
        CMD_WVNEW => "WVNEW",
        _ => "UNKNOWN",
    }
}

// pigpiod error codes derived from pigpio.h (Unlicense / public domain).
// Restricted to codes that GPIO mode/write and waveform commands can return.
fn pi_error(code: i32) -> (&'static str, &'static str) {
    match code {
        -1 => ("PI_INIT_FAILED", "gpioInitialise failed"),
        -2 => ("PI_BAD_USER_GPIO", "GPIO not 0-31"),
        -3 => ("PI_BAD_GPIO", "GPIO not 0-53"),
        -4 => ("PI_BAD_MODE", "mode not 0-7"),
        -5 => ("PI_BAD_LEVEL", "level not 0-1"),
        -31 => (
            "PI_NOT_INITIALISED",
            "function called before gpioInitialise",
        ),
        -33 => ("PI_BAD_WAVE_MODE", "waveform mode not 0-3"),
        -36 => ("PI_TOO_MANY_PULSES", "waveform has too many pulses"),
        -37 => ("PI_TOO_MANY_CHARS", "waveform has too many chars"),
        -41 => ("PI_NOT_PERMITTED", "GPIO operation not permitted"),
        -42 => ("PI_SOME_PERMITTED", "one or more GPIO not permitted"),
        -58 => ("PI_NO_MEMORY", "can't allocate temporary memory"),
        -66 => ("PI_BAD_WAVE_ID", "non existent wave id"),
        -67 => ("PI_TOO_MANY_CBS", "no more CBs for waveform"),
        -68 => ("PI_TOO_MANY_OOL", "no more OOL for waveform"),
        -69 => ("PI_EMPTY_WAVEFORM", "attempt to create an empty waveform"),
        -70 => ("PI_NO_WAVEFORM_ID", "no more waveforms"),
        -88 => ("PI_UNKNOWN_COMMAND", "unknown command"),
        -103 => ("PI_MSG_TOOBIG", "socket/pipe message too big"),
        _ => ("PI_UNKNOWN", "unrecognised pigpiod error code"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;

    #[derive(Debug, Default)]
    struct FakeStream {
        written: Vec<u8>,
        results: VecDeque<i32>,
        parse_cursor: usize,
        pending_response: Option<[u8; 16]>,
        force_mismatch: bool,
    }

    impl FakeStream {
        fn with_results(results: &[i32]) -> Self {
            Self {
                results: results.iter().copied().collect(),
                ..Self::default()
            }
        }

        fn with_mismatch(result: i32) -> Self {
            Self {
                results: VecDeque::from([result]),
                force_mismatch: true,
                ..Self::default()
            }
        }

        fn parse_next_request(&mut self) {
            assert!(self.pending_response.is_none());
            assert!(self.parse_cursor + 16 <= self.written.len());
            let header = &self.written[self.parse_cursor..self.parse_cursor + 16];
            let ext_len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
            let mut response = [0u8; 16];
            response[0..12].copy_from_slice(&header[0..12]);
            if self.force_mismatch {
                // Flip the echoed command word.
                response[0] ^= 0xff;
            }
            let result = self.results.pop_front().expect("result for command");
            response[12..16].copy_from_slice(&result.to_le_bytes());
            self.pending_response = Some(response);
            self.parse_cursor += 16 + ext_len;
        }
    }

    impl Read for FakeStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pending_response.is_none() {
                if self.parse_cursor + 16 > self.written.len() || self.results.is_empty() {
                    return Ok(0);
                }
                self.parse_next_request();
            }
            let response = self.pending_response.take().unwrap();
            buf[..response.len()].copy_from_slice(&response);
            Ok(response.len())
        }
    }

    impl Write for FakeStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn encodes_mode_write_and_wave_lifecycle_commands() {
        let stream = FakeStream::with_results(&[0, 0, 0, 0, 3, 0, 1, 0, 0]);
        let mut client = PigpioClient::new(stream);

        client.set_output(18).unwrap();
        client.write_level(18, false).unwrap();
        client.wave_clear().unwrap();
        client.wave_new().unwrap();
        assert_eq!(client.wave_create().unwrap(), 3);
        client.wave_tx(3).unwrap();
        assert!(client.wave_busy().unwrap());
        client.wave_delete(3).unwrap();
        client.wave_halt().unwrap();

        let stream = client.into_inner();
        let commands: Vec<u32> = stream
            .written
            .chunks_exact(16)
            .map(|chunk| u32::from_le_bytes(chunk[0..4].try_into().unwrap()))
            .collect();
        assert_eq!(
            commands,
            vec![
                CMD_MODES, CMD_WRITE, CMD_WVCLR, CMD_WVNEW, CMD_WVCRE, CMD_WVTX, CMD_WVBSY,
                CMD_WVDEL, CMD_WVHLT
            ]
        );
    }

    #[test]
    fn encodes_wave_add_generic_extension() {
        let stream = FakeStream::with_results(&[0]);
        let mut client = PigpioClient::new(stream);

        client
            .wave_add_generic(&[
                GpioPulse {
                    gpio_on: 1,
                    gpio_off: 0,
                    us_delay: 640,
                },
                GpioPulse {
                    gpio_on: 0,
                    gpio_off: 1,
                    us_delay: 640,
                },
            ])
            .unwrap();

        let stream = client.into_inner();
        assert_eq!(u32_at(&stream.written, 0), CMD_WVAG);
        assert_eq!(u32_at(&stream.written, 4), 2);
        assert_eq!(u32_at(&stream.written, 12), 24);
        assert_eq!(u32_at(&stream.written, 16), 1);
        assert_eq!(u32_at(&stream.written, 20), 0);
        assert_eq!(u32_at(&stream.written, 24), 640);
        assert_eq!(u32_at(&stream.written, 28), 0);
        assert_eq!(u32_at(&stream.written, 32), 1);
        assert_eq!(u32_at(&stream.written, 36), 640);
    }

    #[test]
    fn negative_pigpiod_result_surfaces_pi_error_name() {
        let stream = FakeStream::with_results(&[-36]);
        let mut client = PigpioClient::new(stream);

        let err = client
            .wave_add_generic(&[GpioPulse {
                gpio_on: 1,
                gpio_off: 0,
                us_delay: 640,
            }])
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("WVAG"), "{message}");
        assert!(message.contains("PI_TOO_MANY_PULSES"), "{message}");
        assert!(message.contains("-36"), "{message}");
    }

    #[test]
    fn unknown_negative_result_falls_back_to_pi_unknown() {
        let stream = FakeStream::with_results(&[-9999]);
        let mut client = PigpioClient::new(stream);

        let err = client.wave_clear().unwrap_err();
        assert!(err.to_string().contains("PI_UNKNOWN"), "{err}");
    }

    #[test]
    fn detects_reply_command_mismatch() {
        let stream = FakeStream::with_mismatch(0);
        let mut client = PigpioClient::new(stream);

        let err = client.wave_clear().unwrap_err();
        let message = err.to_string();
        assert!(message.contains("did not match request"), "{message}");
        assert!(message.contains("WVCLR"), "{message}");
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }
}
