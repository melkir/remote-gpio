use anyhow::Result;
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

use crate::config::ResolvedConfig;
use crate::driver::{DriverKind, RtsOptions, PIGPIOD_ADDR};
use crate::gpio::GpioOptions;
use crate::homekit::config;
use crate::systemd;
use crate::version;

pub const UNIT_PATH: &str = "/etc/systemd/system/somfy.service";
pub const BIN_PATH: &str = "/usr/local/bin/somfy";

#[derive(Copy, Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Advisory,
    Blocking,
    Unknown,
    Skipped,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub label: &'static str,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct VersionInfo {
    pub crate_version: &'static str,
    pub git_sha: &'static str,
    pub build_date: &'static str,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub schema_version: u32,
    pub version: VersionInfo,
    pub config_path: String,
    pub checks: Vec<Check>,
}

impl DoctorReport {
    pub fn has_blocking_failure(&self) -> bool {
        self.checks.iter().any(|c| c.status == Status::Blocking)
    }

    pub fn print_summary(&self) {
        println!(
            "somfy v{} (sha {}, built {})",
            self.version.crate_version,
            version::short_sha(),
            self.version.build_date
        );
        println!("Doctor summary (run `somfy doctor -v` for details):");
        let visible: Vec<&Check> = self
            .checks
            .iter()
            .filter(|c| {
                c.status != Status::Skipped
                    && c.id != "deployed_version"
                    && c.id != "configured_driver"
            })
            .collect();
        let label_width = visible.iter().map(|c| c.label.len()).max().unwrap_or(10);
        for check in &visible {
            let marker = match check.status {
                Status::Ok => "[✓]",
                Status::Advisory => "[!]",
                Status::Blocking => "[✗]",
                Status::Unknown => "[?]",
                Status::Skipped => "[-]",
            };
            match &check.detail {
                Some(d) if check.status != Status::Ok => {
                    println!(
                        "{marker} {:<width$} ({})",
                        check.label,
                        d,
                        width = label_width
                    )
                }
                _ => println!("{marker} {}", check.label),
            }
        }
        let advisory_count = self
            .checks
            .iter()
            .filter(|c| c.status == Status::Advisory)
            .count();
        let blocking_count = self
            .checks
            .iter()
            .filter(|c| c.status == Status::Blocking)
            .count();
        if blocking_count > 0 {
            println!("\n✗ {blocking_count} blocking failure(s).");
        } else if advisory_count > 0 {
            println!("\n! {advisory_count} advisory.");
        }
    }

    pub fn print_verbose(&self) {
        self.print_summary();
        println!();
        for check in &self.checks {
            println!(
                "[{}] {} ({})",
                status_str(check.status),
                check.id,
                check.label
            );
            if let Some(d) = &check.detail {
                println!("    {d}");
            }
        }
    }
}

fn status_str(s: Status) -> &'static str {
    match s {
        Status::Ok => "ok",
        Status::Advisory => "advisory",
        Status::Blocking => "blocking",
        Status::Unknown => "unknown",
        Status::Skipped => "skipped",
    }
}

pub async fn run(json: bool, verbose: bool, resolved_config: &ResolvedConfig) -> Result<()> {
    let report = collect(resolved_config, 2000).await;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if verbose {
        report.print_verbose();
    } else {
        report.print_summary();
    }
    if report.has_blocking_failure() {
        std::process::exit(1);
    }
    Ok(())
}

