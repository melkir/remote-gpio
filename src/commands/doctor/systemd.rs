use std::path::Path;

use super::check::Check;
use super::{Status, BIN_PATH, UNIT_PATH};
use crate::config::ResolvedConfig;

pub fn config_file(resolved_config: &ResolvedConfig) -> Check {
    if resolved_config.file_present {
        Check::new("config_file", "Config").detail(resolved_config.path.display().to_string())
    } else {
        Check::new("config_file", "Config")
            .status(Status::Advisory)
            .detail(format!(
                "{} not found; using built-in defaults",
                resolved_config.path.display()
            ))
    }
}

pub fn unit_installed() -> Check {
    let exists = Path::new(UNIT_PATH).exists();
    Check::new("unit_installed", "Systemd unit")
        .when(exists, Status::Ok, Status::Advisory)
        .optional_detail(if exists {
            None
        } else {
            Some(format!("not installed at {UNIT_PATH}"))
        })
}

pub fn unit_in_sync(disk: &str, expected: &str) -> Check {
    let in_sync = disk.trim() == expected.trim();
    Check::new("unit_in_sync", "Unit in sync")
        .when(in_sync, Status::Ok, Status::Advisory)
        .optional_detail(if in_sync {
            None
        } else {
            Some("on-disk unit differs from template; run `sudo somfy install`".into())
        })
}

pub fn exec_start_match(disk: &str) -> Check {
    let ok = exec_start_matches(disk);
    Check::new("exec_start_match", "Unit ExecStart")
        .when(ok, Status::Ok, Status::Blocking)
        .optional_detail(if ok {
            None
        } else {
            Some(format!("ExecStart does not match {} serve", BIN_PATH))
        })
}

pub fn service_active() -> Check {
    let svc_state = crate::systemd::is_active("somfy").unwrap_or_else(|_| "unknown".into());
    let status = match svc_state.as_str() {
        "active" => Status::Ok,
        "inactive" | "unknown" => Status::Advisory,
        _ => Status::Advisory,
    };
    Check::new("service_active", "Service state")
        .status(status)
        .detail(svc_state)
}

pub fn service_user(user: &str) -> (Check, bool) {
    let user_lookup = nix::unistd::User::from_name(user).ok().flatten();
    let exists = user_lookup.is_some();
    let check = Check::new("service_user_exists", "Service user")
        .when(exists, Status::Ok, Status::Blocking)
        .detail(if exists {
            user.to_string()
        } else {
            format!("user `{user}` not found")
        });
    (check, exists)
}

pub fn gpio_group_member(user: &str, user_exists: bool) -> Check {
    let in_group = user_exists && user_in_group(user, "gpio").unwrap_or(false);
    Check::new("gpio_group_member", "GPIO group")
        .when(in_group, Status::Ok, Status::Advisory)
        .optional_detail(if in_group {
            None
        } else {
            Some(format!("user `{user}` not in gpio group"))
        })
}

pub fn render_expected_unit(resolved_config: &ResolvedConfig) -> Option<String> {
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

pub fn parse_service_user(unit: &str) -> Option<String> {
    unit.lines()
        .filter_map(|l| l.trim().strip_prefix("User="))
        .next()
        .map(|s| s.trim().to_string())
}

pub fn exec_start_matches(unit: &str) -> bool {
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

fn user_in_group(user: &str, group: &str) -> anyhow::Result<bool> {
    use nix::unistd::{Group, User};
    let g = Group::from_name(group)?.ok_or_else(|| anyhow::anyhow!("group not found"))?;
    if g.mem.iter().any(|m| m == user) {
        return Ok(true);
    }
    let u = User::from_name(user)?.ok_or_else(|| anyhow::anyhow!("user not found"))?;
    Ok(u.gid == g.gid)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
