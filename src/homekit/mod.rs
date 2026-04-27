//! Project-specific HomeKit accessory adapters.

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::hap::mdns::{self, MdnsConfig};
use crate::hap::runtime::HapRuntime;
use crate::hap::state::{FileHapStore, HapState};
use crate::remote::RemoteControl;

pub mod config;
pub mod positions;
pub mod somfy;

pub fn store() -> FileHapStore {
    FileHapStore::new(config::state_dir())
}

pub fn setup_uri(state: &HapState) -> Result<String> {
    crate::hap::qr::setup_uri(state, config::HAP_CATEGORY)
}

/// Boot the project HomeKit subsystem. Loads or initializes persistent HAP
/// state, prints the setup code, advertises via mDNS, and serves the HAP TCP
/// port. Returns the mDNS announcement guard — drop it to stop advertising.
pub async fn start(remote_control: Arc<RemoteControl>) -> Result<mdns::Announcement> {
    let store = store();
    let hap_state = store.load_or_init()?;
    let setup_uri = setup_uri(&hap_state)?;
    mdns::log_setup_payload(&hap_state, config::HAP_PORT, &setup_uri);
    let announcement = mdns::announce(
        &hap_state,
        &MdnsConfig {
            name_prefix: config::MDNS_NAME_PREFIX,
            model: config::MODEL,
            category: config::HAP_CATEGORY,
            port: config::HAP_PORT,
        },
    )?;

    let (events, _) = broadcast::channel(64);
    let position_rx = remote_control.subscribe_positions();
    let app = Arc::new(somfy::SomfyHapApp::new(remote_control));
    let runtime = Arc::new(HapRuntime::new(hap_state, store, app.clone(), events));

    let event_tx = runtime.event_sender();
    tokio::spawn(async move {
        app.run_position_listener(event_tx, position_rx).await;
    });

    tokio::spawn(async move {
        if let Err(e) = crate::hap::server::serve(runtime, config::HAP_PORT).await {
            tracing::error!("HAP server exited: {}", e);
        }
    });
    Ok(announcement)
}
