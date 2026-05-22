//! Project-specific HomeKit accessory adapters.

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::controller::BlindController;
use crate::hap::mdns::{self, MdnsConfig};
use crate::hap::runtime::{CharacteristicEvent, HapRuntime};
use crate::hap::state::{FileHapStore, HapState};
use crate::positioning::state::PositionDelta;

mod accessory_db;
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
    hap_server: tokio::task::JoinHandle<()>,
}

impl HomekitHandles {
    pub fn abort(&self) {
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
    let app = Arc::new(somfy::SomfyHapApp::new(controller.clone()));
    let runtime = Arc::new(HapRuntime::new(hap_state, store, app, events));

    attach_hap_position_events(&controller, runtime.event_sender());

    let hap_server = tokio::spawn(async move {
        if let Err(e) = crate::hap::server::serve(runtime, HAP_PORT).await {
            tracing::error!("HAP server exited: {}", e);
        }
    });
    Ok(HomekitHandles {
        _announcement: announcement,
        hap_server,
    })
}

fn attach_hap_position_events(
    controller: &BlindController,
    event_tx: broadcast::Sender<Vec<CharacteristicEvent>>,
) {
    controller.attach_position_hook(Arc::new(move |deltas: Vec<PositionDelta>| {
        let events = somfy::position_characteristic_events(&deltas);
        if !events.is_empty() {
            tracing::debug!(count = events.len(), "hap position events published");
            let _ = event_tx.send(events);
        }
    }));
}
