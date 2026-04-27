use anyhow::{bail, Result};
use serde::Serialize;

use crate::cli::HomekitCommand;
use crate::hap::qr;
use crate::hap::runtime::HapStore;
use crate::hap::state::{display_setup_code, FileHapStore, HAP_PORT};

#[derive(Serialize)]
struct StatusReport {
    state_path: String,
    device_id: String,
    setup_id: String,
    setup_code: String,
    setup_uri: String,
    port: u16,
    paired: bool,
    paired_controllers: usize,
    config_number: u32,
    state_number: u32,
}

#[derive(Serialize)]
struct PairingReport {
    identifier: String,
    admin: bool,
}

pub fn run(command: HomekitCommand) -> Result<()> {
    match command {
        HomekitCommand::Status { json, uri_only } => status(json, uri_only),
        HomekitCommand::Reset => reset(),
        HomekitCommand::Pairings { json } => pairings(json),
        HomekitCommand::Unpair { identifier } => unpair(&identifier),
    }
}

fn status(json: bool, uri_only: bool) -> Result<()> {
    let store = FileHapStore::current();
    let state = store.load_or_init()?;
    let uri = qr::setup_uri(&state)?;

    if uri_only {
        println!("{uri}");
        return Ok(());
    }

    let report = StatusReport {
        state_path: store.state_path().display().to_string(),
        device_id: state.device_id.clone(),
        setup_id: state.setup_id.clone(),
        setup_code: display_setup_code(&state.setup_code),
        setup_uri: uri.clone(),
        port: HAP_PORT,
        paired: state.is_paired(),
        paired_controllers: state.paired_controllers.len(),
        config_number: state.config_number,
        state_number: state.state_number,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("HomeKit status");
    println!("  paired      : {}", report.paired);
    println!("  controllers : {}", report.paired_controllers);
    println!("  device id   : {}", report.device_id);
    println!("  setup id    : {}", report.setup_id);
    println!("  setup code  : {}", report.setup_code);
    println!("  setup uri   : {}", report.setup_uri);
    println!("  port        : {}", report.port);
    println!("  state file  : {}", report.state_path);
    println!(
        "  config/state: {}/{}",
        report.config_number, report.state_number
    );

    if !report.paired {
        println!();
        println!("{}", qr::render_terminal(&uri)?);
    }

    Ok(())
}

fn reset() -> Result<()> {
    let state = FileHapStore::current().reset()?;
    println!("HomeKit state reset");
    println!("  device id  : {}", state.device_id);
    println!("  setup id   : {}", state.setup_id);
    println!("  setup code : {}", display_setup_code(&state.setup_code));
    println!();
    println!("Run `sudo somfy restart` to apply the new HomeKit identity.");
    Ok(())
}

fn pairings(json: bool) -> Result<()> {
    let state = FileHapStore::current().load_or_init()?;
    let pairings: Vec<PairingReport> = state
        .paired_controllers
        .iter()
        .map(|controller| PairingReport {
            identifier: controller.identifier.clone(),
            admin: controller.admin,
        })
        .collect();

    if json {
        println!("{}", serde_json::to_string_pretty(&pairings)?);
        return Ok(());
    }

    if pairings.is_empty() {
        println!("No paired HomeKit controllers.");
        return Ok(());
    }

    println!("HomeKit pairings");
    for pairing in pairings {
        let role = if pairing.admin { "admin" } else { "regular" };
        println!("  {} ({role})", pairing.identifier);
    }
    Ok(())
}

fn unpair(identifier: &str) -> Result<()> {
    let store = FileHapStore::current();
    let mut state = store.load_or_init()?;
    let before = state.paired_controllers.len();
    state.remove_pairing(identifier);
    if state.paired_controllers.len() == before {
        bail!("no paired HomeKit controller with identifier `{identifier}`");
    }
    store.save_state(&state)?;

    println!("Removed HomeKit pairing `{identifier}`.");
    println!("Run `sudo somfy restart` to drop any in-memory pairing state.");
    Ok(())
}