pub async fn collect(resolved_config: &ResolvedConfig, network_timeout_ms: u64) -> DoctorReport {
    let mut checks = Vec::new();

    checks.push(Check {
        id: "config_file",
        label: "Config",
        status: if resolved_config.file_present {
            Status::Ok
        } else {
            Status::Advisory
        },
        detail: Some(if resolved_config.file_present {
            resolved_config.path.display().to_string()
        } else {
            format!(
                "{} not found; using built-in defaults",
                resolved_config.path.display()
            )
        }),
    });

    let rendered_unit = render_expected_unit(resolved_config);

    // unit_installed
    let unit_exists = Path::new(UNIT_PATH).exists();
    checks.push(Check {
        id: "unit_installed",
        label: "Systemd unit",
        status: if unit_exists {
            Status::Ok
        } else {
            Status::Advisory
        },
        detail: if unit_exists {
            None
        } else {
            Some(format!("not installed at {UNIT_PATH}"))
        },
    });

    // unit_in_sync + exec_start_match
    let on_disk = std::fs::read_to_string(UNIT_PATH).ok();
    match (&on_disk, &rendered_unit) {
        (Some(disk), Some(expected)) => {
            let in_sync = disk.trim() == expected.trim();
            checks.push(Check {
                id: "unit_in_sync",
                label: "Unit in sync",
                status: if in_sync {
                    Status::Ok
                } else {
                    Status::Advisory
                },
                detail: if in_sync {
                    None
                } else {
                    Some("on-disk unit differs from template; run `sudo somfy install`".into())
                },
            });
            let exec_ok = exec_start_matches(disk);
            checks.push(Check {
                id: "exec_start_match",
                label: "Unit ExecStart",
                status: if exec_ok {
                    Status::Ok
                } else {
                    Status::Blocking
                },
                detail: if exec_ok {
                    None
                } else {
                    Some(format!("ExecStart does not match {} serve", BIN_PATH))
                },
            });
        }
        _ => {
            checks.push(Check {
                id: "unit_in_sync",
                label: "Unit in sync",
                status: Status::Skipped,
                detail: None,
            });
            checks.push(Check {
                id: "exec_start_match",
                label: "Unit ExecStart",
                status: Status::Skipped,
                detail: None,
            });
        }
    }

    // service_active
    let svc_state = systemd::is_active("somfy").unwrap_or_else(|_| "unknown".into());
    checks.push(Check {
        id: "service_active",
        label: "Service state",
        status: match svc_state.as_str() {
            "active" => Status::Ok,
            "inactive" | "unknown" => Status::Advisory,
            _ => Status::Advisory,
        },
        detail: Some(svc_state),
    });

    // service_user_exists + gpio_group_member
    let service_user = on_disk.as_deref().and_then(parse_service_user);
    match &service_user {
        Some(user) => {
            let user_lookup = nix::unistd::User::from_name(user).ok().flatten();
            let exists = user_lookup.is_some();
            checks.push(Check {
                id: "service_user_exists",
                label: "Service user",
                status: if exists { Status::Ok } else { Status::Blocking },
                detail: Some(if exists {
                    user.clone()
                } else {
                    format!("user `{user}` not found")
                }),
            });
            let in_group = exists && user_in_group(user, "gpio").unwrap_or(false);
            checks.push(Check {
                id: "gpio_group_member",
                label: "GPIO group",
                status: if in_group {
                    Status::Ok
                } else {
                    Status::Advisory
                },
                detail: if in_group {
                    None
                } else {
                    Some(format!("user `{user}` not in gpio group"))
                },
            });
        }
        None => {
            checks.push(Check {
                id: "service_user_exists",
                label: "Service user",
                status: Status::Skipped,
                detail: None,
            });
            checks.push(Check {
                id: "gpio_group_member",
                label: "GPIO group",
                status: Status::Skipped,
                detail: None,
            });
        }
    }

    // driver-specific hardware checks
    let configured_driver = resolved_config.config.driver;
    checks.push(Check {
        id: "configured_driver",
        label: "Driver",
        status: Status::Ok,
        detail: Some(configured_driver.to_string()),
    });
    match configured_driver {
        DriverKind::Telis => checks.push(gpio_chip_check(&resolved_config.config.gpio)),
        DriverKind::Rts => {
            checks.extend(rts_checks(&resolved_config.config.rts));
        }
        DriverKind::Fake => checks.push(Check {
            id: "gpio_chip_accessible",
            label: "GPIO",
            status: Status::Skipped,
            detail: Some("fake driver selected".into()),
        }),
    }

    // updates_available
    checks.push(updates_check(network_timeout_ms).await);

    // deployed_version (informational)
    checks.push(Check {
        id: "deployed_version",
        label: "Deployed version",
        status: Status::Ok,
        detail: Some(format!(
            "{} (sha {}, built {})",
            version::CRATE_VERSION,
            version::short_sha(),
            version::BUILD_DATE
        )),
    });

    DoctorReport {
        schema_version: 1,
        version: VersionInfo {
            crate_version: version::CRATE_VERSION,
            git_sha: version::GIT_SHA,
            build_date: version::BUILD_DATE,
        },
        config_path: resolved_config.path.display().to_string(),
        checks,
    }
}

fn render_expected_unit(resolved_config: &ResolvedConfig) -> Option<String> {
    let user = std::fs::read_to_string(UNIT_PATH)
        .ok()
        .as_deref()
        .and_then(parse_service_user)
        .or_else(|| {
            std::env::var("SUDO_USER").ok().or_else(|| {
                nix::unistd::User::from_uid(nix::unistd::Uid::current())
                    .ok()
                    .flatten()
                    .map(|u| u.name)
            })
        })?;
    Some(crate::commands::install::render_unit(
        &user,
        &format!(
            "{} --config {} serve",
            BIN_PATH,
            resolved_config.path.display()
        ),
        &resolved_config.config.gpio.chip,
        &resolved_config.config.rts.spi_device,
    ))
}

