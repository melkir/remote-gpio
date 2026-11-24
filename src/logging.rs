use tracing_subscriber::filter::Targets;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Initialise logging from `RUST_LOG`: a default level and/or comma-separated
/// `target=level` pairs, e.g. `info` or `somfy=debug,tower_http=warn`.
///
/// This is [`Targets`] rather than `EnvFilter`: the latter also understands
/// span-field filters, and pays for them with a regex engine in the binary.
pub fn init() {
    tracing_subscriber::registry()
        .with(filter_from(std::env::var("RUST_LOG").ok().as_deref()))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}

/// Parse a `RUST_LOG` value, falling back to the built-in default when it is
/// unset, empty, or malformed.
fn filter_from(spec: Option<&str>) -> Targets {
    spec.filter(|spec| !spec.trim().is_empty())
        .and_then(|spec| spec.parse().ok())
        .unwrap_or_else(default_filter)
}

fn default_filter() -> Targets {
    use tracing::Level;
    Targets::new()
        .with_target(env!("CARGO_CRATE_NAME"), Level::DEBUG)
        .with_target("tower_http", Level::DEBUG)
        .with_target("axum::rejection", Level::TRACE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::Level;

    #[test]
    fn bare_level_sets_the_default_for_every_target() {
        let filter = filter_from(Some("info"));
        assert!(filter.would_enable("somfy::server", &Level::INFO));
        assert!(filter.would_enable("mdns_sd", &Level::INFO));
        assert!(!filter.would_enable("somfy::server", &Level::DEBUG));
    }

    #[test]
    fn per_target_levels_override_the_default() {
        let filter = filter_from(Some("warn,somfy=debug"));
        assert!(filter.would_enable("somfy::hap", &Level::DEBUG));
        assert!(!filter.would_enable("tower_http", &Level::INFO));
        assert!(filter.would_enable("tower_http", &Level::WARN));
    }

    #[test]
    fn unset_empty_or_malformed_falls_back_to_the_default() {
        for spec in [None, Some(""), Some("somfy=notalevel")] {
            let filter = filter_from(spec);
            assert!(filter.would_enable("somfy::server", &Level::DEBUG));
            assert!(filter.would_enable("axum::rejection", &Level::TRACE));
            assert!(!filter.would_enable("mdns_sd", &Level::ERROR));
        }
    }
}
