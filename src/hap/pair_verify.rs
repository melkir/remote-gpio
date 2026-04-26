//! Pair-Verify state machine (HAP §5.7). X25519 ECDH establishes a shared
//! secret; both sides prove possession of long-term keys via Ed25519
//! signatures. Output is a 32-byte shared secret used to derive the
//! session keys in `session::SessionKeys`.

use anyhow::Result;
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};
use ed25519_dalek::{Signer, Verifier, VerifyingKey};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use sha2::Sha512;
use x25519_dalek::{EphemeralSecret, PublicKey as XPub};

use crate::hap::state::HapState;
use crate::hap::tlv::{error_response, HapError, ParsedTlv, Tag as TlvTag, Tlv};

#[derive(Default)]
pub enum PairVerifyState {
    #[default]
    Initial,
    AwaitingFinish {
        shared_secret: [u8; 32],
        accessory_pub: [u8; 32],
        ios_pub: [u8; 32],
        session_key: [u8; 32],
    },
    Done {
        shared_secret: [u8; 32],
    },
}

#[derive(Default)]
pub struct PairVerifySession {
    pub state: PairVerifyState,
}

pub enum HandleOutcome {
    Reply(Vec<u8>),
    /// M4 succeeded — switch the connection to encrypted mode using this
    /// secret. `controller_id` identifies who's on the other end so post-verify
    /// requests (e.g. `POST /pairings`) can authorize against the persisted
    /// admin flag.
    Verified {
        reply: Vec<u8>,
        shared_secret: [u8; 32],
        controller_id: String,
    },
}

impl PairVerifySession {
    pub fn new() -> Self {
        Self {
            state: PairVerifyState::Initial,
        }
    }

