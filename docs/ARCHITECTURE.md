# Architecture Guide

This guide explains how the codebase fits together and the tradeoffs behind the
main moving parts. It is meant for someone reading the project for the first
time.

## Mental Model

`somfy` exposes one Somfy installation through several software interfaces:

- HTTP/SSE for the web UI.
- WebSocket for bidirectional API clients.
- Native HomeKit Accessory Protocol (HAP) for Apple Home.
- A pluggable driver that talks to the actual hardware.

The frontend is intentionally driver-agnostic. Whether the binary is driving a
wired Telis 4 remote over GPIO or transmitting RTS frames through a CC1101, the
HTTP/SSE/HomeKit surface is identical.

## Drivers

`CommandRouter` (`src/driver/mod.rs`) is the single seam between the UI layer
and the hardware. Three implementations are always compiled in; selection is
purely runtime via the config file:

| Driver  | Module                | What it does                                                                                   |
| ------- | --------------------- | ---------------------------------------------------------------------------------------------- |
| `fake`  | `src/driver/fake.rs`  | Records commands in-memory; default for local dev, tests, and CI-style non-Pi builds.          |
| `telis` | `src/driver/telis.rs` | Drives the wired Telis 4 remote: GPIO output pulses + LED edge debouncing for selection.       |
| `rts`   | `src/driver/rts.rs`   | Acts as a virtual RTS remote: per-channel rolling codes + CC1101 OOK transmission via pigpiod. |

All three implement the same shape:

- `execute(command, channel?)` for stateful UI commands (Select mutates selection; directional commands target the current selection).
- `execute_on(channel, command)` for HomeKit's per-accessory commands; never mutates selection on RTS, may move the physical selector on Telis.
- `selected_channel()` / `subscribe_selected_channel()` for the live selection watch channel.

