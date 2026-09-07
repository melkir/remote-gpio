//! Post-Pair-Verify session encryption (HAP §6.5).
//!
//! Frames: 2-byte little-endian length || ciphertext(plaintext, len) || 16-byte tag.
//! Nonce: 4 zero bytes || u64 little-endian counter.
//! Counters increment per frame, separately for each direction.

use anyhow::{anyhow, bail, Result};
use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use crate::hap::crypto::hkdf_sha512;

pub const MAX_FRAME_PLAINTEXT: usize = 1024;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = 2;
/// Largest frame on the wire: length prefix + max plaintext + tag.
const MAX_FRAME_WIRE: usize = HEADER_LEN + MAX_FRAME_PLAINTEXT + TAG_LEN;

pub struct SessionKeys {
    pub read: [u8; 32],  // controller -> accessory ("Control-Write-Encryption-Key")
    pub write: [u8; 32], // accessory -> controller ("Control-Read-Encryption-Key")
}

impl SessionKeys {
    pub fn derive(shared_secret: &[u8]) -> Result<Self> {
        const SALT: &[u8] = b"Control-Salt";
        Ok(Self {
            read: hkdf_sha512(shared_secret, SALT, b"Control-Write-Encryption-Key")?,
            write: hkdf_sha512(shared_secret, SALT, b"Control-Read-Encryption-Key")?,
        })
    }
}

pub struct EncryptedReader {
    inner: OwnedReadHalf,
    cipher: ChaCha20Poly1305,
    counter: u64,
    buf: Vec<u8>,
    /// Raw wire bytes of the frame currently being assembled.
    ///
    /// Reads are driven from a `select!` arm, so this future can be dropped at
    /// any await point. Bytes already taken off the socket must survive that
    /// cancellation — held in a local they would be silently discarded and the
    /// stream would desync — so they accumulate here instead.
    frame: Vec<u8>,
}

impl EncryptedReader {
    pub fn new(inner: OwnedReadHalf, key: [u8; 32]) -> Self {
        let cipher = ChaCha20Poly1305::new(&Key::from(key));
        Self {
            inner,
            cipher,
            counter: 0,
            buf: Vec::new(),
            frame: Vec::with_capacity(MAX_FRAME_WIRE),
        }
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
        let n = n.min(self.buf.len());
        self.buf.drain(..n);
    }

    /// Accumulate wire bytes until [`Self::frame`] holds at least `want`.
    ///
    /// Uses `read` rather than `read_exact` because only `read` is
    /// cancellation-safe: if the surrounding `select!` drops this future, `read`
    /// guarantees no bytes were consumed, and everything already received is
    /// still in `self.frame`. `read_exact` would discard a partial fill.
    async fn fill_frame(&mut self, want: usize) -> Result<()> {
        let mut chunk = [0u8; MAX_FRAME_WIRE];
        while self.frame.len() < want {
            let wanted = want - self.frame.len();
            let read = self.inner.read(&mut chunk[..wanted]).await?;
            if read == 0 {
                bail!("encrypted connection closed mid-frame");
            }
            self.frame.extend_from_slice(&chunk[..read]);
        }
        Ok(())
    }

    async fn read_one_frame(&mut self) -> Result<()> {
        self.fill_frame(HEADER_LEN).await?;
        let header = [self.frame[0], self.frame[1]];
        let len = u16::from_le_bytes(header) as usize;
        if len > MAX_FRAME_PLAINTEXT {
            bail!("encrypted frame plaintext too large: {len}");
        }
        self.fill_frame(HEADER_LEN + len + TAG_LEN).await?;

        // No awaits past this point, so the assembled frame can be consumed and
        // its buffer recycled for the next one.
        let Self {
            cipher,
            counter,
            buf,
            frame,
            ..
        } = self;
        let (ciphertext, tag_bytes) = frame[HEADER_LEN..].split_at_mut(len);
        let tag = chacha20poly1305::Tag::try_from(&*tag_bytes)
            .map_err(|_| anyhow!("invalid AEAD tag length"))?;
        let nonce = Nonce::from(nonce_for(*counter));
        cipher
            .decrypt_inout_detached(&nonce, &header, (&mut *ciphertext).into(), &tag)
            .map_err(|_| anyhow!("AEAD decrypt failed (frame {counter})"))?;

        *counter += 1;
        buf.extend_from_slice(ciphertext);
        frame.clear();
        Ok(())
    }
}

pub struct EncryptedWriter {
    inner: OwnedWriteHalf,
    cipher: ChaCha20Poly1305,
    counter: u64,
    /// Reused staging buffer holding an entire outgoing message — every frame
    /// of it — so the message leaves in a single write.
    frame: Vec<u8>,
}

