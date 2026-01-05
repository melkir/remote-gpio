# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

RemoteGPIO is a Rust + Preact application that controls a Raspberry Pi-attached Somfy Telis 4 remote over WebSocket and HTTP for managing window blinds/shutters through hardware GPIO pins.

## Build & Development Commands

### Rust Backend
```bash
# Standard build
cargo build --release

# Cross-compile for Raspberry Pi (armv7 with glibc 2.31)
cargo zigbuild --release --target armv7-unknown-linux-gnueabihf.2.31
```

Cross-compilation requires `zig` and `cargo-zigbuild`:
```bash
brew install zig
cargo install cargo-zigbuild
```

### Frontend (Preact/Vite)
```bash
cd app && bun run dev      # Development server (port 5173)
cd app && bun run build    # Production build to app/dist/
cd app && bun run preview  # Preview production build
```

### Lint & Format
```bash
cd app && npx biome check --apply src/
```

### Deployment
```bash
./remote-gpio.sh build    # Builds frontend + cross-compiles Rust
./remote-gpio.sh start    # Builds, deploys to Pi, runs interactively
./remote-gpio.sh delete   # Cleans up remote directory on Pi
```

Configure `RASPBERRY_PI_IP` and `REMOTE_DIR` in `remote-gpio.sh`.

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
- **Static Serving:** Backend serves frontend from `dist/` directory
