use anyhow::{bail, Result};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};

use crate::rts::waveform::GpioPulse;

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
        Ok(Self::new(TcpStream::connect(addr)?))
    }
}

impl<S: Read + Write> PigpioClient<S> {
    pub fn new(stream: S) -> Self {
        Self { stream }
    }

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

        let mut response = [0u8; 16];
        self.stream.read_exact(&mut response)?;
        let result = i32::from_le_bytes(response[12..16].try_into().expect("fixed slice length"));
        if result < 0 {
            bail!("pigpiod command {command} failed with status {result}");
        }
        Ok(result)
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
        responses: VecDeque<[u8; 16]>,
    }

    impl FakeStream {
        fn with_responses(results: &[i32]) -> Self {
            Self {
                written: Vec::new(),
                responses: results.iter().map(|result| response(*result)).collect(),
            }
        }
    }

    impl Read for FakeStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(response) = self.responses.pop_front() else {
                return Ok(0);
            };
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
        let stream = FakeStream::with_responses(&[0, 0, 0, 0, 3, 0, 1, 0, 0]);
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
        let stream = FakeStream::with_responses(&[0]);
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
    fn maps_negative_pigpio_result_to_error() {
        let stream = FakeStream::with_responses(&[-2]);
        let mut client = PigpioClient::new(stream);

        let err = client.wave_clear().unwrap_err();
        assert!(err.to_string().contains("failed with status -2"));
    }

    fn response(result: i32) -> [u8; 16] {
        let mut response = [0u8; 16];
        response[12..16].copy_from_slice(&result.to_le_bytes());
        response
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }
}
