//! HAP TCP server: per-connection state machine handling pair-setup,
//! pair-verify, and (post-verify) the encrypted accessory protocol.

mod handlers;
mod state;
mod transport;
mod types;

use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

use crate::hap::runtime::{HapAccessoryApp, HapRuntime, HapStore};
use handlers::{build_event_body, handle_request, write_request_response};
use state::ConnectionState;
use transport::{HapReader, HapWriter};

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

#[cfg(test)]
mod tests {
    use super::handlers::{
        build_event_body, characteristics_body, parse_characteristic_ids, write_statuses_body,
    };
    use crate::hap::runtime::{
        CharacteristicEvent, CharacteristicId, CharacteristicRead, CharacteristicWriteStatus,
        HapStatus, Subscriptions,
    };
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
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

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
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(parsed["characteristics"][0]["aid"], 2);
        assert_eq!(parsed["characteristics"][0]["iid"], 9);
        assert_eq!(
            parsed["characteristics"][0]["status"],
            HapStatus::ReadOnly.code()
        );
    }

    #[test]
    fn server_runtime_layer_does_not_import_somfy_modules() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src/hap/server");
        for name in ["mod.rs", "handlers.rs", "transport.rs"] {
            let source = std::fs::read_to_string(format!("{root}/{name}")).unwrap();
            assert!(!source.contains(concat!("crate::", "gpio")), "{name}");
            assert!(!source.contains(concat!("crate::", "remote")), "{name}");
            assert!(
                !source.contains(concat!("crate::server::", "AppState")),
                "{name}"
            );
        }
    }
}
