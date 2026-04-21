use anyhow::{anyhow, bail, Context, Result};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use crate::cli::UpgradeChannel;
use crate::commands::doctor::{self, BIN_PATH};
use crate::commands::install;
use crate::systemd;
use crate::version;

const BIN_PREV: &str = "/usr/local/bin/somfy.prev";
const BIN_DIR: &str = "/usr/local/bin";
const ASSET_NAME: &str = "somfy";
const SUMS_ASSET: &str = "SHA256SUMS";
const WAIT_ACTIVE_SECS: u64 = 20;

pub async fn run(channel: UpgradeChannel, version_pin: Option<String>, check: bool) -> Result<()> {
    if !check && !nix::unistd::Uid::current().is_root() {
        bail!("somfy upgrade must be run as root (use sudo)");
    }

    let client = reqwest::Client::builder()
        .user_agent(format!("somfy/{}", version::CRATE_VERSION))
        .timeout(Duration::from_secs(30))
        .build()
        .context("building http client")?;

    let release = fetch_release(&client, channel, version_pin.as_deref()).await?;
    let decision = compare_versions(channel, &release);

    if check {
        print_check(&release, &decision);
        return Ok(());
    }

    if !decision.newer {
        println!(
            "Already at {} ({}). Nothing to do.",
            release.tag_name, decision.reason
        );
        return Ok(());
    }

    println!(
        "Upgrading {} → {} ({})",
        version::CRATE_VERSION,
        release.tag_name,
        decision.reason
    );

    let binary_url = asset_url(&release, ASSET_NAME)?;
    let sums_url = asset_url(&release, SUMS_ASSET).ok();

    let tmp_path = PathBuf::from(BIN_DIR).join(".somfy.download");
    // Wipe any leftover from a prior failed run.
    let _ = fs::remove_file(&tmp_path);

    let expected_sha = match &sums_url {
        Some(url) => {
            let sums = client
                .get(url)
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;
            parse_sha_for(&sums, ASSET_NAME)
        }
        None => None,
    };

    let actual_sha = download_with_hash(&client, &binary_url, &tmp_path).await?;

    if let Some(expected) = &expected_sha {
        if !expected.eq_ignore_ascii_case(&actual_sha) {
            let _ = fs::remove_file(&tmp_path);
            bail!("checksum mismatch: expected {expected}, got {actual_sha}");
        }
        println!("Checksum OK ({actual_sha})");
    } else {
        println!("Warning: no SHA256SUMS published; skipping checksum verification");
    }

    fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o755))?;

    // Smoke test the new binary before swapping anything live.
    let smoke = StdCommand::new(&tmp_path).arg("--version").output();
    match smoke {
        Ok(out) if out.status.success() => {
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            println!("New binary reports: {v}");
        }
        Ok(out) => {
            let _ = fs::remove_file(&tmp_path);
            bail!(
                "new binary failed smoke test: exit {:?}, stderr: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp_path);
            bail!("new binary failed to exec: {e}");
        }
    }

    // Begin the atomic swap.
    if let Err(e) = apply_swap(&tmp_path).await {
        eprintln!("Upgrade failed: {e:#}");
        eprintln!("Rolling back…");
        if let Err(re) = rollback() {
            eprintln!("Rollback error: {re:#}");
        }
        std::process::exit(1);
    }

    println!("Upgrade complete.");
    Ok(())
}

async fn apply_swap(new_bin: &Path) -> Result<()> {
    let bin_path = Path::new(BIN_PATH);
    let prev_path = Path::new(BIN_PREV);

    if bin_path.exists() {
        let _ = fs::remove_file(prev_path);
        fs::rename(bin_path, prev_path).context("moving current binary to .prev")?;
    }

    if let Err(e) = systemd::systemctl(&["stop", "somfy"]) {
        tracing::warn!("systemctl stop reported: {e}");
    }

    fs::rename(new_bin, bin_path).context("moving new binary into place")?;

    // Reconcile unit with new binary's template; picks up SUDO_USER.
    install::run(None).context("refreshing unit")?;

    systemd::systemctl(&["start", "somfy"]).context("starting somfy")?;

    wait_active(Duration::from_secs(WAIT_ACTIVE_SECS))
        .await
        .context("service did not become active")?;

    let report = doctor::collect(0).await;
    if report.has_blocking_failure() {
        bail!("post-upgrade doctor reported blocking failure");
    }
    Ok(())
}

