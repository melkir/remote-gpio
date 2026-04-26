//! HAP TCP server: per-connection state machine handling pair-setup,
//! pair-verify, and (post-verify) the encrypted accessory protocol.

use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Mutex};

use crate::hap::accessories::{self, Blind, IID_CURRENT_POSITION, IID_TARGET_POSITION};
use crate::hap::pair_setup::PairSetupSession;
use crate::hap::pair_verify::{HandleOutcome, PairVerifySession};
use crate::hap::positions;
use crate::hap::session::{EncryptedReader, EncryptedWriter, SessionKeys, MAX_FRAME_PLAINTEXT};
use crate::hap::state::{HapState, HAP_PORT};
use crate::remote::Command;
use crate::server::AppState;

/// Shared HAP runtime state. Wraps the persistent `HapState` plus the
/// in-memory accessory position cache.
pub struct HapContext {
    pub state: Mutex<HapState>,
    pub app: Arc<AppState>,
    /// aid → cached position (0 or 100). Updated on any successful PUT.
    pub positions: Mutex<HashMap<u64, u8>>,
    /// Broadcast channel for position changes. Each connection subscribes and
    /// fans out EVENT/1.0 frames to its currently subscribed characteristics.
    pub events: broadcast::Sender<Vec<(u64, u8)>>,
}

type Subscriptions = HashSet<(u64, u64)>;

pub async fn serve(ctx: Arc<HapContext>) -> Result<()> {
    let addr: SocketAddr = ([0, 0, 0, 0], HAP_PORT).into();
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("HAP server listening on {}", addr);

    loop {
        let (stream, peer) = listener.accept().await?;
        let ctx = ctx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, ctx).await {
                tracing::debug!("hap connection {} closed: {}", peer, e);
            }
        });
    }
}

