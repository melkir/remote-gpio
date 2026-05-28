use anyhow::{bail, Result};
use http::StatusCode;
use httparse::Header;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use crate::hap::session::{EncryptedReader, EncryptedWriter, MAX_FRAME_PLAINTEXT};

// --- HTTP request reading ----------------------------------------------------

const MAX_HTTP_BUFFER: usize = 16 * MAX_FRAME_PLAINTEXT;

#[derive(Debug)]
pub(super) struct RawRequest {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

impl RawRequest {
    pub fn path_only(&self) -> &str {
        self.path.split('?').next().unwrap_or(&self.path)
    }
    pub fn query_param(&self, key: &str) -> Option<String> {
        let q = self.path.split('?').nth(1)?;
        for (k, v) in form_urlencoded::parse(q.as_bytes()) {
            if k == key {
                return Some(v.into_owned());
            }
        }
        None
    }
}

pub(super) enum HapReader {
    Plain { inner: OwnedReadHalf, buf: Vec<u8> },
    Encrypted(EncryptedReader),
    Upgrading,
}

impl HapReader {
    pub async fn next_request(&mut self) -> Result<RawRequest> {
        match self {
            HapReader::Plain { inner, buf } => read_request_plain(inner, buf).await,
            HapReader::Encrypted(r) => read_request_encrypted(r).await,
            HapReader::Upgrading => bail!("reader temporarily unavailable during upgrade"),
        }
    }

    pub fn upgrade(self, key: [u8; 32]) -> Result<Self> {
        match self {
            HapReader::Plain { inner, buf } => {
                if !buf.is_empty() {
                    bail!(
                        "cannot upgrade HAP reader with {} buffered plain bytes",
                        buf.len()
                    );
                }
                Ok(HapReader::Encrypted(EncryptedReader::new(inner, key)))
            }
            other => Ok(other),
        }
    }
}

pub(super) enum HapWriter {
    Plain(OwnedWriteHalf),
    Encrypted(EncryptedWriter),
    Upgrading,
}

impl HapWriter {
    pub fn is_encrypted(&self) -> bool {
        matches!(self, HapWriter::Encrypted(_))
    }

    pub fn upgrade(self, key: [u8; 32]) -> Self {
        match self {
            HapWriter::Plain(w) => HapWriter::Encrypted(EncryptedWriter::new(w, key)),
            other => other,
        }
    }

    pub async fn write_response(
        &mut self,
        status: StatusCode,
        content_type: &str,
        body: &[u8],
    ) -> Result<()> {
        let head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n",
            status.as_u16(),
            status.canonical_reason().unwrap_or("Unknown"),
            content_type,
            body.len()
        );
        let mut out = Vec::with_capacity(head.len() + body.len());
        out.extend_from_slice(head.as_bytes());
        out.extend_from_slice(body);
        self.write_all(&out).await
    }

    pub async fn write_event(&mut self, body: &[u8]) -> Result<()> {
        let head = format!(
            "EVENT/1.0 200 OK\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let mut out = Vec::with_capacity(head.len() + body.len());
        out.extend_from_slice(head.as_bytes());
        out.extend_from_slice(body);
        self.write_all(&out).await
    }

    pub async fn write_status(&mut self, status: StatusCode) -> Result<()> {
        let head = format!(
            "HTTP/1.1 {} {}\r\nContent-Length: 0\r\n\r\n",
            status.as_u16(),
            status.canonical_reason().unwrap_or("Unknown")
        );
        self.write_all(head.as_bytes()).await
    }

    async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        match self {
            HapWriter::Plain(w) => {
                w.write_all(data).await?;
                w.flush().await?;
            }
            HapWriter::Encrypted(w) => {
                w.write_all(data).await?;
                w.flush().await?;
            }
            HapWriter::Upgrading => bail!("writer temporarily unavailable during upgrade"),
        }
        Ok(())
    }
}

