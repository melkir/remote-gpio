//! Native HomeKit Accessory Protocol server. Replaces the Homebridge plugin.
//!
//! Phases 1–3: persistent state, mDNS discovery, pair-setup/verify, and the
//! encrypted accessory protocol for read/write of WindowCovering
//! characteristics. Event notifications land in Phase 4.
//! See `docs/HAP-PLAN.md`.

pub mod accessories;
pub mod mdns;
pub mod pair_setup;
pub mod pair_verify;
pub mod positions;
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
    let ctx = Arc::new(server::HapContext {
        state: Mutex::new(hap_state),
        app,
        positions: Mutex::new(positions::load()),
        events,
    });

    tokio::spawn(async move {
        if let Err(e) = server::serve(ctx).await {
            tracing::error!("HAP server exited: {}", e);
        }
    });
    Ok(announcement)
}
