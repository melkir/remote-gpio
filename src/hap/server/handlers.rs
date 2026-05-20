use anyhow::{anyhow, Result};
use http::StatusCode;
use serde_json::Value;

use super::state::ConnectionState;
use super::transport::{HapReader, HapWriter, RawRequest};
use super::types::{OutboundResponse, RequestOutcome};
use crate::hap::pair_setup::PersistPolicy;
use crate::hap::pair_verify::HandleOutcome;
use crate::hap::runtime::{
    CharacteristicEvent, CharacteristicId, CharacteristicRead, CharacteristicWrite,
    CharacteristicWriteOutcome, CharacteristicWriteStatus, HapAccessoryApp, HapRuntime, HapStatus,
    HapStore, Subscriptions,
};
use crate::hap::session::SessionKeys;
use crate::hap::tlv::{HapError, ParsedTlv, Tag as TlvTag, Tlv};

pub(super) async fn handle_request<A, S>(
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

pub(super) async fn write_request_response(
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

pub(super) fn build_event_body(
    changes: &[CharacteristicEvent],
    subs: &Subscriptions,
) -> Option<Vec<u8>> {
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

pub(crate) fn parse_characteristic_ids(ids: &str) -> Vec<CharacteristicId> {
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

pub(crate) fn characteristics_body(reads: Vec<CharacteristicRead>) -> Vec<u8> {
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

pub(crate) fn write_statuses_body(statuses: Vec<CharacteristicWriteStatus>) -> Vec<u8> {
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
