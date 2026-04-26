use anyhow::Result;
use std::sync::Arc;

use crate::commands::doctor;
use crate::hap;
use crate::remote::RemoteControl;
use crate::server::{serve, AppState};

pub async fn run() -> Result<()> {
    let report = doctor::collect(2000).await;
    report.print_summary();
    if report.has_blocking_failure() {
        std::process::exit(1);
    }

    let remote_control = RemoteControl::new().await?;
    let shared_state = Arc::new(AppState { remote_control });

    let _hap_announcement = match hap::start(shared_state.clone()).await {
        Ok(a) => Some(a),
        Err(e) => {
            tracing::warn!("HAP subsystem failed to start, continuing without HomeKit: {}", e);
            None
        }
    };

    tokio::select! {
        res = serve(shared_state) => res,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received Ctrl-C, shutting down");
            Ok(())
        }
    }
}
