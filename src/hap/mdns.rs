use anyhow::{Context, Result};
use base64::Engine;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use sha2::{Digest, Sha512};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

use crate::hap::qr;
use crate::hap::state::{display_setup_code, HapState, HAP_CATEGORY, HAP_PORT, MODEL};

/// Owns the live mDNS daemon. Dropping it stops the announcement.
pub struct Announcement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Drop for Announcement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

pub fn announce(state: &HapState, port: u16) -> Result<Announcement> {
    let daemon = ServiceDaemon::new().context("creating mDNS daemon")?;

    let service_type = "_hap._tcp.local.";
    let instance = sanitize_instance(&format!("Somfy {}", short_id(&state.device_id)));
    let host = format!("{}.local.", instance.to_lowercase().replace(' ', "-"));

    let mut props: HashMap<String, String> = HashMap::new();
    props.insert("c#".into(), state.config_number.to_string());
    props.insert("ff".into(), "0".into());
    props.insert("id".into(), state.device_id.clone());
    props.insert("md".into(), MODEL.into());
    props.insert("pv".into(), "1.1".into());
    props.insert("s#".into(), state.state_number.to_string());
    props.insert("sf".into(), state.status_flag().into());
    props.insert("ci".into(), HAP_CATEGORY.into());
    props.insert("sh".into(), setup_hash(&state.setup_id, &state.device_id));

    // mdns-sd resolves the host's interface IPs automatically when given an
    // empty address list, but we pass an unspecified placeholder for clarity.
    let placeholder: Vec<IpAddr> = vec![IpAddr::V4(Ipv4Addr::UNSPECIFIED)];
    let info = ServiceInfo::new(
        service_type,
        &instance,
        &host,
        &placeholder[..],
        port,
        props,
    )
    .context("building mDNS service info")?
    .enable_addr_auto();

    let fullname = info.get_fullname().to_string();
    daemon.register(info).context("registering mDNS service")?;

    tracing::info!(
        "HAP mDNS advertised: {} on port {} (id={}, sf={})",
        fullname,
        port,
        state.device_id,
        state.status_flag()
    );

    Ok(Announcement { daemon, fullname })
}

pub fn log_setup_payload(state: &HapState) {
    tracing::info!("┌─ HomeKit pairing");
    tracing::info!("│ setup code: {}", display_setup_code(&state.setup_code));
    tracing::info!("│ setup id  : {}", state.setup_id);
    tracing::info!("│ device id : {}", state.device_id);
    tracing::info!("│ port      : {}", HAP_PORT);
    tracing::info!("└─ paired   : {}", state.is_paired());

    if state.is_paired() {
        return;
    }
    match qr::setup_uri(state).and_then(|uri| {
        let rendered = qr::render_terminal(&uri)?;
        Ok((uri, rendered))
    }) {
        Ok((uri, rendered)) => {
            tracing::info!("HomeKit setup URI: {}", uri);
            // Bypass tracing for the QR itself — line prefixes corrupt the
            // half-block grid and break scanning.
            eprintln!("\n{}", rendered);
        }
        Err(e) => tracing::warn!("could not render HomeKit QR: {:#}", e),
    }
}

fn short_id(device_id: &str) -> String {
    device_id.chars().filter(|c| *c != ':').take(6).collect()
}

fn sanitize_instance(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == ' ' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn setup_hash(setup_id: &str, device_id: &str) -> String {
    let mut hasher = Sha512::new();
    hasher.update(setup_id.as_bytes());
    hasher.update(device_id.as_bytes());
    let digest = hasher.finalize();
    base64::engine::general_purpose::STANDARD.encode(&digest[..4])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_hash_is_base64_of_first_four_sha512_bytes() {
        assert_eq!(
            setup_hash("7OSX", "AB:CD:EF:12:34:56"),
            "L6e5JQ=="
        );
    }

    #[test]
    fn setup_hash_changes_with_setup_id_and_device_id() {
        let original = setup_hash("7OSX", "AB:CD:EF:12:34:56");

        assert_ne!(original, setup_hash("8OSX", "AB:CD:EF:12:34:56"));
        assert_ne!(original, setup_hash("7OSX", "AB:CD:EF:12:34:57"));
    }
}
