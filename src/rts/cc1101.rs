use anyhow::Result;
#[cfg(not(target_os = "linux"))]
use std::fs::File;
use std::io::Write;

const WRITE_BURST: u8 = 0x40;

const REG_IOCFG0: u8 = 0x02;
const REG_PKTCTRL0: u8 = 0x08;
const REG_FREQ2: u8 = 0x0D;
const REG_MDMCFG4: u8 = 0x10;
const REG_MDMCFG3: u8 = 0x11;
const REG_MDMCFG2: u8 = 0x12;
const REG_MCSM0: u8 = 0x18;
const REG_FREND0: u8 = 0x22;
const REG_FSCAL3: u8 = 0x23;
const REG_FSCAL2: u8 = 0x24;
const REG_FSCAL1: u8 = 0x25;
const REG_FSCAL0: u8 = 0x26;
const REG_TEST2: u8 = 0x2C;
const REG_TEST1: u8 = 0x2D;
const REG_TEST0: u8 = 0x2E;
const REG_PATABLE: u8 = 0x3E;

const STROBE_SRES: u8 = 0x30;
const STROBE_STX: u8 = 0x35;
const STROBE_SIDLE: u8 = 0x36;

const FREQ_433_42_26MHZ: [u8; 3] = [0x10, 0xAB, 0x85];

pub trait SpiDevice {
    fn write(&mut self, bytes: &[u8]) -> Result<()>;
}

#[cfg(not(target_os = "linux"))]
impl SpiDevice for File {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        Write::write_all(self, bytes)?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl SpiDevice for spidev::Spidev {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        Write::write_all(self, bytes)?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct Cc1101<S> {
    spi: S,
}

impl<S: SpiDevice> Cc1101<S> {
    pub fn new(spi: S) -> Self {
        Self { spi }
    }

    pub fn into_inner(self) -> S {
        self.spi
    }

    pub fn configure_ook_433_42(&mut self) -> Result<()> {
        self.strobe(STROBE_SRES)?;
        self.write_register(REG_IOCFG0, 0x0D)?;
        self.write_register(REG_PKTCTRL0, 0x30)?;
        self.write_burst(REG_FREQ2, &FREQ_433_42_26MHZ)?;

        // ~2.4 kBaud raw async sampling. The app generates Somfy Manchester;
        // CC1101 packet handling, sync, CRC, and radio-side Manchester stay off.
        self.write_register(REG_MDMCFG4, 0xF5)?;
        self.write_register(REG_MDMCFG3, 0x83)?;
        self.write_register(REG_MDMCFG2, 0x30)?;

        self.write_register(REG_MCSM0, 0x18)?;
        self.write_register(REG_FREND0, 0x11)?;
        self.write_register(REG_FSCAL3, 0xE9)?;
        self.write_register(REG_FSCAL2, 0x2A)?;
        self.write_register(REG_FSCAL1, 0x00)?;
        self.write_register(REG_FSCAL0, 0x1F)?;
        self.write_register(REG_TEST2, 0x81)?;
        self.write_register(REG_TEST1, 0x35)?;
        self.write_register(REG_TEST0, 0x09)?;
        self.write_burst(REG_PATABLE, &[0xC0])?;
        self.idle()
    }

    pub fn tx(&mut self) -> Result<()> {
        self.strobe(STROBE_STX)
    }

    pub fn idle(&mut self) -> Result<()> {
        self.strobe(STROBE_SIDLE)
    }

    fn write_register(&mut self, address: u8, value: u8) -> Result<()> {
        self.spi.write(&[address, value])
    }

    fn write_burst(&mut self, address: u8, values: &[u8]) -> Result<()> {
        let mut bytes = Vec::with_capacity(values.len() + 1);
        bytes.push(address | WRITE_BURST);
        bytes.extend_from_slice(values);
        self.spi.write(&bytes)
    }

    fn strobe(&mut self, command: u8) -> Result<()> {
        self.spi.write(&[command])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct FakeSpi {
        writes: Vec<Vec<u8>>,
    }

    impl SpiDevice for FakeSpi {
        fn write(&mut self, bytes: &[u8]) -> Result<()> {
            self.writes.push(bytes.to_vec());
            Ok(())
        }
    }

    #[test]
    fn configures_async_ook_for_433_42_mhz() {
        let mut cc1101 = Cc1101::new(FakeSpi::default());

        cc1101.configure_ook_433_42().unwrap();

        let writes = cc1101.into_inner().writes;
        assert_eq!(writes[0], vec![STROBE_SRES]);
        assert!(writes.contains(&vec![REG_PKTCTRL0, 0x30]));
        assert!(writes.contains(&vec![REG_MDMCFG2, 0x30]));
        assert!(writes.contains(&vec![
            REG_FREQ2 | WRITE_BURST,
            FREQ_433_42_26MHZ[0],
            FREQ_433_42_26MHZ[1],
            FREQ_433_42_26MHZ[2],
        ]));
        assert_eq!(writes.last().unwrap(), &vec![STROBE_SIDLE]);
    }

    #[test]
    fn exposes_tx_and_idle_strobes() {
        let mut cc1101 = Cc1101::new(FakeSpi::default());

        cc1101.tx().unwrap();
        cc1101.idle().unwrap();

        assert_eq!(
            cc1101.into_inner().writes,
            vec![vec![STROBE_STX], vec![STROBE_SIDLE]]
        );
    }
}
