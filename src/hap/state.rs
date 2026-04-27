use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::hap::runtime::HapStore;

pub const HAP_PORT: u16 = 5010;
pub const MODEL: &str = "Somfy Telis 4";
pub const MANUFACTURER: &str = "Somfy";
pub const HAP_CATEGORY: &str = "14";
pub const STATE_FILE: &str = "hap.json";
pub const SYSTEM_STATE_DIR: &str = "/var/lib/somfy";

/// Persistent HAP accessory identity.
///
/// Generated on first boot and reused forever — re-keying would force every
/// paired controller to re-pair from scratch.
#[derive(Debug, Serialize, Deserialize)]
pub struct HapState {
    pub device_id: String,
    pub setup_code: String,
    pub setup_id: String,
    pub config_number: u32,
    pub state_number: u32,
    #[serde(with = "hex_array_32")]
    pub ltsk: [u8; 32],
    pub paired_controllers: Vec<PairedController>,
    /// Cumulative count of failed pair-setup M3 proofs since the last
    /// successful pairing. HAP §5.6.5 requires the accessory to refuse all
    /// subsequent attempts after 100 failures until factory reset.
    #[serde(default)]
    pub setup_failed_attempts: u32,
}

/// HAP §5.6.5: cap on consecutive failed pair-setup proofs.
pub const MAX_SETUP_FAILED_ATTEMPTS: u32 = 100;

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
        let mut rng = OsRng;
        let signing = SigningKey::generate(&mut rng);

        let mut id_bytes = [0u8; 6];
        rng.fill(&mut id_bytes);
        let device_id = id_bytes
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(":");

        let setup_code = srp_setup_code(rng.gen_range(0..100_000_000u32));
        let setup_id = generate_setup_id();

        Self {
            device_id,
            setup_code,
            setup_id,
            config_number: 1,
            state_number: 1,
            ltsk: signing.to_bytes(),
            paired_controllers: Vec::new(),
            setup_failed_attempts: 0,
        }
    }
}

/// 4-character HomeKit setup ID. Apple's spec recommends a confusion-free
/// alphabet — drop ambiguous glyphs (0/O, 1/I, etc.) to keep the printed code
/// scannable in case the operator falls back to typing it.
fn generate_setup_id() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = OsRng;
    (0..4)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
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

pub fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("STATE_DIRECTORY") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("SOMFY_STATE_DIR") {
        return PathBuf::from(dir);
    }
    default_state_dir()
}

#[cfg(debug_assertions)]
fn default_state_dir() -> PathBuf {
    PathBuf::from("./hap-state")
}

#[cfg(not(debug_assertions))]
fn default_state_dir() -> PathBuf {
    PathBuf::from(SYSTEM_STATE_DIR)
}

pub fn load_or_init() -> Result<HapState> {
    FileHapStore::current().load_or_init()
}

pub fn save_current(state: &HapState) -> Result<()> {
    FileHapStore::current().save_state(state)
}

#[derive(Clone, Debug)]
pub struct FileHapStore {
    dir: PathBuf,
}

impl FileHapStore {
    pub fn current() -> Self {
        Self { dir: state_dir() }
    }

    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn state_path(&self) -> PathBuf {
        self.dir.join(STATE_FILE)
    }

    pub fn load_or_init(&self) -> Result<HapState> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating state directory {}", self.dir.display()))?;

        match self.load_state()? {
            Some(state) => Ok(state),
            None => {
                let state = HapState::generate();
                self.save_state(&state)?;
                tracing::info!("initialized HAP state at {}", self.state_path().display());
                Ok(state)
            }
        }
    }

    pub fn reset(&self) -> Result<HapState> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating state directory {}", self.dir.display()))?;
        let state = HapState::generate();
        self.save_state(&state)?;
        Ok(state)
    }
}

impl HapStore for FileHapStore {
    fn load_state(&self) -> Result<Option<HapState>> {
        let path = self.state_path();
        match fs::read_to_string(&path) {
            Ok(text) => {
                let state: HapState = serde_json::from_str(&text)
                    .with_context(|| format!("parsing {}", path.display()))?;
                Ok(Some(state))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    fn save_state(&self, state: &HapState) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        save(&self.state_path(), state)
    }
}

pub fn reset_current() -> Result<HapState> {
    FileHapStore::current().reset()
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
        assert_eq!(s.setup_id.len(), 4);
        assert!(s
            .setup_id
            .chars()
            .all(|c| "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(c)));
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
        assert_eq!(loaded.setup_id, original.setup_id);
        assert_eq!(loaded.ltsk, original.ltsk);
    }

    #[test]
    fn state_without_setup_id_is_invalid() {
        let json = r#"{
            "device_id": "AB:CD:EF:12:34:56",
            "setup_code": "101-48-005",
            "config_number": 1,
            "state_number": 1,
            "ltsk": "0000000000000000000000000000000000000000000000000000000000000000",
            "paired_controllers": []
        }"#;
        assert!(serde_json::from_str::<HapState>(json).is_err());
    }
}
