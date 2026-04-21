use anyhow::Result;
use std::sync::Arc;

use crate::commands::doctor;
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
    serve(shared_state).await
}
