//! Pair-Setup state machine (HAP §5.6). SRP-6a/SHA-512 over RFC 5054 group
//! 3072 (see `srp.rs`), username "Pair-Setup", password = the setup code
//! including dashes.

use anyhow::{anyhow, Result};
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use rand::{rngs::OsRng, RngCore};
use sha2::Sha512;

use crate::hap::srp;
use crate::hap::state::{HapState, PairedController};
use crate::hap::tlv::{error_response, HapError, ParsedTlv, Tag as TlvTag, Tlv};

#[derive(Default)]
pub enum PairSetupState {
    #[default]
    Initial,
    AwaitingProof {
        setup: srp::ServerSetup,
    },
    AwaitingExchange {
        srp_key: Vec<u8>,
    },
    Done,
}

#[derive(Default)]
pub struct PairSetupSession {
    pub state: PairSetupState,
}

impl PairSetupSession {
    pub fn new() -> Self {
        Self {
            state: PairSetupState::Initial,
        }
    }

    pub fn handle(&mut self, body: &[u8], state: &mut HapState) -> Vec<u8> {
        let parsed = match ParsedTlv::parse(body) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("pair-setup: malformed TLV: {e}");
                return error_response(2, HapError::Unknown);
            }
        };

        let m_state = parsed.get_u8(TlvTag::State).unwrap_or(0);
        let result = match m_state {
            1 => self.handle_m1(state),
            3 => self.handle_m3(&parsed),
            5 => self.handle_m5(&parsed, state),
            other => {
                tracing::warn!("pair-setup: unexpected state byte {other}");
                Err((other.saturating_add(1), HapError::Unknown))
            }
        };

        result.unwrap_or_else(|(state_byte, err)| {
            self.state = PairSetupState::Initial;
            error_response(state_byte, err)
        })
    }

    fn handle_m1(&mut self, state: &HapState) -> Result<Vec<u8>, (u8, HapError)> {
        if state.is_paired() {
            return Err((2, HapError::Unavailable));
        }

        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let mut b_priv = [0u8; 32];
        OsRng.fill_bytes(&mut b_priv);

        let setup = srp::server_setup(state.setup_code.as_bytes(), salt, b_priv);
        let b_pub = setup.b_pub.clone();

        self.state = PairSetupState::AwaitingProof { setup };

        Ok(Tlv::new()
            .put_u8(TlvTag::State, 2)
            .put(TlvTag::PublicKey, b_pub)
            .put(TlvTag::Salt, salt.to_vec())
            .encode())
    }

    fn handle_m3(&mut self, parsed: &ParsedTlv) -> Result<Vec<u8>, (u8, HapError)> {
        let setup = match std::mem::take(&mut self.state) {
            PairSetupState::AwaitingProof { setup } => setup,
            other => {
                self.state = other;
                return Err((4, HapError::Unknown));
            }
        };

        let a_pub = parsed
            .get(TlvTag::PublicKey)
            .ok_or((4, HapError::Authentication))?;
        let m1 = parsed
            .get(TlvTag::Proof)
            .ok_or((4, HapError::Authentication))?;

        let verifier = srp::server_verify(&setup, a_pub).map_err(|e| {
            tracing::warn!("pair-setup M3 verify setup failed: {e}");
            (4, HapError::Authentication)
        })?;
        if !srp::ct_eq(&verifier.m1_expected, m1) {
            tracing::warn!("pair-setup M3 client proof mismatch");
            return Err((4, HapError::Authentication));
        }

        self.state = PairSetupState::AwaitingExchange {
            srp_key: verifier.k.clone(),
        };

        Ok(Tlv::new()
            .put_u8(TlvTag::State, 4)
            .put(TlvTag::Proof, verifier.m2)
            .encode())
    }

    fn handle_m5(
        &mut self,
        parsed: &ParsedTlv,
        state: &mut HapState,
    ) -> Result<Vec<u8>, (u8, HapError)> {
        let srp_key = match std::mem::take(&mut self.state) {
            PairSetupState::AwaitingExchange { srp_key } => srp_key,
            other => {
                self.state = other;
                return Err((6, HapError::Unknown));
            }
        };

        let encrypted = parsed
            .get(TlvTag::EncryptedData)
            .ok_or((6, HapError::Authentication))?;
        if encrypted.len() < 16 {
            return Err((6, HapError::Authentication));
        }

        let session_key = derive_session_key(
            &srp_key,
            b"Pair-Setup-Encrypt-Salt",
            b"Pair-Setup-Encrypt-Info",
        )
        .map_err(|_| (6, HapError::Unknown))?;

        let mut plaintext = encrypted[..encrypted.len() - 16].to_vec();
        let tag = Tag::clone_from_slice(&encrypted[encrypted.len() - 16..]);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&session_key));
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[4..].copy_from_slice(b"PS-Msg05");
        cipher
            .decrypt_in_place_detached(Nonce::from_slice(&nonce_bytes), &[], &mut plaintext, &tag)
            .map_err(|_| {
                tracing::warn!("pair-setup M5 decrypt failed");
                (6, HapError::Authentication)
            })?;

        let sub = ParsedTlv::parse(&plaintext).map_err(|_| (6, HapError::Authentication))?;
        let ios_pairing_id = sub
            .get(TlvTag::Identifier)
            .ok_or((6, HapError::Authentication))?;
        let ios_ltpk_bytes = sub
            .get(TlvTag::PublicKey)
            .ok_or((6, HapError::Authentication))?;
        let ios_signature = sub
            .get(TlvTag::Signature)
            .ok_or((6, HapError::Authentication))?;

        let ios_device_x = derive_session_key(
            &srp_key,
            b"Pair-Setup-Controller-Sign-Salt",
            b"Pair-Setup-Controller-Sign-Info",
        )
        .map_err(|_| (6, HapError::Unknown))?;
        let mut ios_device_info = Vec::with_capacity(32 + ios_pairing_id.len() + 32);
        ios_device_info.extend_from_slice(&ios_device_x);
        ios_device_info.extend_from_slice(ios_pairing_id);
        ios_device_info.extend_from_slice(ios_ltpk_bytes);

        let ios_ltpk_array: [u8; 32] = ios_ltpk_bytes
            .try_into()
            .map_err(|_| (6, HapError::Authentication))?;
        let ios_ltpk =
            VerifyingKey::from_bytes(&ios_ltpk_array).map_err(|_| (6, HapError::Authentication))?;
        let ios_sig_array: [u8; 64] = ios_signature
            .try_into()
            .map_err(|_| (6, HapError::Authentication))?;
        let ios_sig = ed25519_dalek::Signature::from_bytes(&ios_sig_array);
        ios_ltpk.verify(&ios_device_info, &ios_sig).map_err(|_| {
            tracing::warn!("pair-setup M5 iOS signature verify failed");
            (6, HapError::Authentication)
        })?;

        let accessory_x = derive_session_key(
            &srp_key,
            b"Pair-Setup-Accessory-Sign-Salt",
            b"Pair-Setup-Accessory-Sign-Info",
        )
        .map_err(|_| (6, HapError::Unknown))?;
        let accessory_signing: SigningKey = state.signing_key();
        let accessory_ltpk = accessory_signing.verifying_key().to_bytes();
        let pairing_id_bytes = state.device_id.as_bytes();
        let mut accessory_info =
            Vec::with_capacity(32 + pairing_id_bytes.len() + accessory_ltpk.len());
        accessory_info.extend_from_slice(&accessory_x);
        accessory_info.extend_from_slice(pairing_id_bytes);
        accessory_info.extend_from_slice(&accessory_ltpk);
        let accessory_sig = accessory_signing.sign(&accessory_info).to_bytes();

        let sub_response = Tlv::new()
            .put(TlvTag::Identifier, pairing_id_bytes.to_vec())
            .put(TlvTag::PublicKey, accessory_ltpk.to_vec())
            .put(TlvTag::Signature, accessory_sig.to_vec())
            .encode();

        let mut response_buf = sub_response.clone();
        let mut response_nonce = [0u8; 12];
        response_nonce[4..].copy_from_slice(b"PS-Msg06");
        let response_tag = cipher
            .encrypt_in_place_detached(Nonce::from_slice(&response_nonce), &[], &mut response_buf)
            .map_err(|_| (6, HapError::Unknown))?;
        response_buf.extend_from_slice(&response_tag);

        state.add_pairing(PairedController {
            identifier: String::from_utf8_lossy(ios_pairing_id).to_string(),
            public_key: ios_ltpk_bytes.to_vec(),
            admin: true,
        });
        if let Err(e) = crate::hap::state::save_current(state) {
            tracing::error!("failed to persist pairing: {e}");
            return Err((6, HapError::Unknown));
        }

        self.state = PairSetupState::Done;
        tracing::info!(
            "pair-setup complete: controller {} added",
            String::from_utf8_lossy(ios_pairing_id)
        );

        Ok(Tlv::new()
            .put_u8(TlvTag::State, 6)
            .put(TlvTag::EncryptedData, response_buf)
            .encode())
    }
}

fn derive_session_key(srp_key: &[u8], salt: &[u8], info: &[u8]) -> Result<[u8; 32]> {
    let hkdf = Hkdf::<Sha512>::new(Some(salt), srp_key);
    let mut out = [0u8; 32];
    hkdf.expand(info, &mut out)
        .map_err(|e| anyhow!("HKDF: {e}"))?;
    Ok(out)
}
