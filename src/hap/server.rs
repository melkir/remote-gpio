//! HAP TCP server: per-connection state machine handling pair-setup,
//! pair-verify, and (post-verify) the encrypted accessory protocol.

use anyhow::{anyhow, bail, Result};
use http::StatusCode;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

use crate::hap::pair_setup::{PairSetupSession, PersistPolicy};
use crate::hap::pair_verify::{HandleOutcome, PairVerifySession};
use crate::hap::runtime::{
    CharacteristicEvent, CharacteristicId, CharacteristicRead, CharacteristicWrite,
    CharacteristicWriteOutcome, CharacteristicWriteStatus, HapAccessoryApp, HapRuntime, HapStatus,
    HapStore, Subscriptions,
};
use crate::hap::session::{EncryptedReader, EncryptedWriter, SessionKeys, MAX_FRAME_PLAINTEXT};
use crate::hap::tlv::{HapError, ParsedTlv, Tag as TlvTag, Tlv};

pub async fn serve<A, S>(ctx: Arc<HapRuntime<A, S>>, port: u16) -> Result<()>
where
    A: HapAccessoryApp,
    S: HapStore,
{
    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("HAP server listening on {}", addr);

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                tracing::warn!("hap accept failed; continuing: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let ctx = ctx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, ctx).await {
                tracing::debug!("hap connection {} closed: {}", peer, e);
            }
        });
    }
}

