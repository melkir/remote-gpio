//! Native HomeKit Accessory Protocol server. Replaces the Homebridge plugin.
//! See `docs/HAP-PLAN.md` for the phased rollout.

pub mod mdns;
pub mod pair_setup;
pub mod pair_verify;
pub mod qr;
pub mod runtime;
pub mod server;
pub mod session;
pub mod srp;
pub mod state;
pub mod tlv;
