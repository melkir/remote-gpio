use anyhow::Result;
use remote_gpio::remote::RemoteControl;
use remote_gpio::server::{serve, AppState};
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!(
                    "{}=debug,tower_http=debug,axum::rejection=trace",
                    env!("CARGO_CRATE_NAME")
                )
                .into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let remote_control = RemoteControl::new().await?;
    let shared_state = Arc::new(AppState { remote_control });

    serve(shared_state).await?;

    Ok(())
}