fn rollback() -> Result<()> {
    let bin_path = Path::new(BIN_PATH);
    let prev_path = Path::new(BIN_PREV);
    let _ = systemd::systemctl(&["stop", "somfy"]);
    if prev_path.exists() {
        let _ = fs::remove_file(bin_path);
        fs::rename(prev_path, bin_path).context("restoring previous binary")?;
    }
    systemd::systemctl(&["start", "somfy"]).context("starting previous version")?;
    Ok(())
}

async fn wait_active(timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let state = systemd::is_active("somfy").unwrap_or_default();
        match state.as_str() {
            "active" => return Ok(()),
            "failed" => bail!("service entered failed state"),
            _ => {}
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for active state (last={state})");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[derive(Debug, serde::Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    target_commitish: Option<String>,
    assets: Vec<Asset>,
}

#[derive(Debug, serde::Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

async fn fetch_release(
    client: &reqwest::Client,
    channel: UpgradeChannel,
    pin: Option<&str>,
) -> Result<Release> {
    let url = match (pin, channel) {
        (Some(tag), _) => format!(
            "https://api.github.com/repos/{}/releases/tags/{}",
            version::GITHUB_REPO,
            tag
        ),
        (None, UpgradeChannel::Main) => format!(
            "https://api.github.com/repos/{}/releases/tags/main",
            version::GITHUB_REPO
        ),
        (None, UpgradeChannel::Stable) => format!(
            "https://api.github.com/repos/{}/releases/latest",
            version::GITHUB_REPO
        ),
    };
    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("fetching release")?
        .error_for_status()
        .context("GitHub release request failed")?;
    let release: Release = resp.json().await.context("parsing release JSON")?;
    Ok(release)
}

struct Decision {
    newer: bool,
    reason: String,
}

fn compare_versions(channel: UpgradeChannel, release: &Release) -> Decision {
    match channel {
        UpgradeChannel::Stable => {
            let tag = release.tag_name.trim_start_matches('v');
            match (
                semver::Version::parse(tag),
                semver::Version::parse(version::CRATE_VERSION),
            ) {
                (Ok(latest), Ok(current)) => Decision {
                    newer: latest > current,
                    reason: format!("semver {current} vs {latest}"),
                },
                _ => Decision {
                    newer: release.tag_name != format!("v{}", version::CRATE_VERSION),
                    reason: "tag comparison (unparseable semver)".into(),
                },
            }
        }
        UpgradeChannel::Main => {
            let remote_sha = release
                .target_commitish
                .as_deref()
                .or(release.name.as_deref())
                .unwrap_or("");
            let current = version::GIT_SHA;
            let newer = !remote_sha.is_empty()
                && !remote_sha.eq_ignore_ascii_case(current)
                && !remote_sha.starts_with(current)
                && !current.starts_with(remote_sha);
            Decision {
                newer,
                reason: format!(
                    "git sha {} vs {}",
                    version::short_sha(),
                    remote_sha.chars().take(7).collect::<String>()
                ),
            }
        }
    }
}

fn asset_url(release: &Release, name: &str) -> Result<String> {
    release
        .assets
        .iter()
        .find(|a| a.name == name)
        .map(|a| a.browser_download_url.clone())
        .ok_or_else(|| anyhow!("asset `{name}` not found in release {}", release.tag_name))
}

async fn download_with_hash(client: &reqwest::Client, url: &str, dest: &Path) -> Result<String> {
    let resp = client.get(url).send().await?.error_for_status()?;
    let mut stream = resp.bytes_stream();
    let mut hasher = Sha256::new();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(dest)?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        file.write_all(&chunk)?;
    }
    file.sync_all()?;
    Ok(hex::encode(hasher.finalize()))
}

