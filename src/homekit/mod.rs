//! Project-specific HomeKit accessory adapters.

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::controller::BlindController;
use crate::hap::mdns::{self, MdnsConfig};
use crate::hap::runtime::{CharacteristicEvent, HapRuntime};
use crate::hap::state::{FileHapStore, HapState};
use crate::hap::{qr, server};
use crate::persist;
use crate::positioning::state::PositionDelta;

mod accessory_db;
mod characteristic;
pub mod somfy;
mod target_writes;

pub const HAP_PORT: u16 = 5010;
pub const MODEL: &str = "Somfy Telis 4";
/// Accessory Category Identifier advertised over mDNS and encoded into setup QR payloads.
pub const HAP_CATEGORY: &str = "2";
pub const MDNS_NAME_PREFIX: &str = "Somfy";

pub fn store() -> FileHapStore {
    FileHapStore::new(persist::state_dir())
}

pub fn setup_uri(state: &HapState) -> Result<String> {
    qr::setup_uri(state, HAP_CATEGORY)
}

/// Handles for the background HomeKit tasks started by [`start`].
pub struct HomekitHandles {
    _announcement: mdns::Announcement,
    hap_server: tokio::task::JoinHandle<()>,
    _position_events: tokio::task::JoinHandle<()>,
}

impl HomekitHandles {
    pub fn abort(&self) {
        self.hap_server.abort();
        self._position_events.abort();
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

    let position_events = spawn_position_events(controller, runtime.event_sender());

    let hap_server = tokio::spawn(async move {
        if let Err(e) = server::serve(runtime, HAP_PORT).await {
            tracing::error!("HAP server exited: {}", e);
        }
    });
    Ok(HomekitHandles {
        _announcement: announcement,
        hap_server,
        _position_events: position_events,
    })
}

fn spawn_position_events(
    controller: Arc<BlindController>,
    event_tx: broadcast::Sender<Vec<CharacteristicEvent>>,
) -> tokio::task::JoinHandle<()> {
    let mut position_rx = controller.subscribe_positions();
    tokio::spawn(async move {
        loop {
            let events = match position_rx.recv().await {
                Ok(deltas) => somfy::position_characteristic_events(deltas.as_ref()),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        skipped,
                        "position broadcast lagged; resyncing HAP position events from snapshot"
                    );
                    let positions = controller.position_snapshot().await;
                    let deltas: Vec<PositionDelta> = positions
                        .iter()
                        .map(|pos| PositionDelta {
                            aid: pos.aid,
                            current: Some(pos.current),
                            target: Some(pos.target),
                            status: Some(pos.status),
                        })
                        .collect();
                    somfy::position_characteristic_events(&deltas)
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };

            if !events.is_empty() {
                tracing::debug!(count = events.len(), "hap position events published");
                let _ = event_tx.send(events);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::homekit::accessory_db::IID_TARGET_POSITION;
    use crate::testing::fixtures::fake_four_blinds;

    #[tokio::test]
    async fn position_bridge_maps_deltas_to_hap_events() {
        let controller = fake_four_blinds(10).await;
        let (hap_tx, mut hap_rx) = broadcast::channel(8);
        let _bridge = spawn_position_events(controller.clone(), hap_tx);

        controller
            .set_target_positions(vec![(2, 50)])
            .await
            .unwrap();

        let events = hap_rx.recv().await.unwrap();
        assert!(!events.is_empty());
        assert!(events
            .iter()
            .any(|event| event.id.aid.0 == 2 && event.id.iid.0 == IID_TARGET_POSITION));
    }
}