impl EncryptedWriter {
    pub fn new(inner: OwnedWriteHalf, key: [u8; 32]) -> Self {
        let cipher = ChaCha20Poly1305::new(&Key::from(key));
        Self {
            inner,
            cipher,
            counter: 0,
            frame: Vec::with_capacity(MAX_FRAME_WIRE),
        }
    }

    /// Encrypt `plaintext` into as many frames as it needs and emit the whole
    /// message in a single write.
    ///
    /// The socket is unbuffered, so a write per frame — let alone the three per
    /// frame this used to do — is a syscall per 1 KB, each leading with a 2-byte
    /// length segment that Nagle would hold back waiting on an ACK. Responses
    /// larger than one frame (`/accessories` is several KB) are the common case.
    pub async fn write_all(&mut self, plaintext: &[u8]) -> Result<()> {
        self.frame.clear();
        for chunk in plaintext.chunks(MAX_FRAME_PLAINTEXT) {
            self.encode_frame(chunk)?;
        }
        self.inner.write_all(&self.frame).await?;
        Ok(())
    }

    pub async fn flush(&mut self) -> Result<()> {
        self.inner.flush().await?;
        Ok(())
    }

    /// Append one `len || ciphertext || tag` frame to the staging buffer.
    fn encode_frame(&mut self, plaintext: &[u8]) -> Result<()> {
        let aad = (plaintext.len() as u16).to_le_bytes();
        let body = self.frame.len() + HEADER_LEN;
        self.frame.extend_from_slice(&aad);
        self.frame.extend_from_slice(plaintext);
        let nonce = Nonce::from(nonce_for(self.counter));
        let tag = self
            .cipher
            .encrypt_inout_detached(&nonce, &aad, (&mut self.frame[body..]).into())
            .map_err(|_| anyhow!("AEAD encrypt failed"))?;
        self.frame.extend_from_slice(&tag);
        self.counter += 1;
        Ok(())
    }
}

