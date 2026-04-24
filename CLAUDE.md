# CLAUDE.md

Guidance for Claude Code working in this repo. User-facing docs live in [README.md](README.md).

## Project

`somfy` is a Rust + Preact app that controls a Raspberry Pi-attached Somfy Telis 4 remote. It ships as a single self-managing binary with clap subcommands; the Preact frontend is embedded at build time via `rust-embed`.

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
- `src/server.rs` — Axum routes (`/ws`, `/command`, `/led`, embedded static files).
- `src/remote.rs` — `RemoteControl` state engine; broadcasts LED state via `watch::channel`.
- `src/gpio.rs` — `gpiocdev` wrapper. Output pulses are 60ms active-low; input debounce uses a 300ms edge-count window.
- `build.rs` + `vergen` — embeds git SHA and build date at compile time.
- `app/` — Preact PWA. Vite + Tailwind. React imports aliased to `preact/compat` in `tsconfig.json` and `vite.config.ts`.

## Key Patterns

- **Error handling:** `anyhow::Result<T>` throughout the Rust code.
- **WebSocket loop:** single `tokio::select!` handles incoming messages and LED updates; command processing is spawned so it can't block broadcasts.
- **Static serving:** release builds embed `app/dist/`; debug builds read from disk for hot-reload.
- **Doctor is the source of truth** for "is this thing healthy" — `somfy doctor` runs on every `serve` startup and is the single JSON contract for health (unit drift, GPIO access, service user/group, available updates, deployed SHA).
- **Install/upgrade are idempotent.** `somfy install` defaults the service user from `SUDO_USER` and only writes the unit if it differs from the template. `somfy upgrade` downloads, checksums, swaps, restarts, and rolls back to `somfy.prev` if the new binary fails to come up.

## CI / Deployment

One workflow at `.github/workflows/release.yml`. Pushes to `main` refresh a moving `main` prerelease; tag pushes (`v*`) publish a stable release + `SHA256SUMS`. CI never touches the Pi — updates happen via `ssh pi sudo somfy upgrade`. See [README.md](README.md) for the operator-facing flow.
