# AGENTS.md

Guidance for Codex working in this repo. User-facing docs live in [README.md](README.md).

## Project

`somfy` is a Rust + Preact app that drives Somfy blinds from a Raspberry Pi via one of three swappable drivers (`fake` / `telis` / `rts`), selected at runtime by `/etc/somfy/config.toml`. It ships as a single self-managing binary with clap subcommands; the Preact frontend is embedded at build time via `rust-embed`.

## Commands

Use the repo-level `mise` tasks as the primary surface:

```bash
mise tasks         # source of truth for day-to-day commands
mise install       # provisions rust (armv7 target), bun, zig, cargo-zigbuild
mise run dev       # server + frontend dev loop
mise run check     # fmt, lint, typecheck, clippy, test
mise run cross-build  # armv7 release build
```

Reach for raw `cargo`/`bun` only when a subproject-level operation isn't modeled as a task.

## Repo Layout

- `src/cli.rs` — clap subcommands. Default is `serve`. Per-command logic under `src/commands/`.
- `src/config.rs` — TOML config (`/etc/somfy/config.toml`): selects the driver and supplies its options. `validate()` is the single gate.
- `src/server.rs` — Axum routes (`/events`, `/ws`, `/command`, `/channel`, embedded static files).
- `src/remote.rs` — `RemoteControl` state engine; broadcasts the selected `Channel` via `watch::channel` (the legacy `led` payload was removed).
- `src/driver/` — driver abstraction. `CommandRouter` / `DriverExecutor` dispatch to one of `FakeDriver` / `TelisDriver` / `RtsDriver` (all three are always compiled into the binary; selection is purely runtime via config). When the `rts` driver is selected and `telis.gpio.prog` is set, `TelisProgrammer` handles `Prog` via the wired Telis remote.
- `src/rts/` — RTS protocol: `frame` (encode + obfuscation), `waveform` (pulse builder, fixed 4 frames, patent-derived 1280µs bit period), `state` (per-channel rolling code, atomic write-ahead reserve at `$STATE_DIRECTORY/rts.json`), `pigpio` (TCP client to `pigpiod`, loopback-only), `cc1101` (SPI driver for OOK @ 433.42 MHz).
- `src/gpio.rs` — `gpiocdev` wrapper used by the Telis driver. Output pulses are 60ms active-low; input debounce uses a 300ms edge-count window. Hosts the shared `MAX_BCM_GPIO` constant.
- `build.rs` + `vergen` — embeds git SHA and build date at compile time.
- `app/` — Preact PWA. Vite + Tailwind. React imports aliased to `preact/compat` in `tsconfig.json` and `vite.config.ts`.
- `src/hap/` — native HomeKit Accessory Protocol server on port 5010 (mDNS advert, SRP-6a/SHA-512 pair-setup, ChaCha20-Poly1305 session, accessory db, EVENT push). State at `$STATE_DIRECTORY/{hap.json,positions.json}`. Replaces the prior `homebridge/` plugin.

## Key Patterns

- **Error handling:** `anyhow::Result<T>` throughout the Rust code.
- **Driver seam:** all hardware lives behind `CommandRouter`. The HTTP/SSE/WS/HomeKit surfaces never branch on driver kind — they call `execute` / `execute_on`. New transports go through `CommandRouter`; new hardware goes behind a new `DriverExecutor` variant. There are no Cargo feature gates — every driver is always compiled in, and `cfg(target_os = "linux")` is the only platform gate (used to swap real hardware code for stubs on macOS dev builds).
- **Channel, not LED, on the wire:** SSE/WS payloads and the `/command` POST identify targets by `channel` (`L1`–`L4` / `ALL`). Any reference to `led` is legacy and rejected by the server.
- **RTS rolling-code safety:** `RtsStateStore` reserves a block of codes ahead of transmit and only commits on success; the file is rewritten via tmp + atomic rename + fsync. A crash mid-transmit may burn up to `DEFAULT_RESERVE_SIZE` codes per channel — that's intentional and within the receiver window.
- **pigpiod is loopback-only, hard.** `RtsDriver::new` and the doctor probe both reject non-loopback `pigpiod_addr` (`require_loopback`). pigpiod is unauthenticated; treat any non-loopback config as a security bug, not a preference.
- **RTS waveform timings are fixed.** `FRAME_COUNT = 4` and the timing constants in `src/rts/waveform.rs` are derived from the Somfy patent (US7860481 B2: 1280µs bit period). They are not user-configurable; do not re-add a `frame_count` knob.
- **Live web transports:** the Preact PWA uses `GET /events` for SSE state and `POST /command` for actions; `/ws` remains as the bidirectional API client transport.
- **Static serving:** release builds embed `app/dist/`; debug builds read from disk for hot-reload.
- **Doctor is the source of truth** for "is this thing healthy" — `somfy doctor` runs on every `serve` startup and is the single JSON contract for deployment/process health (unit drift, GPIO access, service user/group, available updates, deployed SHA, plus per-driver probes). HomeKit pairing lifecycle belongs to `somfy homekit ...`.
- **Install/upgrade are idempotent.** `somfy install` defaults the service user from `SUDO_USER`, only writes the unit if it differs from the template, and (when the resolved driver is `rts`) installs/configures `pigpiod` for loopback. `somfy upgrade` downloads, checksums, swaps, restarts, and rolls back to `somfy.prev` if the new binary fails to come up.

## CI / Deployment

One workflow at `.github/workflows/release.yml`. Pushes to `main` refresh a moving `main` prerelease; tag pushes (`v*`) publish a stable release + `SHA256SUMS`. CI never touches the Pi — the device pulls updates with `sudo somfy upgrade`. See [README.md](README.md) for the operator-facing flow.
