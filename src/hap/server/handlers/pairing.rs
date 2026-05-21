use anyhow::Result;

use super::{OutboundResponse, RequestOutcome};
use crate::hap::pair_setup::PersistPolicy;
use crate::hap::pair_verify::HandleOutcome;
use crate::hap::runtime::{HapAccessoryApp, HapRuntime, HapStore};
use crate::hap::server::state::ConnectionState;
use crate::hap::session::SessionKeys;
use crate::hap::tlv::{HapError, ParsedTlv, Tag as TlvTag, Tlv};

pub(super) async fn handle_pair_setup<A, S>(
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
    Ok(RequestOutcome::response(OutboundResponse::pairing_tlv(
        body,
    )))
}

pub(super) async fn handle_pair_verify<A, S>(
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
        HandleOutcome::Reply(body) => Ok(RequestOutcome::response(OutboundResponse::pairing_tlv(
            body,
        ))),
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

// HAP §5.10 pair-add / §5.11 pair-remove / §5.12 pair-list. We only ship
// remove for now (it's the one iOS triggers when the user deletes the bridge
// from the Home app); add/list reply with kTLVError_Unavailable.
const PAIRING_METHOD_ADD: u8 = 3;
const PAIRING_METHOD_REMOVE: u8 = 4;
const PAIRING_METHOD_LIST: u8 = 5;

pub(super) async fn handle_pairings<A, S>(
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

pub(super) fn error_tlv(state: u8, err: HapError) -> Vec<u8> {
    Tlv::new()
        .put_u8(TlvTag::State, state)
        .put_u8(TlvTag::Error, err as u8)
        .encode()
}