Live driver switching is unsupported within a running process — the driver is
constructed once at startup. Operators switch drivers with
`somfy config set-driver <kind>` (see [README.md](../README.md#drivers)).
When the config file is absent, Raspberry Pi Linux builds default to `telis`;
other targets default to `fake`.

## Module Map

| Area           | Files            | Responsibility                                                                                         |
| -------------- | ---------------- | ------------------------------------------------------------------------------------------------------ |
| Web API        | `src/server.rs`  | Axum HTTP, SSE, WebSocket routes and static app serving.                                               |
| Remote control | `src/remote.rs`  | Driver-agnostic command surface, position fan-out, and event broadcasting.                             |
| Drivers        | `src/driver/*`   | `fake`, `telis`, `rts` implementations of the active-driver trait.                                     |
| Telis GPIO     | `src/gpio.rs`    | Linux GPIO input/output mapping and LED debounce logic for the Telis driver.                           |
| RTS protocol   | `src/rts/*`      | RTS frame encoder, rolling-code state, waveform builder, pigpiod socket client, CC1101 driver.         |
| HAP core       | `src/hap/*`      | Generic HAP protocol pieces: TLV, SRP, pair setup/verify, session encryption, HTTP framing.            |
| HomeKit app    | `src/homekit/*`  | Somfy-specific HomeKit wiring: accessory database, target-write planning, position cache, HAP startup. |
| CLI commands   | `src/commands/*` | Install, upgrade, doctor, serve, remote, logs, config, and HomeKit commands.                           |

The important boundaries are `hap` versus `homekit` (protocol vs project), and
`driver/`\* versus everything above (hardware vs UX).

## RemoteControl

`RemoteControl` wraps `CommandRouter` and owns the cross-cutting state that
isn't driver-specific:

- `watch<Channel>` (delegated to the driver) tracks the current selection.
- `broadcast<PositionUpdate>` publishes completed Up/Down movement events.

The selection watch is stateful. A new SSE or WebSocket client subscribes and
immediately learns whether the active channel is `L1`, `L2`, `L3`, `L4`, or
`ALL`. On Telis the selection follows the physical LEDs; on RTS it is
persisted to `rts.json` and survives restart.

The position broadcast is event-like. It fires after every successful `Up` /
`Down` and carries an inferred HomeKit position (`100` for Up, `0` for Down).
When the command targets `ALL`, `complete_command` fans the update out to
`L1`–`L4` so HomeKit's per-channel position cache stays consistent with what a
physical RTS remote does over the air.

## Command Serialization

The hardware can only do one thing at a time. Software callers are more
flexible: the web UI, REST/WebSocket clients, and HomeKit can all issue
commands. Each driver serializes its own hardware transactions:

- **Telis** uses `execute_lock` so cycle-and-press sequences don't interleave at the GPIO level.
- **RTS** serializes presses through the hardware mutex inside the blocking transmitter (the radio + pigpio client live behind a single `Mutex`, run on `spawn_blocking`); the selection state lock is separate, so `select` never blocks behind an in-flight transmission.

The lock is not access control — multiple callers can submit commands. It just
guarantees coherent hardware sequences. Queueing keeps software callers simpler
than rejecting concurrent commands with a "busy" error.

## Web Flow

HTTP routes and example JSON bodies are listed in [README.md](../README.md#api).
Implementation: `src/server.rs`.

The web API is intentionally small. `/command` is the only write endpoint, and
it accepts the same shape as WebSocket command messages: a command name, an
optional `channel`, and `long: true` only for RTS pairing.

Both `GET /events` and `GET /ws` subscribe to
`RemoteControl::subscribe_selection()`, send the current channel immediately,
then forward selection changes. Incoming WebSocket commands are spawned as tasks
so updates keep flowing while a command is in flight. A per-connection semaphore
keeps those spawned commands ordered; the driver locks still protect the hardware
globally.

## HomeKit adapter

`src/homekit/mod.rs` boots mDNS, the HAP TCP server, and the position listener.
`src/homekit/somfy.rs` implements the accessory app; `target_writes.rs` batches
and coalesces `TargetPosition` writes; `position_cache.rs` persists inferred
positions. HomeKit commands go through the same `RemoteControl` / `CommandRouter`
path as the web API.

HomeKit has no direct physical position feedback from the blinds. The adapter
therefore keeps a best-effort cache: Up snaps to `100`, Down snaps to `0`, and
requested target positions snap to the nearest endpoint. `ALL` is project-level
behavior; writing it fans out to individual channels, and individual writes only
update `ALL` when every channel matches.

Protocol (pair-setup, encryption, PUT semantics, pairing commands):
[HAP.md](HAP.md).

The HAP server is not built on Axum or Hyper because pair-verify upgrades the
existing TCP socket into Apple's encrypted frame format. `src/hap/server/mod.rs`
owns that socket loop; `src/hap/server/handlers/` contains the protocol routes
so the IO loop stays readable.

## Persistence

State under `$STATE_DIRECTORY` (`/var/lib/somfy/` in production, `./hap-state` in
debug — see [HAP.md](HAP.md#persistent-state) for HomeKit files). RTS adds
`rts.json` (virtual remote IDs, rolling-code reserves, `selected_channel`).

All writes use atomic temp-file + rename. RTS uses a write-ahead reserve block
(default 16 codes) so a crash cannot roll the on-air counter backwards. Burning
a few unused codes is acceptable; replaying an old rolling code is what can
desync a motor.

## Design Tradeoffs

- **Pluggable driver:** lets the binary drive a wired Telis remote, a CC1101 radio, or nothing at all without changing the UI surface.
- **Telis driver uses real hardware as source of truth:** avoids drift for LED selection, but every command pays physical timing cost.
- **RTS driver persists rolling codes with a reserve block:** a crash loses a handful of unused codes rather than reusing one, which is the irreversible failure mode for RTS pairing.
- **Queued command execution:** lets multiple software clients use the remote without interleaving operations.
- **Inferred blind positions with `ALL` fan-out:** useful for HomeKit UX, but not true physical feedback.
- **Native HAP implementation:** removes Homebridge/Node deployment complexity, but requires maintaining protocol code.
- **pigpiod for RF timing instead of in-process sleeps:** 640 µs Manchester half-symbols don't tolerate Tokio scheduler jitter; pigpiod owns the microsecond budget.

## Good First Places To Read

1. `src/remote.rs` for the command and concurrency model.
2. `src/server.rs` for the web API, SSE stream, and WebSocket flow.
3. `src/homekit/somfy.rs` for the HomeKit adapter, then `src/homekit/target_writes.rs` for write planning.
4. `src/hap/server/mod.rs` and `src/hap/server/handlers/` for the HAP request/session loop.
5. `docs/HARDWARE.md` for wiring, pairing, and the end-to-end data-flow diagram.
6. `docs/HAP.md` for the HAP server and pairing lifecycle.
