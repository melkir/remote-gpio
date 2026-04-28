use anyhow::{bail, Context, Result};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::backend::BackendKind;
use crate::commands::doctor::{BIN_PATH, UNIT_PATH};
use crate::commands::upgrade::BIN_PREV;
use crate::homekit::config;
use crate::systemd;

const UNIT_TEMPLATE: &str = include_str!("../../assets/somfy.service.tmpl");

pub fn render_unit(service_user: &str, exec_start: &str) -> String {
    UNIT_TEMPLATE
        .replace("{{SERVICE_USER}}", service_user)
        .replace("{{EXEC_START}}", exec_start)
}

pub fn run(user_override: Option<String>, backend: BackendKind) -> Result<()> {
    require_root()?;

    let service_user = resolve_service_user(user_override)?;

    let user_lookup = nix::unistd::User::from_name(&service_user)
        .with_context(|| format!("looking up user {service_user}"))?;
    let service_user_info = user_lookup
        .ok_or_else(|| anyhow::anyhow!("service user `{service_user}` does not exist"))?;

    let current_exe = std::env::current_exe()?
        .canonicalize()?
        .to_string_lossy()
        .into_owned();
    if current_exe != BIN_PATH && current_exe != BIN_PREV {
        tracing::warn!(
            "running binary is at {} but unit will ExecStart={} — ensure {} points at the intended binary",
            current_exe,
            BIN_PATH,
            BIN_PATH
        );
    }

    let rendered = render_unit(
        &service_user,
        &format!("{BIN_PATH} serve --backend {backend}"),
    );

    let unit_path = Path::new(UNIT_PATH);
    let on_disk = fs::read_to_string(unit_path).ok();

    let needs_write = match &on_disk {
        Some(existing) => existing.trim() != rendered.trim(),
        None => true,
    };

    if needs_write {
        atomic_write(unit_path, &rendered)?;
        systemd::systemctl(&["daemon-reload"])?;
        tracing::info!("wrote {}", UNIT_PATH);
    } else {
        tracing::info!("{} already in sync", UNIT_PATH);
    }

    prepare_state_dir(&service_user_info)?;
    systemd::systemctl(&["enable", "--now", "somfy"])?;
    println!("somfy installed as {service_user}, service enabled");
    Ok(())
}

pub fn installed_backend() -> BackendKind {
    fs::read_to_string(UNIT_PATH)
        .ok()
        .and_then(|unit| parse_backend_from_unit(&unit))
        .unwrap_or(BackendKind::Fake)
}

fn parse_backend_from_unit(unit: &str) -> Option<BackendKind> {
    let exec = unit
        .lines()
        .find_map(|line| line.trim().strip_prefix("ExecStart="))?;
    let mut args = exec.split_whitespace();
    while let Some(arg) = args.next() {
        if arg == "--backend" {
            return match args.next()? {
                "fake" => Some(BackendKind::Fake),
                "telis" => Some(BackendKind::Telis),
                "rts" => Some(BackendKind::Rts),
                _ => None,
            };
        }
    }
    Some(BackendKind::Fake)
}

fn require_root() -> Result<()> {
    if !nix::unistd::Uid::current().is_root() {
        bail!("somfy install must be run as root (use sudo)");
    }
    Ok(())
}

fn resolve_service_user(user_override: Option<String>) -> Result<String> {
    if let Some(u) = user_override {
        return Ok(u);
    }
    if let Ok(u) = std::env::var("SUDO_USER") {
        if !u.is_empty() && u != "root" {
            return Ok(u);
        }
    }
    bail!("cannot determine service user; pass --user <pi-user> when invoking directly as root");
}

fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("/"));
    let filename = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid unit path"))?;
    let tmp = parent.join(format!(".{}.tmp", filename.to_string_lossy()));

    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }

    fs::rename(&tmp, path)?;
    Ok(())
}

fn prepare_state_dir(user: &nix::unistd::User) -> Result<()> {
    let dir = Path::new(config::SYSTEM_STATE_DIR);
    fs::create_dir_all(dir)
        .with_context(|| format!("creating HomeKit state directory {}", dir.display()))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("setting permissions on {}", dir.display()))?;
    chown_path(dir, user).with_context(|| format!("setting owner on {}", dir.display()))?;

    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        chown_path(&path, user).with_context(|| format!("setting owner on {}", path.display()))?;
    }

    Ok(())
}

fn chown_path(path: &Path, user: &nix::unistd::User) -> Result<()> {
    nix::unistd::chown(path, Some(user.uid), Some(user.gid))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_unit_substitutes_placeholders() {
        let out = render_unit("pi", "/usr/local/bin/somfy serve --backend rts");
        assert!(out.contains("User=pi"));
        assert!(out.contains("Group=gpio"));
        assert!(out.contains("ExecStart=/usr/local/bin/somfy serve --backend rts"));
        assert!(!out.contains("{{SERVICE_USER}}"));
        assert!(!out.contains("{{EXEC_START}}"));
    }

    #[test]
    fn resolve_service_user_override_wins() {
        let u = resolve_service_user(Some("alice".into())).unwrap();
        assert_eq!(u, "alice");
    }

    #[test]
    fn resolve_service_user_uses_sudo_user() {
        // Serialize env access within this test module.
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("SUDO_USER").ok();
        std::env::set_var("SUDO_USER", "pi");
        let u = resolve_service_user(None).unwrap();
        assert_eq!(u, "pi");
        restore_env("SUDO_USER", prev);
    }

    #[test]
    fn resolve_service_user_rejects_root() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("SUDO_USER").ok();
        std::env::set_var("SUDO_USER", "root");
        assert!(resolve_service_user(None).is_err());
        restore_env("SUDO_USER", prev);
    }

    #[test]
    fn resolve_service_user_rejects_empty_and_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("SUDO_USER").ok();
        std::env::set_var("SUDO_USER", "");
        assert!(resolve_service_user(None).is_err());
        std::env::remove_var("SUDO_USER");
        assert!(resolve_service_user(None).is_err());
        restore_env("SUDO_USER", prev);
    }

    #[test]
    fn atomic_write_creates_file_with_mode_0644() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("somfy.service");
        atomic_write(&target, "hello\n").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello\n");
        let mode = fs::metadata(&target).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o644);
        let tmp_sibling = dir.path().join(".somfy.service.tmp");
        assert!(!tmp_sibling.exists());
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn restore_env(key: &str, prev: Option<String>) {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
