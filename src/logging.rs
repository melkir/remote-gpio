use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub fn init() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        format!(
            "{}=debug,tower_http=debug,axum::rejection=trace",
            env!("CARGO_CRATE_NAME")
        )
        .into()
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}
