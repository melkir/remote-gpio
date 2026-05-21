//! Project-specific HomeKit accessory adapters.

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::controller::BlindController;
use crate::hap::mdns::{self, MdnsConfig};
use crate::hap::runtime::HapRuntime;
use crate::hap::state::{FileHapStore, HapState};

mod accessory_db;
mod position_cache;
pub mod somfy;
mod target_writes;

pub const HAP_PORT: u16 = 5010;
pub const MODEL: &str = "Somfy Telis 4";
/// Accessory Category Identifier advertised over mDNS and encoded into setup QR payloads.
pub const HAP_CATEGORY: &str = "2";
pub const MDNS_NAME_PREFIX: &str = "Somfy";

pub fn store() -> FileHapStore {
    FileHapStore::new(crate::persist::state_dir())
}

pub fn setup_uri(state: &HapState) -> Result<String> {
    crate::hap::qr::setup_uri(state, HAP_CATEGORY)
}

/// Handles for the background HomeKit tasks started by [`start`].
pub struct HomekitHandles {
    _announcement: mdns::Announcement,
    position_listener: tokio::task::JoinHandle<()>,
    hap_server: tokio::task::JoinHandle<()>,
}

impl HomekitHandles {
    pub fn abort(&self) {
        self.position_listener.abort();
        self.hap_server.abort();
    }
}

/// Boot the project HomeKit subsystem. Loads or initializes persistent HAP
/// state, prints the setup code, advertises via mDNS, and serves the HAP TCP
/// port.
pub async fn start(controller: Arc<BlindController>) -> Result<HomekitHandles> {
    let store = store();
    let hap_state = store.load_or_init()?;
    let setup_uri = setup_uri(&hap_state)?;
    mdns::log_setup_payload(&hap_state, HAP_PORT, &setup_uri);
    let announcement = mdns::announce(
        &hap_state,
        &MdnsConfig {
            name_prefix: MDNS_NAME_PREFIX,
            model: MODEL,
            category: HAP_CATEGORY,
            port: HAP_PORT,
        },
    )?;

    let (events, _) = broadcast::channel(64);
    let position_rx = controller.subscribe_positions();
    let app = Arc::new(somfy::SomfyHapApp::new(controller));
    let runtime = Arc::new(HapRuntime::new(hap_state, store, app.clone(), events));

    let event_tx = runtime.event_sender();
    let position_listener = tokio::spawn(async move {
        app.run_position_listener(event_tx, position_rx).await;
    });

    let hap_server = tokio::spawn(async move {
        if let Err(e) = crate::hap::server::serve(runtime, HAP_PORT).await {
            tracing::error!("HAP server exited: {}", e);
        }
    });
    Ok(HomekitHandles {
        _announcement: announcement,
        position_listener,
        hap_server,
    })
}
