use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use rand::{rngs::SysRng, TryRng};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const HAP_PORT: u16 = 5010;
pub const MODEL: &str = "Somfy Telis 4";
pub const MANUFACTURER: &str = "Somfy";
pub const HAP_CATEGORY: &str = "14";
pub const STATE_FILE: &str = "hap.json";

/// Persistent HAP accessory identity.
///
/// Generated on first boot and reused forever — re-keying would force every
/// paired controller to re-pair from scratch.
#[derive(Debug, Serialize, Deserialize)]
pub struct HapState {
    pub device_id: String,
    pub setup_code: String,
    pub config_number: u32,
    pub state_number: u32,
    #[serde(with = "hex_array_32")]
    pub ltsk: [u8; 32],
    pub paired_controllers: Vec<PairedController>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PairedController {
    pub identifier: String,
    #[serde(with = "hex_vec")]
    pub public_key: Vec<u8>,
    pub admin: bool,
}

impl HapState {
    pub fn signing_key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.ltsk)
    }

    pub fn is_paired(&self) -> bool {
        !self.paired_controllers.is_empty()
    }

    pub fn find_paired(&self, identifier: &str) -> Option<&PairedController> {
        self.paired_controllers
            .iter()
            .find(|c| c.identifier == identifier)
    }

    pub fn add_pairing(&mut self, controller: PairedController) {
        if let Some(existing) = self
            .paired_controllers
            .iter_mut()
            .find(|c| c.identifier == controller.identifier)
        {
            existing.public_key = controller.public_key;
            existing.admin = controller.admin;
        } else {
            self.paired_controllers.push(controller);
        }
    }

    pub fn remove_pairing(&mut self, identifier: &str) {
        self.paired_controllers
            .retain(|c| c.identifier != identifier);
    }

    pub fn status_flag(&self) -> &'static str {
        if self.is_paired() {
            "0"
        } else {
            "1"
        }
    }

    fn generate() -> Self {
        let mut ltsk = [0u8; 32];
        SysRng
            .try_fill_bytes(&mut ltsk)
            .expect("system RNG failed while generating HAP identity");

        let mut id_bytes = [0u8; 6];
        SysRng
            .try_fill_bytes(&mut id_bytes)
            .expect("system RNG failed while generating HAP device id");
        let device_id = id_bytes
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(":");

        let setup_code = srp_setup_code(random_setup_code());

        Self {
            device_id,
            setup_code,
            config_number: 1,
            state_number: 1,
            ltsk: SigningKey::from_bytes(&ltsk).to_bytes(),
            paired_controllers: Vec::new(),
        }
    }
}

pub fn display_setup_code(setup_code: &str) -> String {
    let digits: String = setup_code.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != 8 {
        return setup_code.to_string();
    }
    format!("{}-{}", &digits[..4], &digits[4..])
}

fn srp_setup_code(code_num: u32) -> String {
    format!(
        "{:03}-{:02}-{:03}",
        code_num / 100_000,
        (code_num / 1_000) % 100,
        code_num % 1_000
    )
}

fn random_setup_code() -> u32 {
    const UPPER: u64 = 100_000_000;
    const ZONE: u64 = (u32::MAX as u64 + 1) / UPPER * UPPER;

    loop {
        let value = SysRng
            .try_next_u32()
            .expect("system RNG failed while generating HAP setup code") as u64;
        if value < ZONE {
            return (value % UPPER) as u32;
        }
    }
}

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("STATE_DIRECTORY") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("SOMFY_STATE_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from("./hap-state")
}

pub fn load_or_init() -> Result<HapState> {
    let dir = state_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating state directory {}", dir.display()))?;

    let path = dir.join(STATE_FILE);
    match fs::read_to_string(&path) {
        Ok(text) => {
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let state = HapState::generate();
            save(&path, &state)?;
            tracing::info!("initialized HAP state at {}", path.display());
            Ok(state)
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save_current(state: &HapState) -> Result<()> {
    let dir = state_dir();
    fs::create_dir_all(&dir)?;
    save(&dir.join(STATE_FILE), state)
}

pub fn save(path: &Path, state: &HapState) -> Result<()> {
    let json = serde_json::to_vec_pretty(state)?;
    atomic_save_bytes(path, &json, true)
}

pub(crate) fn atomic_save_bytes(path: &Path, bytes: &[u8], durable: bool) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let filename = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid state path"))?;
    let tmp = parent.join(format!(".{}.tmp", filename.to_string_lossy()));
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        if durable {
            f.sync_all()?;
        }
    }
    fs::rename(&tmp, path)?;
    if durable {
        sync_parent_dir(parent)?;
    }
    Ok(())
}

fn sync_parent_dir(parent: &Path) -> Result<()> {
    match fs::File::open(parent).and_then(|dir| dir.sync_all()) {
        Ok(()) => Ok(()),
        Err(e) if directory_sync_unsupported(&e) => Ok(()),
        Err(e) => Err(e).with_context(|| format!("syncing state directory {}", parent.display())),
    }
}

fn directory_sync_unsupported(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
    )
}

mod hex_array_32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected 32-byte hex string"))
    }
}

mod hex_vec {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_state_is_well_formed() {
        let s = HapState::generate();
        assert_eq!(s.device_id.len(), 17);
        assert_eq!(s.device_id.matches(':').count(), 5);
        assert_eq!(s.setup_code.len(), 10);
        assert_eq!(s.setup_code.matches('-').count(), 2);
        assert_eq!(s.config_number, 1);
        assert!(!s.is_paired());
        assert_eq!(s.status_flag(), "1");
    }

    #[test]
    fn setup_code_formats_are_distinct_for_srp_and_display() {
        assert_eq!(srp_setup_code(10148005), "101-48-005");
        assert_eq!(display_setup_code("101-48-005"), "1014-8005");
    }

    #[test]
    fn display_setup_code_leaves_unexpected_format_unchanged() {
        assert_eq!(display_setup_code("not-a-code"), "not-a-code");
    }

    #[test]
    fn round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let original = HapState::generate();
        save(&path, &original).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let loaded: HapState = serde_json::from_str(&text).unwrap();
        assert_eq!(loaded.device_id, original.device_id);
        assert_eq!(loaded.setup_code, original.setup_code);
        assert_eq!(loaded.ltsk, original.ltsk);
    }
}