async fn handle_connection<A, S>(
    stream: tokio::net::TcpStream,
    ctx: Arc<HapRuntime<A, S>>,
) -> Result<()>
where
    A: HapAccessoryApp,
    S: HapStore,
{
    let (read_half, write_half) = stream.into_split();
    let mut reader = HapReader::Plain {
        inner: read_half,
        buf: Vec::new(),
    };
    let mut writer = HapWriter::Plain(write_half);
    let mut conn = ConnectionState::new();
    let mut event_rx = ctx.subscribe_events();

    loop {
        tokio::select! {
            req = reader.next_request() => {
                let req = req?;
                tracing::debug!("hap request: {} {}", req.method, req.path);
                let encrypted = writer.is_encrypted();
                let outcome = handle_request(req, &ctx, &mut conn, encrypted).await?;
                write_request_response(outcome.response, &mut reader, &mut writer, &mut conn).await?;
                ctx.publish_events(outcome.events);
            }
            evt = event_rx.recv() => {
                match evt {
                    Ok(changes) => {
                        if !writer.is_encrypted() {
                            continue;
                        }
                        if let Some(body) = build_event_body(&changes, &conn.subs) {
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

struct ConnectionState {
    pair_setup: PairSetupSession,
    pair_verify: PairVerifySession,
    subs: Subscriptions,
    /// The controller identifier learned from the most recent pair-verify on
    /// this connection. Only set after the channel becomes encrypted.
    controller_id: Option<String>,
}

impl ConnectionState {
    fn new() -> Self {
        Self {
            pair_setup: PairSetupSession::new(),
            pair_verify: PairVerifySession::new(),
            subs: Subscriptions::default(),
            controller_id: None,
        }
    }
}

struct RequestOutcome {
    response: OutboundResponse,
    events: Vec<CharacteristicEvent>,
}

impl RequestOutcome {
    fn response(response: OutboundResponse) -> Self {
        Self {
            response,
            events: Vec::new(),
        }
    }
}

enum OutboundResponse {
    Status(StatusCode),
    Body {
        status: StatusCode,
        content_type: &'static str,
        body: Vec<u8>,
    },
    Upgrade {
        reply: Vec<u8>,
        keys: SessionKeys,
        controller_id: String,
    },
}

async fn handle_request<A, S>(
    req: RawRequest,
    ctx: &HapRuntime<A, S>,
    conn: &mut ConnectionState,
    encrypted: bool,
) -> Result<RequestOutcome>
where
    A: HapAccessoryApp,
    S: HapStore,
{
    match (req.method.as_str(), req.path_only()) {
        ("POST", "/pair-setup") => handle_pair_setup(ctx, conn, &req.body).await,
        ("POST", "/pair-verify") => handle_pair_verify(ctx, conn, &req.body).await,
        ("GET", "/accessories") => {
            if !encrypted {
                return Ok(RequestOutcome::response(OutboundResponse::Status(
                    StatusCode::UNAUTHORIZED,
                )));
            }
            let body = serde_json::to_vec(&ctx.app.accessories().await?)?;
            Ok(RequestOutcome::response(OutboundResponse::Body {
                status: StatusCode::OK,
                content_type: "application/hap+json",
                body,
            }))
        }
        ("GET", "/characteristics") => {
            if !encrypted {
                return Ok(RequestOutcome::response(OutboundResponse::Status(
                    StatusCode::UNAUTHORIZED,
                )));
            }
            let ids = req.query_param("id").unwrap_or_default();
            let body = handle_get_characteristics(ctx.app.as_ref(), &ids).await?;
            Ok(RequestOutcome::response(OutboundResponse::Body {
                status: StatusCode::OK,
                content_type: "application/hap+json",
                body,
            }))
        }
        ("PUT", "/characteristics") => {
            if !encrypted {
                return Ok(RequestOutcome::response(OutboundResponse::Status(
                    StatusCode::UNAUTHORIZED,
                )));
            }
            match handle_put_characteristics(ctx.app.as_ref(), &req.body, &mut conn.subs).await {
                Ok(write) => {
                    let response = if write.all_success() {
                        OutboundResponse::Status(StatusCode::NO_CONTENT)
                    } else {
                        OutboundResponse::Body {
                            status: StatusCode::MULTI_STATUS,
                            content_type: "application/hap+json",
                            body: write_statuses_body(write.statuses),
                        }
                    };
                    Ok(RequestOutcome {
                        response,
                        events: write.events,
                    })
                }
                Err(e) => {
                    tracing::warn!("PUT /characteristics failed: {e}");
                    Ok(RequestOutcome::response(OutboundResponse::Status(
                        StatusCode::BAD_REQUEST,
                    )))
                }
            }
        }
        ("POST", "/pairings") => {
            if !encrypted {
                return Ok(RequestOutcome::response(OutboundResponse::Status(
                    StatusCode::UNAUTHORIZED,
                )));
            }
            let body = handle_pairings(ctx, conn.controller_id.as_deref(), &req.body).await;
            Ok(RequestOutcome::response(OutboundResponse::Body {
                status: StatusCode::OK,
                content_type: "application/pairing+tlv8",
                body,
            }))
        }
        (method, path) => {
            tracing::warn!("hap: unhandled {method} {path}");
            Ok(RequestOutcome::response(OutboundResponse::Status(
                StatusCode::NOT_FOUND,
            )))
        }
    }
}

async fn handle_pair_setup<A, S>(
    ctx: &HapRuntime<A, S>,
    conn: &mut ConnectionState,
    body: &[u8],
) -> Result<RequestOutcome>
where
    A: HapAccessoryApp,
    S: HapStore,
{
    let mut state = ctx.state.lock().await;
    let outcome = conn.pair_setup.handle(body, &mut state);
    let body = match (ctx.store.save_state(&state), outcome.persist) {
        (Ok(()), _) => outcome.body,
        (Err(e), PersistPolicy::BestEffort) => {
            tracing::error!("failed to persist pair-setup state: {e}");
            outcome.body
        }
        (Err(e), PersistPolicy::Required) => {
            tracing::error!("pair-setup M5: refusing to claim success after persist failure: {e}");
            error_tlv(6, HapError::Unknown)
        }
    };
    Ok(RequestOutcome::response(OutboundResponse::Body {
        status: StatusCode::OK,
        content_type: "application/pairing+tlv8",
        body,
    }))
}

async fn handle_pair_verify<A, S>(
    ctx: &HapRuntime<A, S>,
    conn: &mut ConnectionState,
    body: &[u8],
) -> Result<RequestOutcome>
where
    A: HapAccessoryApp,
    S: HapStore,
{
    let state = ctx.state.lock().await;
    let outcome = conn.pair_verify.handle(body, &state);
    drop(state);
    match outcome {
        HandleOutcome::Reply(body) => Ok(RequestOutcome::response(OutboundResponse::Body {
            status: StatusCode::OK,
            content_type: "application/pairing+tlv8",
            body,
        })),
        HandleOutcome::Verified {
            reply,
            shared_secret,
            controller_id,
        } => Ok(RequestOutcome::response(OutboundResponse::Upgrade {
            reply,
            keys: SessionKeys::derive(&shared_secret)?,
            controller_id,
        })),
    }
}

async fn write_request_response(
    response: OutboundResponse,
    reader: &mut HapReader,
    writer: &mut HapWriter,
    conn: &mut ConnectionState,
) -> Result<()> {
    match response {
        OutboundResponse::Status(status) => writer.write_status(status).await,
        OutboundResponse::Body {
            status,
            content_type,
            body,
        } => writer.write_response(status, content_type, &body).await,
        OutboundResponse::Upgrade {
            reply,
            keys,
            controller_id,
        } => {
            writer
                .write_response(StatusCode::OK, "application/pairing+tlv8", &reply)
                .await?;
            let upgraded_reader =
                std::mem::replace(reader, HapReader::Upgrading).upgrade(keys.read);
            let upgraded_writer =
                std::mem::replace(writer, HapWriter::Upgrading).upgrade(keys.write);
            *reader = upgraded_reader;
            *writer = upgraded_writer;
            conn.controller_id = Some(controller_id);
            tracing::info!("hap session encrypted; switched to control channel");
            Ok(())
        }
    }
}

fn build_event_body(changes: &[CharacteristicEvent], subs: &Subscriptions) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for event in changes {
        if subs.contains(&event.id) {
            out.push(serde_json::json!({
                "aid": event.id.aid.0,
                "iid": event.id.iid.0,
                "value": event.value.clone(),
            }));
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

async fn handle_get_characteristics(app: &impl HapAccessoryApp, ids: &str) -> Result<Vec<u8>> {
    let ids = parse_characteristic_ids(ids);
    let values = app.read_characteristics(&ids).await?;
    Ok(characteristics_body(values))
}

// HAP §5.10 pair-add / §5.11 pair-remove / §5.12 pair-list. We only ship
// remove for now (it's the one iOS triggers when the user deletes the bridge
// from the Home app); add/list reply with kTLVError_Unavailable.
const PAIRING_METHOD_ADD: u8 = 3;
const PAIRING_METHOD_REMOVE: u8 = 4;
const PAIRING_METHOD_LIST: u8 = 5;

async fn handle_pairings<A, S>(
    ctx: &HapRuntime<A, S>,
    caller_id: Option<&str>,
    body: &[u8],
) -> Vec<u8>
where
    A: HapAccessoryApp,
    S: HapStore,
{
    let parsed = match ParsedTlv::parse(body) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("/pairings: malformed TLV: {e}");
            return error_tlv(2, HapError::Unknown);
        }
    };

    let method = parsed.get_u8(TlvTag::Method).unwrap_or(0xFF);

    let mut state = ctx.state.lock().await;
    let caller_admin = caller_id
        .and_then(|id| state.find_paired(id))
        .map(|c| c.admin)
        .unwrap_or(false);
    if !caller_admin {
        tracing::warn!(
            "/pairings refused: caller {:?} not an admin",
            caller_id.unwrap_or("<unknown>")
        );
        return error_tlv(2, HapError::Authentication);
    }

    match method {
        PAIRING_METHOD_REMOVE => {
            let target = match parsed.get(TlvTag::Identifier) {
                Some(b) => String::from_utf8_lossy(b).to_string(),
                None => return error_tlv(2, HapError::Unknown),
            };
            state.remove_pairing(&target);
            if let Err(e) = ctx.store.save_state(&state) {
                tracing::error!("failed to persist pair-remove: {e}");
                return error_tlv(2, HapError::Unknown);
            }
            tracing::info!(
                "pair-remove: {} removed by {} (paired={})",
                target,
                caller_id.unwrap_or("<unknown>"),
                state.paired_controllers.len()
            );
            Tlv::new().put_u8(TlvTag::State, 2).encode()
        }
        PAIRING_METHOD_ADD | PAIRING_METHOD_LIST => {
            tracing::warn!("/pairings method {method} not implemented");
            error_tlv(2, HapError::Unavailable)
        }
        other => {
            tracing::warn!("/pairings unknown method {other}");
            error_tlv(2, HapError::Unknown)
        }
    }
}

fn error_tlv(state: u8, err: HapError) -> Vec<u8> {
    Tlv::new()
        .put_u8(TlvTag::State, state)
        .put_u8(TlvTag::Error, err as u8)
        .encode()
}

async fn handle_put_characteristics(
    app: &impl HapAccessoryApp,
    body: &[u8],
    subs: &mut Subscriptions,
) -> Result<CharacteristicWriteOutcome> {
    let parsed: Value = serde_json::from_slice(body)?;
    let chars = parsed
        .get("characteristics")
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow!("missing characteristics array"))?;

    let writes = chars
        .iter()
        .map(parse_characteristic_write)
        .collect::<Vec<_>>();
    app.write_characteristics(writes, subs).await
}

fn parse_characteristic_ids(ids: &str) -> Vec<CharacteristicId> {
    ids.split(',')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let mut parts = pair.split('.');
            let aid = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let iid = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            CharacteristicId::new(aid, iid)
        })
        .collect()
}

fn parse_characteristic_write(entry: &Value) -> CharacteristicWrite {
    let aid = entry.get("aid").and_then(|v| v.as_u64()).unwrap_or(0);
    let iid = entry.get("iid").and_then(|v| v.as_u64()).unwrap_or(0);
    CharacteristicWrite {
        id: CharacteristicId::new(aid, iid),
        value: entry.get("value").cloned(),
        ev: entry.get("ev").and_then(|v| v.as_bool()),
    }
}

fn characteristics_body(reads: Vec<CharacteristicRead>) -> Vec<u8> {
    let characteristics = reads
        .into_iter()
        .map(|read| {
            if read.status == HapStatus::Success {
                serde_json::json!({
                    "aid": read.id.aid.0,
                    "iid": read.id.iid.0,
                    "value": read.value,
                })
            } else {
                serde_json::json!({
                    "aid": read.id.aid.0,
                    "iid": read.id.iid.0,
                    "status": read.status.code(),
                })
            }
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "characteristics": characteristics })
        .to_string()
        .into_bytes()
}

fn write_statuses_body(statuses: Vec<CharacteristicWriteStatus>) -> Vec<u8> {
    let characteristics = statuses
        .into_iter()
        .map(|status| {
            serde_json::json!({
                "aid": status.id.aid.0,
                "iid": status.id.iid.0,
                "status": status.status.code(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "characteristics": characteristics })
        .to_string()
        .into_bytes()
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
    Upgrading,
}

impl HapReader {
    async fn next_request(&mut self) -> Result<RawRequest> {
        match self {
            HapReader::Plain { inner, buf } => read_request_plain(inner, buf).await,
            HapReader::Encrypted(r) => read_request_encrypted(r).await,
            HapReader::Upgrading => bail!("reader temporarily unavailable during upgrade"),
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
    Upgrading,
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

    async fn write_response(
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

    async fn write_status(&mut self, status: StatusCode) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_body_filters_to_subscribed_characteristics() {
        let event = CharacteristicEvent {
            id: CharacteristicId::new(2, 9),
            value: json!(100),
        };
        let mut subs = Subscriptions::default();
        assert!(build_event_body(std::slice::from_ref(&event), &subs).is_none());

        subs.insert(event.id);
        let body = build_event_body(&[event], &subs).unwrap();
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["characteristics"][0]["aid"], 2);
        assert_eq!(parsed["characteristics"][0]["iid"], 9);
        assert_eq!(parsed["characteristics"][0]["value"], 100);
    }

    #[test]
    fn parses_characteristic_ids() {
        let ids = parse_characteristic_ids("2.9,3.10,bad");

        assert_eq!(ids[0], CharacteristicId::new(2, 9));
        assert_eq!(ids[1], CharacteristicId::new(3, 10));
        assert_eq!(ids[2], CharacteristicId::new(0, 0));
    }

    #[test]
    fn characteristics_body_uses_status_for_read_errors() {
        let body = characteristics_body(vec![CharacteristicRead::error(
            CharacteristicId::new(2, 99),
            HapStatus::ResourceDoesNotExist,
        )]);
        let parsed: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(parsed["characteristics"][0]["aid"], 2);
        assert_eq!(parsed["characteristics"][0]["iid"], 99);
        assert_eq!(
            parsed["characteristics"][0]["status"],
            HapStatus::ResourceDoesNotExist.code()
        );
        assert!(parsed["characteristics"][0].get("value").is_none());
    }

    #[test]
    fn write_statuses_body_reports_per_characteristic_status() {
        let body = write_statuses_body(vec![CharacteristicWriteStatus::error(
            CharacteristicId::new(2, 9),
            HapStatus::ReadOnly,
        )]);
        let parsed: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(parsed["characteristics"][0]["aid"], 2);
        assert_eq!(parsed["characteristics"][0]["iid"], 9);
        assert_eq!(
            parsed["characteristics"][0]["status"],
            HapStatus::ReadOnly.code()
        );
    }

    #[test]
    fn server_runtime_layer_does_not_import_somfy_modules() {
        let server =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/hap/server.rs"))
                .unwrap();

        assert!(!server.contains(concat!("crate::", "gpio")));
        assert!(!server.contains(concat!("crate::", "remote")));
        assert!(!server.contains(concat!("crate::server::", "AppState")));
    }
}
