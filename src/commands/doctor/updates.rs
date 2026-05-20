use std::time::Duration;

use super::check::Check;
use super::Status;
use crate::version;

pub async fn check(timeout_ms: u64) -> Check {
    if timeout_ms == 0 {
        return Check::new("updates_available", "Updates").skipped();
    }
    let client = match reqwest::Client::builder()
        .user_agent(format!("somfy/{}", version::CRATE_VERSION))
        .timeout(Duration::from_millis(timeout_ms))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Check::new("updates_available", "Updates")
                .status(Status::Advisory)
                .detail(format!("client error: {e}"));
        }
    };
    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        version::GITHUB_REPO
    );
    match client
        .get(&url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(v) => updates_from_release(v),
            Err(e) => Check::new("updates_available", "Updates")
                .status(Status::Advisory)
                .detail(format!("parse error: {e}")),
        },
        Err(e) => Check::new("updates_available", "Updates")
            .status(Status::Advisory)
            .detail(format!("network: {e}")),
    }
}

fn updates_from_release(v: serde_json::Value) -> Check {
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .trim_start_matches('v');
    let newer = match (
        semver::Version::parse(tag),
        semver::Version::parse(version::CRATE_VERSION),
    ) {
        (Ok(latest), Ok(current)) => latest > current,
        _ => false,
    };
    if newer {
        Check::new("updates_available", "Updates")
            .status(Status::Advisory)
            .detail(format!("v{tag} available"))
    } else {
        Check::new("updates_available", "Updates")
            .detail(format!("up to date (v{})", version::CRATE_VERSION))
    }
}
