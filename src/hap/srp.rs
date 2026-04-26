//! HAP-flavored SRP-6a/SHA-512 over RFC 5054 group 3072.
//!
//! HAP §5.6.5 differs from the bare SRP-6a typical in two ways: the M1
//! verification hash is the RFC 5054 form `H(H(N) XOR H(g) | H(I) | s | A | B
//! | K)`, and K is `H(S)` rather than S itself. The widely-used `srp` crate
//! only ships the simplified form, hence this module.

use anyhow::{bail, Result};
use num_bigint::BigUint;
use sha2::{Digest, Sha512};

// RFC 5054 Appendix A, 3072-bit group. N hex constant copied directly so the
// constants are auditable in-tree without depending on the srp crate.
const N_HEX: &str = "\
FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74\
020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F1437\
4FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED\
EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF05\
98DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB\
9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B\
E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF695581718\
3995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33\
A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7\
ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864\
D87602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E2\
08E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF";
const G_VAL: u32 = 5;
const USERNAME: &[u8] = b"Pair-Setup";

fn group_n() -> BigUint {
    BigUint::parse_bytes(N_HEX.as_bytes(), 16).expect("valid N constant")
}

fn group_g() -> BigUint {
    BigUint::from(G_VAL)
}

fn n_byte_len() -> usize {
    (N_HEX.len()) / 2
}

fn pad_to_n(val: &BigUint) -> Vec<u8> {
    let n_len = n_byte_len();
    let bytes = val.to_bytes_be();
    if bytes.len() < n_len {
        let mut padded = vec![0u8; n_len - bytes.len()];
        padded.extend_from_slice(&bytes);
        padded
    } else {
        bytes
    }
}

fn sha512(data: &[u8]) -> Vec<u8> {
    Sha512::digest(data).to_vec()
}

pub struct ServerSetup {
    pub salt: [u8; 16],
    pub b_priv: [u8; 32],
    pub b_pub: Vec<u8>, // big-endian, padded to N length
    v: BigUint,
}

pub struct ServerVerifier {
    pub m1_expected: Vec<u8>,
    pub m2: Vec<u8>,
    pub k: Vec<u8>, // H(S), 64 bytes
}

/// M1: generate verifier v from password, compute B from random b.
pub fn server_setup(password: &[u8], salt: [u8; 16], b_priv: [u8; 32]) -> ServerSetup {
    let n = group_n();
    let g = group_g();

    let inner = {
        let mut h = Sha512::new();
        h.update(USERNAME);
        h.update(b":");
        h.update(password);
        h.finalize()
    };
    let x_hash = {
        let mut h = Sha512::new();
        h.update(salt);
        h.update(inner);
        h.finalize()
    };
    let x = BigUint::from_bytes_be(&x_hash);

    let v = g.modpow(&x, &n);

    // k = H(N | PAD(g))
    let k_hash = {
        let mut h = Sha512::new();
        h.update(pad_to_n(&n));
        h.update(pad_to_n(&g));
        h.finalize()
    };
    let k = BigUint::from_bytes_be(&k_hash);

    // B = (k*v + g^b) mod N
    let b_int = BigUint::from_bytes_be(&b_priv);
    let b_pub_int = (&k * &v + g.modpow(&b_int, &n)) % &n;

    ServerSetup {
        salt,
        b_priv,
        b_pub: pad_to_n(&b_pub_int),
        v,
    }
}

/// M3: derive shared secret S, K, expected M1, and accessory M2 from the
/// controller's A and the original verifier.
pub fn server_verify(setup: &ServerSetup, a_pub_bytes: &[u8]) -> Result<ServerVerifier> {
    let n = group_n();
    let g = group_g();
    let a_pub = BigUint::from_bytes_be(a_pub_bytes);

    if &a_pub % &n == BigUint::from(0u32) {
        bail!("invalid A");
    }

    let a_pad = pad_to_n(&a_pub);
    let b_pad = setup.b_pub.clone();

    let u_hash = {
        let mut h = Sha512::new();
        h.update(&a_pad);
        h.update(&b_pad);
        h.finalize()
    };
    let u = BigUint::from_bytes_be(&u_hash);

    let b_int = BigUint::from_bytes_be(&setup.b_priv);
    let s_base = (&a_pub * setup.v.modpow(&u, &n)) % &n;
    let s = s_base.modpow(&b_int, &n);
    let s_pad = pad_to_n(&s);
    let k = sha512(&s_pad);

    let h_n = sha512(&n.to_bytes_be());
    let h_g = sha512(&g.to_bytes_be());
    let h_xor: Vec<u8> = h_n
        .iter()
        .zip(h_g.iter())
        .map(|(a, b)| a ^ b)
        .collect();
    let h_i = sha512(USERNAME);

    let m1_expected = {
        let mut h = Sha512::new();
        h.update(&h_xor);
        h.update(&h_i);
        h.update(setup.salt);
        h.update(&a_pad);
        h.update(&b_pad);
        h.update(&k);
        h.finalize().to_vec()
    };

    let m2 = {
        let mut h = Sha512::new();
        h.update(&a_pad);
        h.update(&m1_expected);
        h.update(&k);
        h.finalize().to_vec()
    };

    Ok(ServerVerifier { m1_expected, m2, k })
}

pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_constants_round_trip() {
        let n = group_n();
        assert_eq!(n.bits(), 3072);
        assert_eq!(n_byte_len(), 384);
    }

    #[test]
    fn pad_grows_short_values() {
        let v = BigUint::from(1u32);
        let padded = pad_to_n(&v);
        assert_eq!(padded.len(), 384);
        assert_eq!(padded[383], 1);
        assert_eq!(padded[0], 0);
    }
}