    pub fn handle(&mut self, body: &[u8], state: &HapState) -> HandleOutcome {
        let parsed = match ParsedTlv::parse(body) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("pair-verify: malformed TLV: {e}");
                return HandleOutcome::Reply(error_response(2, HapError::Unknown));
            }
        };

        let m_state = parsed.get_u8(TlvTag::State).unwrap_or(0);
        match m_state {
            1 => self.handle_m1(&parsed, state),
            3 => self.handle_m3(&parsed, state),
            other => {
                tracing::warn!("pair-verify: unexpected state {other}");
                HandleOutcome::Reply(error_response(other.saturating_add(1), HapError::Unknown))
            }
        }
    }

    fn handle_m1(&mut self, parsed: &ParsedTlv, state: &HapState) -> HandleOutcome {
        let ios_pub_bytes = match parsed.get(TlvTag::PublicKey) {
            Some(b) if b.len() == 32 => b,
            _ => return HandleOutcome::Reply(error_response(2, HapError::Authentication)),
        };
        let ios_pub_array: [u8; 32] = ios_pub_bytes.try_into().unwrap();

        let accessory_secret = EphemeralSecret::random_from_rng(OsRng);
        let accessory_pub = XPub::from(&accessory_secret);
        let shared = accessory_secret.diffie_hellman(&XPub::from(ios_pub_array));
        let shared_secret: [u8; 32] = shared.to_bytes();

        let session_key = match derive_key(
            &shared_secret,
            b"Pair-Verify-Encrypt-Salt",
            b"Pair-Verify-Encrypt-Info",
        ) {
            Ok(k) => k,
            Err(_) => return HandleOutcome::Reply(error_response(2, HapError::Unknown)),
        };

        let signing = state.signing_key();
        let accessory_pairing_id = state.device_id.as_bytes();
        let mut info_to_sign = Vec::with_capacity(32 + accessory_pairing_id.len() + 32);
        info_to_sign.extend_from_slice(accessory_pub.as_bytes());
        info_to_sign.extend_from_slice(accessory_pairing_id);
        info_to_sign.extend_from_slice(&ios_pub_array);
        let signature = signing.sign(&info_to_sign).to_bytes();

        let sub = Tlv::new()
            .put(TlvTag::Identifier, accessory_pairing_id.to_vec())
            .put(TlvTag::Signature, signature.to_vec())
            .encode();

        let mut buf = sub;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&session_key));
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[4..].copy_from_slice(b"PV-Msg02");
        let tag = match cipher.encrypt_in_place_detached(
            Nonce::from_slice(&nonce_bytes),
            &[],
            &mut buf,
        ) {
            Ok(t) => t,
            Err(_) => return HandleOutcome::Reply(error_response(2, HapError::Unknown)),
        };
        buf.extend_from_slice(&tag);

        let accessory_pub_bytes = *accessory_pub.as_bytes();
        self.state = PairVerifyState::AwaitingFinish {
            shared_secret,
            accessory_pub: accessory_pub_bytes,
            ios_pub: ios_pub_array,
            session_key,
        };

        HandleOutcome::Reply(
            Tlv::new()
                .put_u8(TlvTag::State, 2)
                .put(TlvTag::PublicKey, accessory_pub_bytes.to_vec())
                .put(TlvTag::EncryptedData, buf)
                .encode(),
        )
    }

    fn handle_m3(&mut self, parsed: &ParsedTlv, state: &HapState) -> HandleOutcome {
        let (shared_secret, accessory_pub, ios_pub, session_key) =
            match std::mem::take(&mut self.state) {
                PairVerifyState::AwaitingFinish {
                    shared_secret,
                    accessory_pub,
                    ios_pub,
                    session_key,
                } => (shared_secret, accessory_pub, ios_pub, session_key),
                other => {
                    self.state = other;
                    return HandleOutcome::Reply(error_response(4, HapError::Unknown));
                }
            };

        let encrypted = match parsed.get(TlvTag::EncryptedData) {
            Some(b) if b.len() >= 16 => b,
            _ => return HandleOutcome::Reply(error_response(4, HapError::Authentication)),
        };

        let mut plaintext = encrypted[..encrypted.len() - 16].to_vec();
        let tag = Tag::clone_from_slice(&encrypted[encrypted.len() - 16..]);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&session_key));
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[4..].copy_from_slice(b"PV-Msg03");
        if cipher
            .decrypt_in_place_detached(Nonce::from_slice(&nonce_bytes), &[], &mut plaintext, &tag)
            .is_err()
        {
            tracing::warn!("pair-verify M3 decrypt failed");
            return HandleOutcome::Reply(error_response(4, HapError::Authentication));
        }

        let sub = match ParsedTlv::parse(&plaintext) {
            Ok(p) => p,
            Err(_) => return HandleOutcome::Reply(error_response(4, HapError::Authentication)),
        };
        let ios_pairing_id = match sub.get(TlvTag::Identifier) {
            Some(b) => b,
            None => return HandleOutcome::Reply(error_response(4, HapError::Authentication)),
        };
        let ios_signature = match sub.get(TlvTag::Signature) {
            Some(b) if b.len() == 64 => b,
            _ => return HandleOutcome::Reply(error_response(4, HapError::Authentication)),
        };

        let pairing_id_str = String::from_utf8_lossy(ios_pairing_id).to_string();
        let controller = match state.find_paired(&pairing_id_str) {
            Some(c) => c,
            None => {
                tracing::warn!("pair-verify M3: unknown controller {}", pairing_id_str);
                return HandleOutcome::Reply(error_response(4, HapError::Authentication));
            }
        };
        let ltpk_bytes: [u8; 32] = match controller.public_key.as_slice().try_into() {
            Ok(b) => b,
            Err(_) => return HandleOutcome::Reply(error_response(4, HapError::Authentication)),
        };
        let ltpk = match VerifyingKey::from_bytes(&ltpk_bytes) {
            Ok(k) => k,
            Err(_) => return HandleOutcome::Reply(error_response(4, HapError::Authentication)),
        };

        let mut info = Vec::with_capacity(32 + ios_pairing_id.len() + 32);
        info.extend_from_slice(&ios_pub);
        info.extend_from_slice(ios_pairing_id);
        info.extend_from_slice(&accessory_pub);

        let sig_array: [u8; 64] = ios_signature.try_into().expect("length checked above");
        let sig = ed25519_dalek::Signature::from_bytes(&sig_array);
        if ltpk.verify(&info, &sig).is_err() {
            tracing::warn!("pair-verify M3 iOS signature failed");
            return HandleOutcome::Reply(error_response(4, HapError::Authentication));
        }

        self.state = PairVerifyState::Done { shared_secret };
        tracing::info!("pair-verify complete: session established with {pairing_id_str}");

        HandleOutcome::Verified {
            reply: Tlv::new().put_u8(TlvTag::State, 4).encode(),
            shared_secret,
            controller_id: pairing_id_str,
        }
    }
}

fn derive_key(ikm: &[u8], salt: &[u8], info: &[u8]) -> Result<[u8; 32]> {
    let hkdf = Hkdf::<Sha512>::new(Some(salt), ikm);
    let mut out = [0u8; 32];
    hkdf.expand(info, &mut out)
        .map_err(|e| anyhow::anyhow!("HKDF: {e}"))?;
    Ok(out)
}
