//! Somfy blind controller for Raspberry Pi.
//!
//! Drives Somfy blinds via swappable drivers (`fake`, `telis`, `rts`) selected in
//! `/etc/somfy/config.toml`. Exposes an HTTP API, WebSocket control, and optional
//! native HomeKit Accessory Protocol support.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod cli;
pub mod commands;
pub mod config;
pub mod controller;
pub mod core;
pub mod deploy;
pub mod driver;
pub mod embed;
pub mod gpio;
pub mod hap;
pub mod homekit;
pub mod logging;
pub mod persist;
pub mod rts;
pub mod server;
pub mod service;
pub mod systemd;
pub mod version;
