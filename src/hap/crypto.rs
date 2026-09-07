//! Key derivation and nonce construction shared by the HAP pairing and session
//! layers.
//!
//! Both live here rather than in the modules that use them because getting
//! either wrong fails silently: a mismatched nonce or info string produces a
//! decrypt error at the far end with nothing to point at the cause.

use anyhow::{anyhow, Result};
use chacha20poly1305::Nonce;
use hkdf::Hkdf;
use sha2::Sha512;

/// HKDF-SHA512 down to a 32-byte key — the only KDF HAP uses.
pub fn hkdf_sha512(ikm: &[u8], salt: &[u8], info: &[u8]) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    Hkdf::<Sha512>::new(Some(salt), ikm)
        .expand(info, &mut out)
        .map_err(|e| anyhow!("HKDF: {e}"))?;
    Ok(out)
}

/// Nonce for one pairing message, e.g. `b"PS-Msg05"`.
///
/// HAP pairing nonces are four zero bytes followed by the 8-byte ASCII message
/// label. Session frames use the same 12-byte shape with a counter instead —
/// see `session::nonce_for`.
pub fn pairing_nonce(label: &[u8; 8]) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[4..].copy_from_slice(label);
    Nonce::from(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pairing and session layers used to build these inline. Pin the bytes
    /// against the original constructions so the shared helpers cannot drift.
    #[test]
    fn pairing_nonce_is_four_zeros_then_the_label() {
        let mut expected = [0u8; 12];
        expected[4..].copy_from_slice(b"PS-Msg05");

        assert_eq!(pairing_nonce(b"PS-Msg05").as_slice(), &expected);
        for label in [b"PS-Msg06", b"PV-Msg02", b"PV-Msg03"] {
            let nonce = pairing_nonce(label);
            assert_eq!(&nonce[..4], &[0, 0, 0, 0]);
            assert_eq!(&nonce[4..], label);
        }
    }

    #[test]
    fn hkdf_sha512_matches_a_direct_expand() {
        let mut expected = [0u8; 32];
        Hkdf::<Sha512>::new(Some(b"Pair-Setup-Encrypt-Salt"), b"srp-key")
            .expand(b"Pair-Setup-Encrypt-Info", &mut expected)
            .unwrap();

        let actual = hkdf_sha512(
            b"srp-key",
            b"Pair-Setup-Encrypt-Salt",
            b"Pair-Setup-Encrypt-Info",
        )
        .unwrap();

        assert_eq!(actual, expected);
    }

    /// `SessionKeys::derive` used to extract once and expand twice; it now calls
    /// [`hkdf_sha512`] twice. Extract is deterministic, so both keys must match.
    #[test]
    fn repeated_extract_matches_one_extract_with_two_expands() {
        let hkdf = Hkdf::<Sha512>::new(Some(b"Control-Salt"), b"shared-secret");
        let mut read = [0u8; 32];
        let mut write = [0u8; 32];
        hkdf.expand(b"Control-Write-Encryption-Key", &mut read)
            .unwrap();
        hkdf.expand(b"Control-Read-Encryption-Key", &mut write)
            .unwrap();

        assert_eq!(
            hkdf_sha512(
                b"shared-secret",
                b"Control-Salt",
                b"Control-Write-Encryption-Key"
            )
            .unwrap(),
            read
        );
        assert_eq!(
            hkdf_sha512(
                b"shared-secret",
                b"Control-Salt",
                b"Control-Read-Encryption-Key"
            )
            .unwrap(),
            write
        );
        assert_ne!(read, write);
    }
}