fn nonce_for(counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::{TcpListener, TcpStream};

    const KEY: [u8; 32] = [7u8; 32];

    /// Independent oracle for the wire format, mirroring [`EncryptedWriter::encode_frame`].
    fn encrypt_frame(counter: u64, plaintext: &[u8]) -> Vec<u8> {
        let cipher = ChaCha20Poly1305::new(&Key::from(KEY));
        let aad = (plaintext.len() as u16).to_le_bytes();
        let mut out = Vec::new();
        out.extend_from_slice(&aad);
        out.extend_from_slice(plaintext);
        let nonce = Nonce::from(nonce_for(counter));
        let tag = cipher
            .encrypt_inout_detached(&nonce, &aad, (&mut out[HEADER_LEN..]).into())
            .unwrap();
        out.extend_from_slice(&tag);
        out
    }

    async fn connected() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, server)
    }

    /// An `EncryptedReader`, the raw peer socket feeding it, and the reader
    /// side's unused write half (held so the socket is not half-shut).
    async fn reader_pair() -> (EncryptedReader, TcpStream, OwnedWriteHalf) {
        let (client, server) = connected().await;
        let (read_half, write_half) = server.into_split();
        (EncryptedReader::new(read_half, KEY), client, write_half)
    }

    #[tokio::test]
    async fn frame_split_across_reads_is_reassembled() {
        let (mut reader, mut peer, _writer_guard) = reader_pair().await;
        let frame = encrypt_frame(0, b"hello world");

        let (head, tail) = frame.split_at(5);
        peer.write_all(head).await.unwrap();
        peer.flush().await.unwrap();
        peer.write_all(tail).await.unwrap();
        peer.flush().await.unwrap();

        reader.fill(b"hello world".len()).await.unwrap();
        assert_eq!(reader.buffered(), b"hello world");
    }

    /// The reader is driven from a `select!` arm, so a partially delivered frame
    /// can have its read future dropped. Bytes already off the socket must
    /// survive that, or the stream desyncs and every later frame fails to
    /// decrypt.
    #[tokio::test]
    async fn cancelling_a_partial_read_does_not_lose_buffered_bytes() {
        let (mut reader, mut peer, _writer_guard) = reader_pair().await;
        let plaintext = b"the quick brown fox jumps over the lazy dog";
        let frame = encrypt_frame(0, plaintext);
        let (head, tail) = frame.split_at(9);

        peer.write_all(head).await.unwrap();
        peer.flush().await.unwrap();

        // Only `head` is available, so `fill` parks mid-frame and the timeout
        // wins the race — dropping the read future exactly as `select!` would.
        let cancelled = tokio::select! {
            result = reader.fill(plaintext.len()) => Some(result),
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => None,
        };
        assert!(cancelled.is_none(), "fill should not have completed yet");

        peer.write_all(tail).await.unwrap();
        peer.flush().await.unwrap();

        reader.fill(plaintext.len()).await.unwrap();
        assert_eq!(reader.buffered(), plaintext);
    }

    /// Frames can be pipelined into one TCP segment. `fill_frame` must stop at
    /// the current frame's boundary, or the next frame's bytes get absorbed into
    /// it and both fail to decrypt.
    #[tokio::test]
    async fn two_frames_in_one_segment_decrypt_independently() {
        let (mut reader, mut peer, _writer_guard) = reader_pair().await;
        let mut wire = encrypt_frame(0, b"alpha");
        wire.extend_from_slice(&encrypt_frame(1, b"omega"));

        peer.write_all(&wire).await.unwrap();
        peer.flush().await.unwrap();

        reader.fill(b"alphaomega".len()).await.unwrap();
        assert_eq!(reader.buffered(), b"alphaomega");
    }

    #[tokio::test]
    async fn repeated_cancellation_still_decrypts_consecutive_frames() {
        let (mut reader, mut peer, _writer_guard) = reader_pair().await;
        let first = encrypt_frame(0, b"first frame");
        let second = encrypt_frame(1, b"second frame");
        let wire: Vec<u8> = first.iter().chain(second.iter()).copied().collect();

        // Dribble the stream one byte at a time, cancelling between every byte.
        for byte in &wire {
            peer.write_all(&[*byte]).await.unwrap();
            peer.flush().await.unwrap();
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(5),
                reader.fill(b"first framesecond frame".len()),
            )
            .await;
        }

        reader.fill(b"first framesecond frame".len()).await.unwrap();
        assert_eq!(reader.buffered(), b"first framesecond frame");
    }

    #[tokio::test]
    async fn writer_emits_one_contiguous_frame_per_chunk() {
        let (client, server) = connected().await;
        let (_client_read, write_half) = client.into_split();
        let mut writer = EncryptedWriter::new(write_half, KEY);

        writer.write_all(b"payload").await.unwrap();
        writer.flush().await.unwrap();

        let (read_half, _server_write) = server.into_split();
        let mut reader = EncryptedReader::new(read_half, KEY);
        reader.fill(b"payload".len()).await.unwrap();
        assert_eq!(reader.buffered(), b"payload");
    }

    #[tokio::test]
    async fn writer_splits_oversized_payloads_into_reader_readable_frames() {
        let (client, server) = connected().await;
        let (_client_read, write_half) = client.into_split();
        let mut writer = EncryptedWriter::new(write_half, KEY);

        let payload = vec![0xABu8; MAX_FRAME_PLAINTEXT + 100];
        writer.write_all(&payload).await.unwrap();
        writer.flush().await.unwrap();

        let (read_half, _server_write) = server.into_split();
        let mut reader = EncryptedReader::new(read_half, KEY);
        reader.fill(payload.len()).await.unwrap();
        assert_eq!(reader.buffered(), payload.as_slice());
    }

    /// The whole message must be staged before it hits the socket — that is what
    /// makes it one syscall. A round-trip test alone would still pass if each
    /// frame were written separately, so assert on the staging buffer directly.
    #[tokio::test]
    async fn multi_frame_message_is_staged_as_one_buffer() {
        let (client, _server) = connected().await;
        let (_read, write_half) = client.into_split();
        let mut writer = EncryptedWriter::new(write_half, KEY);

        let payload = vec![0x5Au8; MAX_FRAME_PLAINTEXT + 100];
        writer.write_all(&payload).await.unwrap();

        // Two frames: a full one and a 100-byte remainder, each len + body + tag.
        let expected = (HEADER_LEN + MAX_FRAME_PLAINTEXT + TAG_LEN) + (HEADER_LEN + 100 + TAG_LEN);
        assert_eq!(writer.frame.len(), expected);
        assert_eq!(writer.counter, 2);
    }

    #[tokio::test]
    async fn consume_drops_only_the_requested_prefix() {
        let (mut reader, mut peer, _writer_guard) = reader_pair().await;
        peer.write_all(&encrypt_frame(0, b"abcdef")).await.unwrap();
        peer.flush().await.unwrap();

        reader.fill(6).await.unwrap();
        reader.consume(2);
        assert_eq!(reader.buffered(), b"cdef");
        reader.consume(99);
        assert!(reader.buffered().is_empty());
    }

    #[tokio::test]
    async fn oversized_frame_length_is_rejected() {
        let (mut reader, mut peer, _writer_guard) = reader_pair().await;
        let mut bogus = ((MAX_FRAME_PLAINTEXT + 1) as u16).to_le_bytes().to_vec();
        bogus.extend_from_slice(&[0u8; 32]);
        peer.write_all(&bogus).await.unwrap();
        peer.flush().await.unwrap();

        let err = reader.fill(1).await.unwrap_err();
        assert!(err.to_string().contains("plaintext too large"), "{err}");
    }
}
