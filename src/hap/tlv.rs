//! HAP TLV8 codec. See HAP spec section 14.1.
//!
//! Each item is 1-byte type + 1-byte length + value. Values longer than 255
//! bytes are split into consecutive fragments with the same type tag and
//! reassembled on decode.

use anyhow::{bail, Result};

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tag {
    Method = 0x00,
    Identifier = 0x01,
    Salt = 0x02,
    PublicKey = 0x03,
    Proof = 0x04,
    EncryptedData = 0x05,
    State = 0x06,
    Error = 0x07,
    RetryDelay = 0x08,
    Certificate = 0x09,
    Signature = 0x0A,
    Permissions = 0x0B,
    FragmentData = 0x0C,
    FragmentLast = 0x0D,
    Separator = 0xFF,
}

#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum HapError {
    Unknown = 0x01,
    Authentication = 0x02,
    Backoff = 0x03,
    MaxPeers = 0x04,
    MaxTries = 0x05,
    Unavailable = 0x06,
    Busy = 0x07,
}

#[derive(Debug, Default)]
pub struct Tlv {
    items: Vec<(u8, Vec<u8>)>,
}

impl Tlv {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(mut self, tag: Tag, value: impl Into<Vec<u8>>) -> Self {
        self.items.push((tag as u8, value.into()));
        self
    }

    pub fn put_u8(self, tag: Tag, value: u8) -> Self {
        self.put(tag, vec![value])
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (tag, value) in &self.items {
            if value.is_empty() {
                out.push(*tag);
                out.push(0);
                continue;
            }
            for chunk in value.chunks(255) {
                out.push(*tag);
                out.push(chunk.len() as u8);
                out.extend_from_slice(chunk);
            }
            if value.len() % 255 == 0 {
                out.push(*tag);
                out.push(0);
            }
        }
        out
    }
}

/// Parsed TLV. Values are reassembled across fragments per HAP rules:
/// consecutive items with the same tag (each 255 bytes long) are concatenated
/// until a shorter fragment terminates the run.
#[derive(Debug, Default)]
pub struct ParsedTlv {
    items: Vec<(u8, Vec<u8>)>,
}

impl ParsedTlv {
    pub fn parse(input: &[u8]) -> Result<Self> {
        let mut items: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut terminated_tag: Option<u8> = None;
        let mut i = 0;
        while i < input.len() {
            if i + 2 > input.len() {
                bail!("TLV truncated header");
            }
            let tag = input[i];
            let len = input[i + 1] as usize;
            i += 2;
            if i + len > input.len() {
                bail!("TLV truncated value");
            }
            let value = &input[i..i + len];
            i += len;

            if len == 0 {
                if let Some(last) = items.last() {
                    if last.0 == tag && last.1.len() % 255 == 0 && !last.1.is_empty() {
                        terminated_tag = Some(tag);
                        continue;
                    }
                }
            }

            // Concatenate fragments: previous item with same tag of length 255.
            if let Some(last) = items.last_mut() {
                if last.0 == tag
                    && last.1.len() % 255 == 0
                    && !last.1.is_empty()
                    && terminated_tag != Some(tag)
                {
                    last.1.extend_from_slice(value);
                    terminated_tag = None;
                    continue;
                }
            }
            items.push((tag, value.to_vec()));
            terminated_tag = None;
        }
        Ok(Self { items })
    }

    pub fn get(&self, tag: Tag) -> Option<&[u8]> {
        self.items
            .iter()
            .find(|(t, _)| *t == tag as u8)
            .map(|(_, v)| v.as_slice())
    }

    pub fn get_u8(&self, tag: Tag) -> Option<u8> {
        self.get(tag).and_then(|v| v.first().copied())
    }
}

pub fn error_response(state: u8, err: HapError) -> Vec<u8> {
    Tlv::new()
        .put_u8(Tag::State, state)
        .put_u8(Tag::Error, err as u8)
        .encode()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_short() {
        let bytes = Tlv::new()
            .put_u8(Tag::State, 2)
            .put(Tag::Salt, vec![1, 2, 3, 4])
            .encode();
        let parsed = ParsedTlv::parse(&bytes).unwrap();
        assert_eq!(parsed.get_u8(Tag::State), Some(2));
        assert_eq!(parsed.get(Tag::Salt), Some(&[1u8, 2, 3, 4][..]));
    }

    #[test]
    fn round_trip_fragmented() {
        let big = vec![0xAB; 600];
        let bytes = Tlv::new().put(Tag::PublicKey, big.clone()).encode();
        // 600 bytes splits into 255 + 255 + 90, so 3 fragments + 3 headers.
        assert_eq!(bytes.len(), 600 + 6);
        let parsed = ParsedTlv::parse(&bytes).unwrap();
        assert_eq!(parsed.get(Tag::PublicKey), Some(big.as_slice()));
    }

    #[test]
    fn fragment_boundary_exact_multiple() {
        // Exact-255 fragments are followed by a zero-length terminator so the
        // next item with the same tag starts a separate value.
        let bytes = Tlv::new()
            .put(Tag::PublicKey, vec![1u8; 255])
            .put(Tag::PublicKey, vec![2u8; 10])
            .encode();
        let parsed = ParsedTlv::parse(&bytes).unwrap();
        let values: Vec<&[u8]> = parsed
            .items
            .iter()
            .filter(|(tag, _)| *tag == Tag::PublicKey as u8)
            .map(|(_, value)| value.as_slice())
            .collect();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0], vec![1u8; 255]);
        assert_eq!(values[1], vec![2u8; 10]);
    }
}
