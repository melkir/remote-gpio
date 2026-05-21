mod characteristics;
mod pairing;

use anyhow::Result;
use http::StatusCode;

use crate::hap::runtime::{
    CharacteristicEvent, CharacteristicId, CharacteristicRead, CharacteristicWriteStatus,
    HapAccessoryApp, HapRuntime, HapStore, Subscriptions,
};
use crate::hap::server::state::ConnectionState;
use crate::hap::server::transport::{HapReader, HapWriter, RawRequest};
use crate::hap::session::SessionKeys;

use characteristics::{
    characteristics_body, handle_get_characteristics, handle_put_characteristics,
    parse_characteristic_ids, write_statuses_body,
};
use pairing::{handle_pair_setup, handle_pair_verify, handle_pairings};

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

pub(super) struct RequestOutcome {
    pub response: OutboundResponse,
    pub events: Vec<CharacteristicEvent>,
}

impl RequestOutcome {
    fn response(response: OutboundResponse) -> Self {
        Self {
            response,
            events: Vec::new(),
        }
    }
}

pub(super) enum OutboundResponse {
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

impl OutboundResponse {
    fn unauthorized() -> Self {
        Self::Status(StatusCode::UNAUTHORIZED)
    }

    fn hap_json(body: Vec<u8>) -> Self {
        Self::Body {
            status: StatusCode::OK,
            content_type: "application/hap+json",
            body,
        }
    }

    fn pairing_tlv(body: Vec<u8>) -> Self {
        Self::Body {
            status: StatusCode::OK,
            content_type: "application/pairing+tlv8",
            body,
        }
    }

    fn no_content() -> Self {
        Self::Status(StatusCode::NO_CONTENT)
    }

    fn multi_status(statuses: Vec<CharacteristicWriteStatus>) -> Self {
        Self::Body {
            status: StatusCode::MULTI_STATUS,
            content_type: "application/hap+json",
            body: write_statuses_body(statuses),
        }
    }
}

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
                return Ok(RequestOutcome::response(OutboundResponse::unauthorized()));
            }
            let body = serde_json::to_vec(&ctx.app.accessories().await?)?;
            Ok(RequestOutcome::response(OutboundResponse::hap_json(body)))
        }
        ("GET", "/characteristics") => {
            if !encrypted {
                return Ok(RequestOutcome::response(OutboundResponse::unauthorized()));
            }
            let ids = req.query_param("id").unwrap_or_default();
            let ids = match parse_characteristic_ids(&ids) {
                Ok(ids) => ids,
                Err(status) => {
                    let body = characteristics_body(vec![CharacteristicRead::error(
                        CharacteristicId::new(0, 0),
                        status,
                    )]);
                    return Ok(RequestOutcome::response(OutboundResponse::hap_json(body)));
                }
            };
            let body = handle_get_characteristics(ctx.app.as_ref(), &ids).await?;
            Ok(RequestOutcome::response(OutboundResponse::hap_json(body)))
        }
        ("PUT", "/characteristics") => {
            if !encrypted {
                return Ok(RequestOutcome::response(OutboundResponse::unauthorized()));
            }
            match handle_put_characteristics(ctx.app.as_ref(), &req.body, &mut conn.subs).await {
                Ok(write) => {
                    let response = if write.all_success() {
                        OutboundResponse::no_content()
                    } else {
                        OutboundResponse::multi_status(write.statuses)
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
                return Ok(RequestOutcome::response(OutboundResponse::unauthorized()));
            }
            let body = handle_pairings(ctx, conn.controller_id.as_deref(), &req.body).await;
            Ok(RequestOutcome::response(OutboundResponse::pairing_tlv(
                body,
            )))
        }
        (method, path) => {
            tracing::warn!("hap: unhandled {method} {path}");
            Ok(RequestOutcome::response(OutboundResponse::Status(
                StatusCode::NOT_FOUND,
            )))
        }
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
                std::mem::replace(reader, HapReader::Upgrading).upgrade(keys.read)?;
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

#[cfg(test)]
mod event_body_tests {
    use super::*;
    use crate::hap::runtime::CharacteristicId;
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
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["characteristics"][0]["aid"], 2);
        assert_eq!(parsed["characteristics"][0]["iid"], 9);
        assert_eq!(parsed["characteristics"][0]["value"], 100);
    }
}
