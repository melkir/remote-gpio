//! HAP TCP server: per-connection state machine handling pair-setup,
//! pair-verify, and (post-verify) the encrypted accessory protocol.

mod handlers;
mod state;
mod transport;

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
                tracing::debug!(
                    "hap request: {} {} body={}",
                    req.method,
                    req.path,
                    format_request_body(&req.body),
                );
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

fn format_request_body(body: &[u8]) -> String {
    const MAX_JSON_BODY: usize = 2048;
    const MAX_HEX_BYTES: usize = 32;

    if body.is_empty() {
        return "<empty>".to_string();
    }

    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) {
        let mut text = value.to_string();
        if text.len() > MAX_JSON_BODY {
            text.truncate(MAX_JSON_BODY);
            text.push_str("...");
        }
        return text;
    }

    let shown = body.len().min(MAX_HEX_BYTES);
    let mut hex = String::with_capacity(shown * 2);
    for byte in &body[..shown] {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    let suffix = if body.len() > shown { "..." } else { "" };
    format!("<{} bytes non-json hex={}{}>", body.len(), hex, suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_runtime_layer_does_not_import_somfy_modules() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src/hap/server");
        let files = [
            "mod.rs",
            "transport.rs",
            "state.rs",
            "handlers/mod.rs",
            "handlers/characteristics.rs",
            "handlers/pairing.rs",
        ];
        for name in files {
            let source = std::fs::read_to_string(format!("{root}/{name}")).unwrap();
            assert!(!source.contains(concat!("crate::", "gpio")), "{name}");
            assert!(!source.contains(concat!("crate::", "remote")), "{name}");
            assert!(
                !source.contains(concat!("crate::server::", "AppState")),
                "{name}"
            );
        }
    }

    #[test]
    fn formats_empty_json_and_binary_request_bodies() {
        assert_eq!(format_request_body(b""), "<empty>");
        assert_eq!(
            format_request_body(br#"{"characteristics":[{"aid":2,"iid":10,"value":50}]}"#),
            r#"{"characteristics":[{"aid":2,"iid":10,"value":50}]}"#
        );
        assert_eq!(
            format_request_body(&[0x01, 0x02, 0xff]),
            "<3 bytes non-json hex=0102ff>"
        );
    }
}
