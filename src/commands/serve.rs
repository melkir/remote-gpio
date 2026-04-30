use anyhow::Result;
use std::sync::Arc;

use crate::commands::doctor;
use crate::config::ResolvedConfig;
use crate::homekit;
use crate::remote::RemoteControl;
use crate::server::{serve, AppState};

pub async fn run(resolved_config: ResolvedConfig) -> Result<()> {
    let report = doctor::collect(&resolved_config, 2000).await;
    report.print_summary();
    if report.has_blocking_failure() {
        std::process::exit(1);
    }

    let remote_control =
        Arc::new(RemoteControl::with_backend(resolved_config.config.backend_config()).await?);
    let shared_state = Arc::new(AppState {
        remote_control: remote_control.clone(),
    });

    let _hap_announcement = match homekit::start(remote_control).await {
        Ok(a) => Some(a),
        Err(e) => {
            tracing::warn!(
                "HAP subsystem failed to start, continuing without HomeKit: {}",
                e
            );
            None
        }
    };

    tokio::select! {
        res = serve(shared_state) => res,
        sig = wait_for_shutdown() => {
            tracing::info!("received {sig}, shutting down");
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
