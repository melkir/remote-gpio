//! Post-Pair-Verify session encryption (HAP §6.5).
//!
//! Frames: 2-byte little-endian length || ciphertext(plaintext, len) || 16-byte tag.
//! Nonce: 4 zero bytes || u64 little-endian counter.
//! Counters increment per frame, separately for each direction.

use anyhow::{anyhow, bail, Result};
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha512;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

pub const MAX_FRAME_PLAINTEXT: usize = 1024;
const TAG_LEN: usize = 16;

pub struct SessionKeys {
    pub read: [u8; 32],  // controller -> accessory ("Control-Write-Encryption-Key")
    pub write: [u8; 32], // accessory -> controller ("Control-Read-Encryption-Key")
}

impl SessionKeys {
    pub fn derive(shared_secret: &[u8]) -> Result<Self> {
        let hkdf = Hkdf::<Sha512>::new(Some(b"Control-Salt"), shared_secret);
        let mut read = [0u8; 32];
        let mut write = [0u8; 32];
        hkdf.expand(b"Control-Write-Encryption-Key", &mut read)
            .map_err(|e| anyhow!("HKDF read key: {e}"))?;
        hkdf.expand(b"Control-Read-Encryption-Key", &mut write)
            .map_err(|e| anyhow!("HKDF write key: {e}"))?;
        Ok(Self { read, write })
    }
}

pub struct EncryptedReader {
    inner: OwnedReadHalf,
    cipher: ChaCha20Poly1305,
    counter: u64,
    buf: Vec<u8>,
}

impl EncryptedReader {
    pub fn new(inner: OwnedReadHalf, key: [u8; 32]) -> Self {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        Self { inner, cipher, counter: 0, buf: Vec::new() }
    }

    /// Read at least `min` plaintext bytes into the internal buffer.
    pub async fn fill(&mut self, min: usize) -> Result<()> {
        while self.buf.len() < min {
            self.read_one_frame().await?;
        }
        Ok(())
    }

    pub fn buffered(&self) -> &[u8] {
        &self.buf
    }

    pub fn consume(&mut self, n: usize) {
        self.buf.drain(..n);
    }

    async fn read_one_frame(&mut self) -> Result<()> {
        let mut header = [0u8; 2];
        self.inner.read_exact(&mut header).await?;
        let len = u16::from_le_bytes(header) as usize;
        if len > MAX_FRAME_PLAINTEXT {
            bail!("encrypted frame plaintext too large: {len}");
        }

        let mut body = vec![0u8; len + TAG_LEN];
        self.inner.read_exact(&mut body).await?;

        let nonce_bytes = nonce_for(self.counter);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let tag_start = body.len() - TAG_LEN;
        let tag = chacha20poly1305::Tag::clone_from_slice(&body[tag_start..]);
        body.truncate(tag_start);
        self.cipher
            .decrypt_in_place_detached(nonce, &header, &mut body, &tag)
            .map_err(|_| anyhow!("AEAD decrypt failed (frame {})", self.counter))?;

        self.counter += 1;
        self.buf.extend_from_slice(&body);
        Ok(())
    }
}

pub struct EncryptedWriter {
    inner: OwnedWriteHalf,
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl EncryptedWriter {
    pub fn new(inner: OwnedWriteHalf, key: [u8; 32]) -> Self {
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        Self { inner, cipher, counter: 0 }
    }

    pub async fn write_all(&mut self, plaintext: &[u8]) -> Result<()> {
        for chunk in plaintext.chunks(MAX_FRAME_PLAINTEXT) {
            self.write_frame(chunk).await?;
        }
        Ok(())
    }

    pub async fn flush(&mut self) -> Result<()> {
        self.inner.flush().await?;
        Ok(())
    }

    async fn write_frame(&mut self, plaintext: &[u8]) -> Result<()> {
        let aad = (plaintext.len() as u16).to_le_bytes();
        let mut buf = plaintext.to_vec();
        let nonce_bytes = nonce_for(self.counter);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let tag = self
            .cipher
            .encrypt_in_place_detached(nonce, &aad, &mut buf)
            .map_err(|_| anyhow!("AEAD encrypt failed"))?;
        self.inner.write_all(&aad).await?;
        self.inner.write_all(&buf).await?;
        self.inner.write_all(&tag).await?;
        self.counter += 1;
        Ok(())
    }
}

fn nonce_for(counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n
}