async fn read_request_plain(reader: &mut OwnedReadHalf, buf: &mut Vec<u8>) -> Result<RawRequest> {
    loop {
        if let Some(req) = try_parse(buf)? {
            return Ok(req);
        }
        if buf.len() >= MAX_HTTP_BUFFER {
            bail!("plain HTTP request too large");
        }
        let mut chunk = [0u8; 2048];
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            bail!("connection closed");
        }
        if buf.len() + n > MAX_HTTP_BUFFER {
            bail!("plain HTTP request too large");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

async fn read_request_encrypted(reader: &mut EncryptedReader) -> Result<RawRequest> {
    loop {
        // Try parse against currently buffered plaintext (clone to a Vec since
        // try_parse mutates).
        let mut snapshot = reader.buffered().to_vec();
        if let Some(req) = try_parse(&mut snapshot)? {
            let consumed = reader.buffered().len() - snapshot.len();
            reader.consume(consumed);
            return Ok(req);
        }
        // Need more bytes.
        reader.fill(reader.buffered().len() + 1).await?;
        if reader.buffered().is_empty() {
            bail!("encrypted connection closed");
        }
        // safety: prevent runaway frames
        if reader.buffered().len() > MAX_HTTP_BUFFER {
            bail!("encrypted request too large");
        }
    }
}

fn try_parse(buf: &mut Vec<u8>) -> Result<Option<RawRequest>> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut req = httparse::Request::new(&mut headers);
    let header_len = match req.parse(buf)? {
        httparse::Status::Complete(n) => n,
        httparse::Status::Partial => return Ok(None),
    };
    let content_length = parse_content_length(req.headers)?;
    if buf.len() < header_len + content_length {
        return Ok(None);
    }
    let Some(method) = req.method else {
        bail!("HTTP request missing method");
    };
    let Some(path) = req.path else {
        bail!("HTTP request missing path");
    };
    let method = method.to_string();
    let path = path.to_string();
    let body = buf[header_len..header_len + content_length].to_vec();
    buf.drain(..header_len + content_length);
    Ok(Some(RawRequest { method, path, body }))
}

fn parse_content_length(headers: &[Header<'_>]) -> Result<usize> {
    let mut parsed = None;
    for header in headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case("content-length"))
    {
        let value = std::str::from_utf8(header.value)?;
        let length = value
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("invalid Content-Length: {value}"))?;
        if parsed.replace(length).is_some() {
            bail!("duplicate Content-Length header");
        }
    }
    Ok(parsed.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Result<Option<RawRequest>> {
        let mut buf = input.as_bytes().to_vec();
        try_parse(&mut buf)
    }

    #[test]
    fn parses_complete_request_and_drains_only_that_request() {
        let mut buf = b"POST /pair-setup HTTP/1.1\r\nContent-Length: 4\r\n\r\nbodyGET /accessories HTTP/1.1\r\n\r\n".to_vec();

        let req = try_parse(&mut buf).unwrap().unwrap();

        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/pair-setup");
        assert_eq!(req.body, b"body");
        assert_eq!(buf, b"GET /accessories HTTP/1.1\r\n\r\n");
    }

    #[test]
    fn waits_for_declared_body_bytes() {
        assert!(
            parse("POST /pair-setup HTTP/1.1\r\nContent-Length: 4\r\n\r\nbo")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_invalid_content_length() {
        let err =
            parse("POST /pair-setup HTTP/1.1\r\nContent-Length: nope\r\n\r\nbody").unwrap_err();

        assert!(err.to_string().contains("invalid Content-Length"));
    }

    #[test]
    fn rejects_duplicate_content_length() {
        let err = parse(
            "POST /pair-setup HTTP/1.1\r\nContent-Length: 4\r\nContent-Length: 4\r\n\r\nbody",
        )
        .unwrap_err();

        assert!(err.to_string().contains("duplicate Content-Length"));
    }

    #[test]
    fn decodes_query_parameters() {
        let req = RawRequest {
            method: "GET".to_string(),
            path: "/characteristics?id=2.9%2C3.10&name=Living+Room".to_string(),
            body: Vec::new(),
        };

        assert_eq!(req.path_only(), "/characteristics");
        assert_eq!(req.query_param("id").as_deref(), Some("2.9,3.10"));
        assert_eq!(req.query_param("name").as_deref(), Some("Living Room"));
    }

    #[test]
    fn finds_query_param_after_empty_segment() {
        let req = RawRequest {
            method: "GET".to_string(),
            path: "/characteristics?&id=2.9".to_string(),
            body: Vec::new(),
        };

        assert_eq!(req.query_param("id").as_deref(), Some("2.9"));
    }

    #[test]
    fn rejects_malformed_request_line() {
        assert!(parse(" /pair-setup HTTP/1.1\r\n\r\n").is_err());
        assert!(parse("POST  HTTP/1.1\r\n\r\n").is_err());
    }
}
