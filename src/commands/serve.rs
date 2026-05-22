use anyhow::{bail, Result};
use std::sync::Arc;

use crate::commands::doctor;
use crate::config::ResolvedConfig;
use crate::controller::BlindController;
use crate::homekit;
use crate::server::{serve, AppState};

pub async fn run(resolved_config: ResolvedConfig) -> Result<()> {
    let report = doctor::collect(&resolved_config, 0).await;
    report.print_summary();
    if report.has_blocking_failure() {
        bail!("doctor reported blocking failures; refusing to start");
    }

    let controller = Arc::new(
        BlindController::with_driver(
            resolved_config.config.driver_config(),
            resolved_config.config.positioning.clone(),
        )
        .await?,
    );
    let shared_state = Arc::new(AppState::new(controller.clone()));

    let hap_handles = if resolved_config.config.homekit {
        match homekit::start(controller.clone()).await {
            Ok(handles) => Some(handles),
            Err(e) => {
                tracing::warn!(
                    "HAP subsystem failed to start, continuing without HomeKit: {}",
                    e
                );
                None
            }
        }
    } else {
        None
    };

    tokio::select! {
        res = serve(shared_state) => res,
        sig = wait_for_shutdown() => {
            tracing::info!("received {sig}, shutting down");
            if let Some(handles) = hap_handles {
                handles.abort();
            }
            Ok(())
        }
    }
}

/// Resolves to a human-readable signal name when SIGINT or SIGTERM fires.
/// SIGTERM is what systemd sends on `systemctl stop somfy`.
async fn wait_for_shutdown() -> &'static str {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("failed to install SIGTERM handler: {e}");
            tokio::signal::ctrl_c().await.ok();
            return "SIGINT";
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => "SIGINT",
        _ = term.recv() => "SIGTERM",
    }
}
