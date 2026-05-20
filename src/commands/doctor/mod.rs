mod check;
mod hardware;
mod systemd;
mod updates;

use anyhow::Result;
use check::Check;
use serde::Serialize;

use crate::config::ResolvedConfig;
use crate::driver::DriverKind;
use crate::version;

pub use crate::deploy::{BIN_PATH, UNIT_PATH};

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

    checks.push(systemd::config_file(resolved_config));

    let rendered_unit = systemd::render_expected_unit(resolved_config);
    checks.push(systemd::unit_installed());

    let on_disk = std::fs::read_to_string(UNIT_PATH).ok();
    match (&on_disk, &rendered_unit) {
        (Some(disk), Some(expected)) => {
            checks.push(systemd::unit_in_sync(disk, expected));
            checks.push(systemd::exec_start_match(disk));
        }
        _ => {
            checks.push(Check::new("unit_in_sync", "Unit in sync").skipped());
            checks.push(Check::new("exec_start_match", "Unit ExecStart").skipped());
        }
    }

    checks.push(systemd::service_active());

    match on_disk.as_deref().and_then(systemd::parse_service_user) {
        Some(user) => {
            let (user_check, exists) = systemd::service_user(&user);
            checks.push(user_check);
            checks.push(systemd::gpio_group_member(&user, exists));
        }
        None => {
            checks.push(Check::new("service_user_exists", "Service user").skipped());
            checks.push(Check::new("gpio_group_member", "GPIO group").skipped());
        }
    }

    let configured_driver = resolved_config.config.driver;
    checks.push(Check::new("configured_driver", "Driver").detail(configured_driver.to_string()));
    match configured_driver {
        DriverKind::Telis => checks.push(hardware::gpio_chip(&resolved_config.config.gpio)),
        DriverKind::Rts => checks.extend(hardware::rts_checks(&resolved_config.config.rts)),
        DriverKind::Fake => checks.push(hardware::fake_gpio_skipped()),
    }

    checks.push(updates::check(network_timeout_ms).await);

    checks.push(
        Check::new("deployed_version", "Deployed version").detail(format!(
            "{} (sha {}, built {})",
            version::CRATE_VERSION,
            version::short_sha(),
            version::BUILD_DATE
        )),
    );

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

#[cfg(test)]
mod tests {
    use super::*;

    fn check(id: &'static str, status: Status) -> Check {
        Check::new(id, id).status(status)
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