fn parse_sha_for(sums: &str, asset: &str) -> Option<String> {
    for line in sums.lines() {
        let mut parts = line.split_whitespace();
        let sha = parts.next()?;
        // `sha256sum` format: "<hex>  <filename>" (with possible leading '*')
        let file = parts.next()?.trim_start_matches('*');
        if file == asset {
            return Some(sha.to_string());
        }
    }
    None
}

fn print_check(release: &Release, decision: &Decision) {
    println!(
        "Current: {} (sha {})",
        version::CRATE_VERSION,
        version::short_sha()
    );
    println!("Latest:  {}", release.tag_name);
    println!("{}", decision.reason);
    if decision.newer {
        println!("Newer version available. Run `sudo somfy upgrade` to apply.");
    } else {
        println!("Up to date.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, target: Option<&str>, assets: &[(&str, &str)]) -> Release {
        Release {
            tag_name: tag.to_string(),
            name: None,
            target_commitish: target.map(|s| s.to_string()),
            assets: assets
                .iter()
                .map(|(n, u)| Asset {
                    name: n.to_string(),
                    browser_download_url: u.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn compare_versions_stable_newer() {
        let r = release("v9.9.9", None, &[]);
        let d = compare_versions(UpgradeChannel::Stable, &r);
        assert!(d.newer, "expected newer: {}", d.reason);
    }

    #[test]
    fn compare_versions_stable_same_not_newer() {
        let r = release(&format!("v{}", version::CRATE_VERSION), None, &[]);
        let d = compare_versions(UpgradeChannel::Stable, &r);
        assert!(!d.newer, "expected not newer: {}", d.reason);
    }

    #[test]
    fn compare_versions_stable_unparseable_falls_back() {
        let r = release("not-semver", None, &[]);
        let d = compare_versions(UpgradeChannel::Stable, &r);
        assert!(d.newer);
        assert!(d.reason.contains("tag comparison"));
    }

    #[test]
    fn compare_versions_main_differs() {
        let r = release(
            "main",
            Some("deadbeefcafebabe1234567890abcdef12345678"),
            &[],
        );
        let d = compare_versions(UpgradeChannel::Main, &r);
        assert!(d.newer, "expected newer: {}", d.reason);
    }

    #[test]
    fn compare_versions_main_same_sha_not_newer() {
        let r = release("main", Some(version::GIT_SHA), &[]);
        let d = compare_versions(UpgradeChannel::Main, &r);
        assert!(!d.newer, "expected not newer: {}", d.reason);
    }

    #[test]
    fn compare_versions_main_empty_remote_not_newer() {
        let r = release("main", Some(""), &[]);
        let d = compare_versions(UpgradeChannel::Main, &r);
        assert!(!d.newer);
    }

    #[test]
    fn parse_sha_for_plain() {
        let sums = "abc123  somfy\nffeedd  other\n";
        assert_eq!(parse_sha_for(sums, "somfy"), Some("abc123".into()));
    }

    #[test]
    fn parse_sha_for_binary_mode() {
        let sums = "abc123  *somfy\n";
        assert_eq!(parse_sha_for(sums, "somfy"), Some("abc123".into()));
    }

    #[test]
    fn parse_sha_for_missing() {
        assert_eq!(parse_sha_for("abc  other\n", "somfy"), None);
        assert_eq!(parse_sha_for("", "somfy"), None);
    }

    #[test]
    fn asset_url_picks_matching_name() {
        let r = release(
            "v1.0.0",
            None,
            &[
                ("somfy", "https://example/somfy"),
                ("SHA256SUMS", "https://example/sums"),
            ],
        );
        assert_eq!(asset_url(&r, "somfy").unwrap(), "https://example/somfy");
        assert_eq!(asset_url(&r, "SHA256SUMS").unwrap(), "https://example/sums");
    }

    #[test]
    fn asset_url_missing_errors() {
        let r = release("v1.0.0", None, &[]);
        assert!(asset_url(&r, "somfy").is_err());
    }
}
