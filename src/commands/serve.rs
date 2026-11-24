use anyhow::{bail, Result};
use std::sync::Arc;
use std::time::Duration;

use crate::commands::doctor;
use crate::config::ResolvedConfig;
use crate::controller::BlindController;
use crate::homekit;
use crate::server::serve;

/// How long shutdown waits for in-flight timed moves. Well above any real
/// blind's travel time and below systemd's default 90s stop timeout.
const SHUTDOWN_MOTION_GRACE: Duration = Duration::from_secs(60);

pub async fn run(resolved_config: &ResolvedConfig) -> Result<()> {
    let report = doctor::collect(resolved_config, 0).await;
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
        res = serve(controller.clone()) => res,
        sig = wait_for_shutdown() => {
            tracing::info!("received {sig}, shutting down");
            if let Some(handles) = hap_handles {
                handles.abort();
            }
            finish_timed_motions(&controller).await;
            Ok(())
        }
    }
}

/// Let in-flight timed moves send their `stop` and persist their position
/// before exiting. A second signal skips the wait.
async fn finish_timed_motions(controller: &BlindController) {
    tokio::select! {
        finished = controller.finish_timed_motions(SHUTDOWN_MOTION_GRACE) => {
            if !finished {
                tracing::warn!(
                    "timed blind moves still running after {}s; exiting without their final stop",
                    SHUTDOWN_MOTION_GRACE.as_secs()
                );
            }
        }
        sig = wait_for_shutdown() => {
            tracing::warn!("received {sig} again; exiting without waiting for timed blind moves");
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