async fn handle_connection(stream: tokio::net::TcpStream, ctx: Arc<HapContext>) -> Result<()> {
    let (read_half, write_half) = stream.into_split();
    let mut reader = HapReader::Plain {
        inner: read_half,
        buf: Vec::new(),
    };
    let mut writer = HapWriter::Plain(write_half);
    let mut pair_setup = PairSetupSession::new();
    let mut pair_verify = PairVerifySession::new();
    let mut subs: Subscriptions = HashSet::new();
    let mut event_rx = ctx.events.subscribe();

    loop {
        tokio::select! {
            req = reader.next_request() => {
                let req = req?;
                tracing::debug!("hap request: {} {}", req.method, req.path);

                match (req.method.as_str(), req.path_only()) {
                    ("POST", "/pair-setup") => {
                        let mut state = ctx.state.lock().await;
                        let body = pair_setup.handle(&req.body, &mut state);
                        drop(state);
                        writer
                            .write_response(200, "application/pairing+tlv8", &body)
                            .await?;
                    }
                    ("POST", "/pair-verify") => {
                        let state = ctx.state.lock().await;
                        let outcome = pair_verify.handle(&req.body, &state);
                        drop(state);
                        match outcome {
                            HandleOutcome::Reply(body) => {
                                writer
                                    .write_response(200, "application/pairing+tlv8", &body)
                                    .await?;
                            }
                            HandleOutcome::Verified { reply, shared_secret } => {
                                writer
                                    .write_response(200, "application/pairing+tlv8", &reply)
                                    .await?;
                                let keys = SessionKeys::derive(&shared_secret)?;
                                reader = reader.upgrade(keys.read);
                                writer = writer.upgrade(keys.write);
                                tracing::info!("hap session encrypted; switched to control channel");
                            }
                        }
                    }
                    ("GET", "/accessories") => {
                        if !writer.is_encrypted() {
                            writer.write_status(401, "Unauthorized").await?;
                            continue;
                        }
                        let positions = snapshot_positions(&ctx).await;
                        let body = serde_json::to_vec(&accessories::build_accessories(&positions))?;
                        writer.write_response(200, "application/hap+json", &body).await?;
                    }
                    ("GET", "/characteristics") => {
                        if !writer.is_encrypted() {
                            writer.write_status(401, "Unauthorized").await?;
                            continue;
                        }
                        let ids = req.query_param("id").unwrap_or_default();
                        let body = handle_get_characteristics(&ctx, &ids).await;
                        writer.write_response(200, "application/hap+json", body.as_bytes()).await?;
                    }
                    ("PUT", "/characteristics") => {
                        if !writer.is_encrypted() {
                            writer.write_status(401, "Unauthorized").await?;
                            continue;
                        }
                        match handle_put_characteristics(&ctx, &req.body, &mut subs).await {
                            Ok(changes) => {
                                writer.write_status(204, "No Content").await?;
                                if !changes.is_empty() {
                                    let _ = ctx.events.send(changes);
                                }
                            }
                            Err(e) => {
                                tracing::warn!("PUT /characteristics failed: {e}");
                                writer.write_status(400, "Bad Request").await?;
                            }
                        }
                    }
                    ("POST", "/pairings") => {
                        writer.write_status(503, "Service Unavailable").await?;
                    }
                    (method, path) => {
                        tracing::warn!("hap: unhandled {method} {path}");
                        writer.write_status(404, "Not Found").await?;
                    }
                }
            }
            evt = event_rx.recv() => {
                match evt {
                    Ok(changes) => {
                        if !writer.is_encrypted() {
                            continue;
                        }
                        if let Some(body) = build_event_body(&changes, &subs) {
                            writer.write_event(&body).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("hap event subscriber lagged by {n}");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    Ok(())
}

fn build_event_body(changes: &[(u64, u8)], subs: &Subscriptions) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for &(aid, pos) in changes {
        for &iid in &[
            IID_CURRENT_POSITION,
            IID_TARGET_POSITION,
            accessories::IID_POSITION_STATE,
        ] {
            if !subs.contains(&(aid, iid)) {
                continue;
            }
            let value = if iid == accessories::IID_POSITION_STATE {
                2u8
            } else {
                pos
            };
            out.push(serde_json::json!({ "aid": aid, "iid": iid, "value": value }));
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(
        serde_json::json!({ "characteristics": out })
            .to_string()
            .into_bytes(),
    )
}

async fn snapshot_positions(ctx: &HapContext) -> Vec<(u64, u8)> {
    let positions = ctx.positions.lock().await;
    accessories::BLINDS
        .iter()
        .map(|b| (b.aid, positions.get(&b.aid).copied().unwrap_or(100)))
        .collect()
}

async fn handle_get_characteristics(ctx: &HapContext, ids: &str) -> String {
    let positions = ctx.positions.lock().await;
    let mut out = Vec::new();
    for pair in ids.split(',') {
        let mut parts = pair.split('.');
        let aid: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let iid: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let value: Value = match (aid, iid) {
            (a, i) if accessories::find_blind(a).is_some() && i == IID_CURRENT_POSITION => {
                serde_json::Value::Number(positions.get(&a).copied().unwrap_or(100).into())
            }
            (a, i) if accessories::find_blind(a).is_some() && i == IID_TARGET_POSITION => {
                serde_json::Value::Number(positions.get(&a).copied().unwrap_or(100).into())
            }
            (a, i)
                if accessories::find_blind(a).is_some() && i == accessories::IID_POSITION_STATE =>
            {
                serde_json::Value::Number(2.into())
            }
            _ => serde_json::Value::Null,
        };
        out.push(serde_json::json!({ "aid": aid, "iid": iid, "value": value }));
    }
    serde_json::json!({ "characteristics": out }).to_string()
}

async fn handle_put_characteristics(
    ctx: &HapContext,
    body: &[u8],
    subs: &mut Subscriptions,
) -> Result<Vec<(u64, u8)>> {
    let parsed: Value = serde_json::from_slice(body)?;
    let chars = parsed
        .get("characteristics")
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow!("missing characteristics array"))?;

    let mut changes: Vec<(u64, u8)> = Vec::new();
    for entry in chars {
        let aid = entry.get("aid").and_then(|v| v.as_u64()).unwrap_or(0);
        let iid = entry.get("iid").and_then(|v| v.as_u64()).unwrap_or(0);

        // Event subscription toggle. `{aid, iid, ev: true|false}` has no
        // `value` key — handle it before the value-write path.
        if let Some(ev) = entry.get("ev").and_then(|v| v.as_bool()) {
            if ev {
                subs.insert((aid, iid));
            } else {
                subs.remove(&(aid, iid));
            }
            continue;
        }

        if iid != IID_TARGET_POSITION {
            continue;
        }
        let value = match entry.get("value").and_then(|v| v.as_u64()) {
            Some(v) => v as u8,
            None => continue,
        };
        let blind: &Blind = match accessories::find_blind(aid) {
            Some(b) => b,
            None => continue,
        };
        let snapped = if value >= 50 { 100u8 } else { 0u8 };

        // Skip the physical command when the cached position already matches.
        // iOS replays the last-seen TargetPosition right after pairing; without
        // this check we'd send an unwanted UP/DOWN on registration.
        let current = ctx.positions.lock().await.get(&aid).copied().unwrap_or(100);
        if current == snapped {
            tracing::debug!("PUT TargetPosition aid={aid} value={snapped}: cache hit, no-op");
            continue;
        }

        let command = if snapped == 100 {
            Command::Up
        } else {
            Command::Down
        };
        ctx.app
            .remote_control
            .execute(Some(blind.led), command)
            .await?;

        let mut positions = ctx.positions.lock().await;
        let before = positions.clone();
        positions.insert(aid, snapped);
        propagate_positions(&mut positions, blind, snapped);
        let snapshot = positions.clone();
        drop(positions);
        if let Err(e) = positions::save(&snapshot) {
            tracing::warn!("failed to persist positions: {e}");
        }
        for (k, v) in &snapshot {
            if before.get(k) != Some(v) {
                changes.push((*k, *v));
            }
        }
    }
    Ok(changes)
}

fn propagate_positions(positions: &mut HashMap<u64, u8>, changed: &Blind, snapped: u8) {
    use crate::gpio::Input;
    if matches!(changed.led, Input::ALL) {
        for b in accessories::BLINDS
            .iter()
            .filter(|b| !matches!(b.led, Input::ALL))
        {
            positions.insert(b.aid, snapped);
        }
        return;
    }
    let individuals: Vec<&Blind> = accessories::BLINDS
        .iter()
        .filter(|b| !matches!(b.led, Input::ALL))
        .collect();
    let all_match = individuals
        .iter()
        .all(|b| positions.get(&b.aid).copied() == Some(snapped));
    if all_match {
        if let Some(all_blind) = accessories::BLINDS
            .iter()
            .find(|b| matches!(b.led, Input::ALL))
        {
            positions.insert(all_blind.aid, snapped);
        }
    }
}

// --- HTTP request reading ----------------------------------------------------

struct RawRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

impl RawRequest {
    fn path_only(&self) -> &str {
        self.path.split('?').next().unwrap_or(&self.path)
    }
    fn query_param(&self, key: &str) -> Option<String> {
        let q = self.path.split('?').nth(1)?;
        for part in q.split('&') {
            let mut it = part.splitn(2, '=');
            let k = it.next()?;
            let v = it.next().unwrap_or("");
            if k == key {
                return Some(v.to_string());
            }
        }
        None
    }
}

enum HapReader {
    Plain { inner: OwnedReadHalf, buf: Vec<u8> },
    Encrypted(EncryptedReader),
}

impl HapReader {
    async fn next_request(&mut self) -> Result<RawRequest> {
        match self {
            HapReader::Plain { inner, buf } => read_request_plain(inner, buf).await,
            HapReader::Encrypted(r) => read_request_encrypted(r).await,
        }
    }

    fn upgrade(self, key: [u8; 32]) -> Self {
        match self {
            HapReader::Plain { inner, .. } => {
                HapReader::Encrypted(EncryptedReader::new(inner, key))
            }
            other => other,
        }
    }
}

enum HapWriter {
    Plain(OwnedWriteHalf),
    Encrypted(EncryptedWriter),
}

impl HapWriter {
    fn is_encrypted(&self) -> bool {
        matches!(self, HapWriter::Encrypted(_))
    }

    fn upgrade(self, key: [u8; 32]) -> Self {
        match self {
            HapWriter::Plain(w) => HapWriter::Encrypted(EncryptedWriter::new(w, key)),
            other => other,
        }
    }

    async fn write_response(&mut self, status: u16, content_type: &str, body: &[u8]) -> Result<()> {
        let head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n",
            status,
            status_phrase(status),
            content_type,
            body.len()
        );
        let mut out = Vec::with_capacity(head.len() + body.len());
        out.extend_from_slice(head.as_bytes());
        out.extend_from_slice(body);
        self.write_all(&out).await
    }

    async fn write_event(&mut self, body: &[u8]) -> Result<()> {
        let head = format!(
            "EVENT/1.0 200 OK\r\nContent-Type: application/hap+json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let mut out = Vec::with_capacity(head.len() + body.len());
        out.extend_from_slice(head.as_bytes());
        out.extend_from_slice(body);
        self.write_all(&out).await
    }

    async fn write_status(&mut self, status: u16, phrase: &str) -> Result<()> {
        let head = format!(
            "HTTP/1.1 {} {}\r\nContent-Length: 0\r\n\r\n",
            status, phrase
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
        }
        Ok(())
    }
}

async fn read_request_plain(reader: &mut OwnedReadHalf, buf: &mut Vec<u8>) -> Result<RawRequest> {
    loop {
        if let Some(req) = try_parse(buf)? {
            return Ok(req);
        }
        let mut chunk = [0u8; 2048];
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            bail!("connection closed");
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
        if reader.buffered().len() > 16 * MAX_FRAME_PLAINTEXT {
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
    let content_length: usize = req
        .headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-length"))
        .and_then(|h| std::str::from_utf8(h.value).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if buf.len() < header_len + content_length {
        return Ok(None);
    }
    let method = req.method.unwrap_or("").to_string();
    let path = req.path.unwrap_or("").to_string();
    let body = buf[header_len..header_len + content_length].to_vec();
    buf.drain(..header_len + content_length);
    Ok(Some(RawRequest { method, path, body }))
}

fn status_phrase(code: u16) -> &'static str {
    match code {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}
