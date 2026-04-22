# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`somfy` is a Rust + Preact application that controls a Raspberry Pi-attached Somfy Telis 4 remote over WebSocket and HTTP for managing window blinds/shutters through hardware GPIO pins. The Rust crate ships as a single self-managing binary (`somfy`) with clap subcommands; the frontend is embedded at build time.

## Build & Development Commands

### Rust Backend
```bash
# Standard build
cargo build --release

# Cross-compile for Raspberry Pi (armv7 with glibc 2.31). `hw` is required; `fake` is the default for local dev.
cargo zigbuild --release --no-default-features --features hw --target armv7-unknown-linux-gnueabihf.2.31
```

Cross-compilation requires `zig` and `cargo-zigbuild`:
```bash
brew install zig
cargo install cargo-zigbuild
```

### Frontend (Preact/Vite)
```bash
bun --cwd=app run dev      # Development server (port 5173)
bun --cwd=app run build    # Production build to app/dist/
bun --cwd=app run preview  # Preview production build
```

### Lint & Format
```bash
bun --cwd=app run lint     # oxlint
bun --cwd=app run format   # oxfmt --write
```

### Deployment

See `docs/deploy-cli.md`. CI cross-compiles for armv7 and publishes release assets; the Pi pulls updates itself:

```bash
ssh pi sudo somfy install              # idempotent: write unit, enable --now
ssh pi sudo somfy upgrade              # latest stable
ssh pi sudo somfy upgrade --channel main  # moving main-branch prerelease
ssh pi sudo somfy upgrade --version v0.2.0
ssh pi somfy doctor                    # health check; works without sudo
ssh pi somfy --version                 # embedded git SHA + build date
```

Fresh-Pi bootstrap: `curl -fsSL https://raw.githubusercontent.com/melkir/server-remote-gpio/main/install.sh | sudo bash`.

## Architecture

```
Frontend (Preact)                Backend (Axum)              Hardware (Pi GPIO)
     │                                │                            │
     │ WebSocket /ws                  │                            │
     │ POST /command ────────────────►│ server.rs                  │
     │                                │     │                      │
     │                                │     ▼                      │
     │                                │ remote.rs ◄────────────► gpio.rs
     │◄──────────────────────────────┤ (watch::channel)            │
     │ LED state broadcasts           │                            │
                                                            Output pins (pulses):
                                                            - GPIO26: UP
                                                            - GPIO19: STOP
                                                            - GPIO13: DOWN
                                                            - GPIO6:  SELECT

                                                            Input pins (LED read):
                                                            - GPIO21: L1
                                                            - GPIO20: L2
                                                            - GPIO16: L3
                                                            - GPIO12: L4
```

**Backend (Rust):** Axum server on `0.0.0.0:5002` with Tokio single-threaded runtime. Uses `gpiocdev` for GPIO control.

**Frontend (Preact):** PWA with WebSocket auto-reconnect, haptic feedback, and Tailwind CSS styling.

**State Flow:** Pi reads GPIO input states and broadcasts changes via `watch::channel` to all WebSocket clients.

## Key Patterns

- **GPIO Timing:** Output pulses are 60ms async (non-blocking); input debounce uses 300ms window
- **WebSocket:** Single `select!` loop handles LED updates and incoming messages; commands spawned to avoid blocking
- **Error Handling:** `anyhow::Result<T>` throughout Rust code
- **Preact Aliases:** React imports aliased to `preact/compat` in tsconfig and vite config
- **Static Serving:** `rust-embed` bundles `app/dist/` into the release binary; debug builds read from disk for hot-reload
- **CLI structure:** `src/cli.rs` defines clap subcommands; per-command logic lives under `src/commands/`. Default subcommand is `serve`. `somfy doctor` is the source-of-truth health check and runs on every `serve` startup.
