use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Duration;

use crate::cli::UpgradeChannel;
use crate::commands::doctor;
use crate::commands::install;
use crate::config;
use crate::deploy::{self, ServiceState, STAGED_DOWNLOAD};
use crate::version;

const ASSET_NAME: &str = "somfy";
const SUMS_ASSET: &str = "SHA256SUMS";
const WAIT_ACTIVE_SECS: u64 = 20;

pub async fn run(channel: UpgradeChannel, version_pin: Option<String>, check: bool) -> Result<()> {
    if !check {
        deploy::require_root("somfy upgrade")?;
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
        if decision.status == UpdateStatus::Unknown {
            bail!(
                "cannot determine whether {} is newer: {}",
                release.tag_name,
                decision.reason
            );
        }
        return Ok(());
    }

    match decision.status {
        UpdateStatus::Newer => {}
        UpdateStatus::Current => {
            println!(
                "Already at {} ({}). Nothing to do.",
                release.tag_name, decision.reason
            );
            return Ok(());
        }
        UpdateStatus::Unknown => {
            bail!(
                "cannot determine whether {} is newer: {}",
                release.tag_name,
                decision.reason
            );
        }
    }

    println!(
        "Upgrading {} → {} ({})",
        version::CRATE_VERSION,
        release.tag_name,
        decision.reason
    );

    let binary_url = asset_url(&release, ASSET_NAME)?;
    let sums_url = asset_url(&release, SUMS_ASSET).ok();

    let tmp_path = PathBuf::from(STAGED_DOWNLOAD);
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
            Some(parse_sha_for(&sums, ASSET_NAME).ok_or_else(|| {
                anyhow!("SHA256SUMS asset does not contain an entry for `{ASSET_NAME}`")
            })?)
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

    let resolved_config = config::resolve(None).context("loading config for unit refresh")?;
    let service_state = ServiceState::capture();
    let was_running = service_state.was_running();

    if let Err(e) = deploy::apply_binary_swap(
        &tmp_path,
        service_state,
        || {
            install::refresh(None, &resolved_config).context("refreshing unit")?;
            Ok(())
        },
        || async {
            let report = doctor::collect(&resolved_config, 0).await;
            if report.has_blocking_failure() {
                bail!("post-upgrade doctor reported blocking failure");
            }
            Ok(())
        },
        Duration::from_secs(WAIT_ACTIVE_SECS),
    )
    .await
    {
        eprintln!("Upgrade failed: {e:#}");
        eprintln!("Rolling back...");
        if let Err(re) = deploy::rollback_binary_swap(service_state) {
            eprintln!("Rollback error: {re:#}");
        }
        return Err(e);
    }

    if !was_running {
        println!(
            "Service was {}; upgraded binary and left somfy stopped.",
            service_state.state_label()
        );
        println!("Run `sudo systemctl start somfy` when you want to start it.");
    }

    println!("Upgrade complete.");
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    target_commitish: Option<String>,
    #[serde(default)]
    body: Option<String>,
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
        (None, UpgradeChannel::Nightly) => format!(
            "https://api.github.com/repos/{}/releases/tags/nightly",
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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum UpdateStatus {
    Newer,
    Current,
    Unknown,
}

struct Decision {
    status: UpdateStatus,
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
                    status: if latest > current {
                        UpdateStatus::Newer
                    } else {
                        UpdateStatus::Current
                    },
                    reason: format!("semver {current} vs {latest}"),
                },
                _ => Decision {
                    status: if release.tag_name != format!("v{}", version::CRATE_VERSION) {
                        UpdateStatus::Newer
                    } else {
                        UpdateStatus::Current
                    },
                    reason: "tag comparison (unparseable semver)".into(),
                },
            }
        }
        UpgradeChannel::Nightly => {
            let Some(remote_sha) = nightly_commit_sha(release) else {
                return Decision {
                    status: UpdateStatus::Unknown,
                    reason: "release metadata does not contain a commit SHA".into(),
                };
            };
            let current = version::GIT_SHA;
            Decision {
                status: if same_git_commit(remote_sha, current) {
                    UpdateStatus::Current
                } else {
                    UpdateStatus::Newer
                },
                reason: format!(
                    "git sha {} vs {}",
                    version::short_sha(),
                    remote_sha.chars().take(7).collect::<String>()
                ),
            }
        }
    }
}

