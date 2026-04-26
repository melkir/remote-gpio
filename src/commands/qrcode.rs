use anyhow::Result;

use crate::hap::qr;
use crate::hap::state::{self, display_setup_code, HAP_PORT};

/// Print the HomeKit pairing QR. Shares the loader with `serve`, so running
/// this on a fresh install will provision the persistent HAP identity.
pub fn run(uri_only: bool) -> Result<()> {
    let state = state::load_or_init()?;
    let uri = qr::setup_uri(&state)?;

    if uri_only {
        println!("{}", uri);
        return Ok(());
    }

    println!("HomeKit pairing");
    println!("  setup code : {}", display_setup_code(&state.setup_code));
    println!("  setup id   : {}", state.setup_id);
    println!("  device id  : {}", state.device_id);
    println!("  port       : {}", HAP_PORT);
    println!("  paired     : {}", state.is_paired());
    println!("  uri        : {}", uri);
    if state.is_paired() {
        println!("\nAccessory is already paired — scanning the QR will be rejected.");
        return Ok(());
    }
    println!();
    println!("{}", qr::render_terminal(&uri)?);
    Ok(())
}
