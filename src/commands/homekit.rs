use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::cli::HomekitCommand;
use crate::hap::qr;
use crate::hap::runtime::HapStore;
use crate::hap::state::display_setup_code;
use crate::homekit::{self, config};

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
    let store = homekit::store();
    let state = store.load_or_init().with_context(|| {
        format!(
            "HomeKit status needs writable state at {}. Run `sudo somfy install` first, or set SOMFY_STATE_DIR to a writable directory.",
            store.state_path().display()
        )
    })?;
    let uri = homekit::setup_uri(&state)?;

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
        port: config::HAP_PORT,
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
        "  config     : {}/{}",
        report.config_number, report.state_number
    );

    if !report.paired {
        println!();
        println!("{}", qr::render_terminal(&uri)?);
    }

    Ok(())
}

fn reset() -> Result<()> {
    let state = homekit::store().reset()?;
    println!("HomeKit state reset");
    println!("  device id  : {}", state.device_id);
    println!("  setup id   : {}", state.setup_id);
    println!("  setup code : {}", display_setup_code(&state.setup_code));
    println!();
    println!("Run `sudo somfy restart` to apply the new HomeKit identity.");
    Ok(())
}

fn pairings(json: bool) -> Result<()> {
    let Some(state) = homekit::store().load_state()? else {
        if json {
            println!("[]");
        } else {
            println!("No HomeKit state found. Run `somfy homekit status` after install to initialize pairing.");
        }
        return Ok(());
    };
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
    let store = homekit::store();
    let Some(mut state) = store.load_state()? else {
        bail!("no HomeKit state found; nothing is paired");
    };
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