fn nightly_commit_sha(release: &Release) -> Option<&str> {
    // GitHub reports the moving release's target_commitish as the branch name
    // (`main`); the release workflow writes the immutable commit into the body.
    release
        .body
        .as_deref()
        .and_then(|body| {
            body.lines().find_map(|line| {
                let candidate = line.trim().strip_prefix("Commit:")?.trim();
                is_git_sha(candidate).then_some(candidate)
            })
        })
        .or_else(|| {
            release
                .target_commitish
                .as_deref()
                .filter(|sha| is_git_sha(sha))
        })
}

fn is_git_sha(value: &str) -> bool {
    (7..=40).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn same_git_commit(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
        || left
            .get(..right.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(right))
        || right
            .get(..left.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(left))
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
    match decision.status {
        UpdateStatus::Newer => {
            println!("Newer version available. Run `sudo somfy upgrade` to apply.");
        }
        UpdateStatus::Current => println!("Up to date."),
        UpdateStatus::Unknown => println!("Unable to determine update status."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, target: Option<&str>, assets: &[(&str, &str)]) -> Release {
        Release {
            tag_name: tag.to_string(),
            target_commitish: target.map(|s| s.to_string()),
            body: None,
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
        assert_eq!(d.status, UpdateStatus::Newer, "{}", d.reason);
    }

    #[test]
    fn compare_versions_stable_same_not_newer() {
        let r = release(&format!("v{}", version::CRATE_VERSION), None, &[]);
        let d = compare_versions(UpgradeChannel::Stable, &r);
        assert_eq!(d.status, UpdateStatus::Current, "{}", d.reason);
    }

    #[test]
    fn compare_versions_stable_unparseable_falls_back() {
        let r = release("not-semver", None, &[]);
        let d = compare_versions(UpgradeChannel::Stable, &r);
        assert_eq!(d.status, UpdateStatus::Newer);
        assert!(d.reason.contains("tag comparison"));
    }

    #[test]
    fn compare_versions_nightly_differs() {
        let r = release(
            "nightly",
            Some("deadbeefcafebabe1234567890abcdef12345678"),
            &[],
        );
        let d = compare_versions(UpgradeChannel::Nightly, &r);
        assert_eq!(d.status, UpdateStatus::Newer, "{}", d.reason);
    }

    #[test]
    fn compare_versions_nightly_same_sha_not_newer() {
        let r = release("nightly", Some(version::GIT_SHA), &[]);
        let d = compare_versions(UpgradeChannel::Nightly, &r);
        assert_eq!(d.status, UpdateStatus::Current, "{}", d.reason);
    }

    #[test]
    fn compare_versions_nightly_reads_commit_from_release_body() {
        let mut r = release("nightly", Some("main"), &[]);
        r.body = Some(format!(
            "Moving prerelease from the main branch.\nCommit: {}\n",
            version::GIT_SHA
        ));

        let d = compare_versions(UpgradeChannel::Nightly, &r);

        assert_eq!(d.status, UpdateStatus::Current, "{}", d.reason);
    }

    #[test]
    fn compare_versions_nightly_without_commit_is_unknown() {
        let r = release("nightly", Some("main"), &[]);
        let d = compare_versions(UpgradeChannel::Nightly, &r);

        assert_eq!(d.status, UpdateStatus::Unknown);
        assert!(d.reason.contains("does not contain a commit SHA"));
    }

    #[test]
    fn nightly_commit_sha_rejects_non_hex_body_value() {
        let mut r = release("nightly", Some("main"), &[]);
        r.body = Some("Commit: not-a-sha".into());

        assert_eq!(nightly_commit_sha(&r), None);
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

    #[test]
    fn service_state_restarts_running_states() {
        for state in ["active", "activating", "reloading"] {
            let decision = ServiceState::from_state(state);
            assert_eq!(decision.state_label(), state);
            assert!(decision.was_running(), "expected restart for {state}");
        }
    }

    #[test]
    fn service_state_leaves_stopped_states_stopped() {
        for state in ["inactive", "failed", "deactivating", "", "not-found"] {
            let decision = ServiceState::from_state(state);
            assert!(!decision.was_running(), "expected no restart for {state:?}");
        }
    }
}
