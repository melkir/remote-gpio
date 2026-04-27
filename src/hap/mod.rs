//! Native HomeKit Accessory Protocol server. Replaces the Homebridge plugin.
//! See `docs/HAP-PLAN.md` for the phased rollout.

pub mod mdns;
pub mod pair_setup;
pub mod pair_verify;
pub mod qr;
pub mod runtime;
pub mod server;
pub mod session;
pub mod srp;
pub mod state;
pub mod tlv;

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::hap::runtime::HapRuntime;
use crate::homekit::somfy::SomfyHapApp;
use crate::remote::RemoteControl;

/// Boot the HAP subsystem. Loads or initializes persistent state, prints the
/// setup code, advertises via mDNS, and serves the HAP TCP port. Returns the
/// mDNS announcement guard — drop it to stop advertising.
pub async fn start(remote_control: Arc<RemoteControl>) -> Result<mdns::Announcement> {
    let store = state::FileHapStore::current();
    let hap_state = store.load_or_init()?;
    mdns::log_setup_payload(&hap_state);
    let announcement = mdns::announce(&hap_state, state::HAP_PORT)?;

    let (events, _) = broadcast::channel(64);
    let position_rx = remote_control.subscribe_positions();
    let app = Arc::new(SomfyHapApp::new(remote_control));
    let runtime = Arc::new(HapRuntime::new(hap_state, store, app.clone(), events));

    let event_tx = runtime.event_sender();
    tokio::spawn(async move {
        app.run_position_listener(event_tx, position_rx).await;
    });

    tokio::spawn(async move {
        if let Err(e) = server::serve(runtime).await {
            tracing::error!("HAP server exited: {}", e);
        }
    });
    Ok(announcement)
}
