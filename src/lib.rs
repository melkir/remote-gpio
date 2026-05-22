//! Somfy blind controller for Raspberry Pi.
//!
//! Drives Somfy blinds via swappable drivers (`fake`, `telis`, `rts`) selected in
//! `/etc/somfy/config.toml`. Exposes an HTTP API, WebSocket control, and optional
//! native HomeKit Accessory Protocol support.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod cli;
pub mod commands;
pub mod config;
pub(crate) mod controller;
pub(crate) mod core;
pub(crate) mod deploy;
pub(crate) mod driver;
pub(crate) mod embed;
pub(crate) mod gpio;
pub(crate) mod hap;
pub(crate) mod homekit;
pub mod logging;
pub(crate) mod persist;
pub(crate) mod positioning;
pub(crate) mod rts;
pub(crate) mod server;
pub(crate) mod service;
pub(crate) mod systemd;
pub(crate) mod version;
