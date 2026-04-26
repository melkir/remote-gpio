//! Native HomeKit Accessory Protocol server. Replaces the Homebridge plugin.
//! See `docs/HAP-PLAN.md` for the phased rollout.

pub mod accessories;
pub mod mdns;
pub mod pair_setup;
pub mod pair_verify;
pub mod positions;
pub mod qr;
pub mod server;
pub mod session;
pub mod srp;
pub mod state;
pub mod tlv;

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

use crate::server::AppState;

/// Boot the HAP subsystem. Loads or initializes persistent state, prints the
/// setup code, advertises via mDNS, and serves the HAP TCP port. Returns the
/// mDNS announcement guard — drop it to stop advertising.
pub async fn start(app: Arc<AppState>) -> Result<mdns::Announcement> {
    let hap_state = state::load_or_init()?;
    mdns::log_setup_payload(&hap_state);
    let announcement = mdns::announce(&hap_state, state::HAP_PORT)?;

    let (events, _) = broadcast::channel(64);
    let position_rx = app.remote_control.subscribe_positions();
    let ctx = Arc::new(server::HapContext {
        state: Mutex::new(hap_state),
        app,
        positions: Mutex::new(positions::load()),
        events,
    });

    let listener_ctx = ctx.clone();
    tokio::spawn(async move {
        server::run_position_listener(listener_ctx, position_rx).await;
    });

    tokio::spawn(async move {
        if let Err(e) = server::serve(ctx).await {
            tracing::error!("HAP server exited: {}", e);
        }
    });
    Ok(announcement)
}