fn exec_start_matches(unit: &str) -> bool {
    unit.lines().any(|l| {
        let l = l.trim();
        let Some(rest) = l.strip_prefix("ExecStart=") else {
            return false;
        };
        let Some(rest) = rest.strip_prefix(BIN_PATH) else {
            return false;
        };
        if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
            return false;
        }
        let mut args = rest.split_whitespace();
        while let Some(arg) = args.next() {
            if arg == "serve" {
                return true;
            }
            if arg == "--config" {
                let _ = args.next();
            }
        }
        false
    })
}

fn parse_service_user(unit: &str) -> Option<String> {
    unit.lines()
        .filter_map(|l| l.trim().strip_prefix("User="))
        .next()
        .map(|s| s.trim().to_string())
}

fn user_in_group(user: &str, group: &str) -> Result<bool> {
    use nix::unistd::{Group, User};
    let g = Group::from_name(group)?.ok_or_else(|| anyhow::anyhow!("group not found"))?;
    if g.mem.iter().any(|m| m == user) {
        return Ok(true);
    }
    let u = User::from_name(user)?.ok_or_else(|| anyhow::anyhow!("user not found"))?;
    Ok(u.gid == g.gid)
}

fn gpio_chip_check(options: &GpioOptions) -> Check {
    match std::fs::OpenOptions::new().read(true).open(&options.chip) {
        Ok(_) => Check {
            id: "gpio_chip_accessible",
            label: "GPIO",
            status: Status::Ok,
            detail: Some(options.chip.clone()),
        },
        Err(e) => Check {
            id: "gpio_chip_accessible",
            label: "GPIO",
            status: Status::Blocking,
            detail: Some(format!("{}: {e}", options.chip)),
        },
    }
}

use crate::gpio::MAX_BCM_GPIO;

fn rts_checks(options: &RtsOptions) -> Vec<Check> {
    let spi_check = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&options.spi_device)
    {
        Ok(_) => Check {
            id: "rts_spi_device",
            label: "RTS SPI",
            status: Status::Ok,
            detail: Some(options.spi_device.clone()),
        },
        Err(e) => Check {
            id: "rts_spi_device",
            label: "RTS SPI",
            status: Status::Blocking,
            detail: Some(format!("{}: {e}", options.spi_device)),
        },
    };

    let gdo0_check = if options.gpio.gdo0 <= MAX_BCM_GPIO {
        Check {
            id: "rts_gdo0_gpio",
            label: "RTS GDO0",
            status: Status::Ok,
            detail: Some(format!("BCM{}", options.gpio.gdo0)),
        }
    } else {
        Check {
            id: "rts_gdo0_gpio",
            label: "RTS GDO0",
            status: Status::Blocking,
            detail: Some(format!(
                "BCM{} out of range (0..={MAX_BCM_GPIO})",
                options.gpio.gdo0
            )),
        }
    };

    let pigpiod_check = match std::net::TcpStream::connect_timeout(
        &PIGPIOD_ADDR.parse().unwrap(),
        Duration::from_millis(500),
    ) {
        Ok(_) => Check {
            id: "pigpiod",
            label: "pigpiod",
            status: Status::Ok,
            detail: Some(PIGPIOD_ADDR.to_string()),
        },
        Err(e) => Check {
            id: "pigpiod",
            label: "pigpiod",
            status: Status::Blocking,
            detail: Some(format!("{PIGPIOD_ADDR}: {e}")),
        },
    };

    let pigpiod_local_check = Check {
        id: "pigpiod_localhost_only",
        label: "pigpiod local",
        status: Status::Ok,
        detail: Some("fixed local endpoint".into()),
    };

    let state_check = rts_state_file_check();

    vec![
        spi_check,
        gdo0_check,
        pigpiod_check,
        pigpiod_local_check,
        state_check,
    ]
}

fn rts_state_file_check() -> Check {
    let path = config::state_dir().join(crate::rts::state::STATE_FILE);
    let display = path.display().to_string();
    if !path.exists() {
        return Check {
            id: "rts_state_file",
            label: "RTS state",
            status: Status::Advisory,
            detail: Some(format!("{display} not yet created")),
        };
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<crate::rts::state::RtsState>(&text) {
            Ok(state) if state.schema_version == crate::rts::state::SCHEMA_VERSION => Check {
                id: "rts_state_file",
                label: "RTS state",
                status: Status::Ok,
                detail: Some(display),
            },
            Ok(state) => Check {
                id: "rts_state_file",
                label: "RTS state",
                status: Status::Blocking,
                detail: Some(format!(
                    "{display}: schema_version {} unsupported (expected {})",
                    state.schema_version,
                    crate::rts::state::SCHEMA_VERSION
                )),
            },
            Err(e) => Check {
                id: "rts_state_file",
                label: "RTS state",
                status: Status::Blocking,
                detail: Some(format!("{display}: parse error: {e}")),
            },
        },
        Err(e) => Check {
            id: "rts_state_file",
            label: "RTS state",
            status: Status::Blocking,
            detail: Some(format!("{display}: {e}")),
        },
    }
}

async fn updates_check(timeout_ms: u64) -> Check {
    if timeout_ms == 0 {
        return Check {
            id: "updates_available",
            label: "Updates",
            status: Status::Skipped,
            detail: None,
        };
    }
    let client = match reqwest::Client::builder()
        .user_agent(format!("somfy/{}", version::CRATE_VERSION))
        .timeout(Duration::from_millis(timeout_ms))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Check {
                id: "updates_available",
                label: "Updates",
                status: Status::Advisory,
                detail: Some(format!("client error: {e}")),
            }
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
            Ok(v) => {
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
                    Check {
                        id: "updates_available",
                        label: "Updates",
                        status: Status::Advisory,
                        detail: Some(format!("v{tag} available")),
                    }
                } else {
                    Check {
                        id: "updates_available",
                        label: "Updates",
                        status: Status::Ok,
                        detail: Some(format!("up to date (v{})", version::CRATE_VERSION)),
                    }
                }
            }
            Err(e) => Check {
                id: "updates_available",
                label: "Updates",
                status: Status::Advisory,
                detail: Some(format!("parse error: {e}")),
            },
        },
        Err(e) => Check {
            id: "updates_available",
            label: "Updates",
            status: Status::Advisory,
            detail: Some(format!("network: {e}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(id: &'static str, status: Status) -> Check {
        Check {
            id,
            label: id,
            status,
            detail: None,
        }
    }

    #[test]
    fn exec_start_matches_accepts_serve() {
        let unit = "[Service]\nExecStart=/usr/local/bin/somfy serve\n";
        assert!(exec_start_matches(unit));
    }

    #[test]
    fn exec_start_matches_rejects_wrong_path() {
        let unit = "ExecStart=/opt/somfy serve\n";
        assert!(!exec_start_matches(unit));
    }

    #[test]
    fn exec_start_matches_rejects_missing_serve() {
        let unit = "ExecStart=/usr/local/bin/somfy\n";
        assert!(!exec_start_matches(unit));
    }

    #[test]
    fn exec_start_matches_allows_trailing_args() {
        let unit = "ExecStart=/usr/local/bin/somfy serve --flag\n";
        assert!(exec_start_matches(unit));
    }

    #[test]
    fn exec_start_matches_rejects_prev_suffix() {
        let unit = "ExecStart=/usr/local/bin/somfy.prev serve\n";
        assert!(!exec_start_matches(unit));
    }

    #[test]
    fn parse_service_user_extracts_name() {
        let unit = "[Service]\nUser=pi\nGroup=gpio\n";
        assert_eq!(parse_service_user(unit), Some("pi".into()));
    }

    #[test]
    fn parse_service_user_absent_returns_none() {
        let unit = "[Service]\nGroup=gpio\n";
        assert_eq!(parse_service_user(unit), None);
    }

    #[test]
    fn has_blocking_failure_detects_blocking() {
        let report = DoctorReport {
            schema_version: 1,
            version: VersionInfo {
                crate_version: "0.0.0",
                git_sha: "dev",
                build_date: "today",
            },
            config_path: "/etc/somfy/config.toml".into(),
            checks: vec![check("a", Status::Ok), check("b", Status::Blocking)],
        };
        assert!(report.has_blocking_failure());
    }

    #[test]
    fn has_blocking_failure_ignores_advisory() {
        let report = DoctorReport {
            schema_version: 1,
            version: VersionInfo {
                crate_version: "0.0.0",
                git_sha: "dev",
                build_date: "today",
            },
            config_path: "/etc/somfy/config.toml".into(),
            checks: vec![check("a", Status::Ok), check("b", Status::Advisory)],
        };
        assert!(!report.has_blocking_failure());
    }
}
