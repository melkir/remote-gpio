# Architecture Guide

This guide explains how the codebase fits together and the tradeoffs behind the main moving parts. It is meant for someone reading the project for the first time.

## Mental Model

`somfy` exposes one Somfy installation through several software interfaces:

- HTTP/SSE for the web UI.
- WebSocket for bidirectional API clients.
- Native HomeKit Accessory Protocol (HAP) for Apple Home.
- A pluggable driver that talks to the actual hardware.

The frontend is intentionally driver-agnostic. Whether the binary is driving a wired Telis 4 remote over GPIO or transmitting RTS frames through a CC1101, the HTTP/SSE/HomeKit surface is identical.

## Data Flow

```
┌─────────────────────────────────────────────────────────────┐
│  FRONTEND (Preact / Vite PWA)                               │
│  EventSource (browser-managed reconnect)                    │
│  Channel indicators (L1–L4 / ALL) + Up / Stop / Down        │
└──────────────────────────┬──────────────────────────────────┘
                           │
         SSE (GET /events) + HTTP (POST /command)
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                  BACKEND (Axum / Tokio)                     │
│                                                             │
│  Routes:                                                    │
│  ├─ GET  /channel  → currently-selected channel (text)      │
│  ├─ POST /command  → execute up/down/stop/select/prog       │
│  ├─ GET  /events   → SSE: selection updates                 │
│  ├─ GET  /ws       → WebSocket: bidirectional API           │
│  └─ /*             → embedded Preact PWA                    │
│                                                             │
│  BlindService → BlindController → CommandRouter             │
│   (fake / telis / rts)                                      │
│   broadcasts the selected Channel via watch::channel        │
└──────────────────────────┬──────────────────────────────────┘
                           │
              ┌────────────┴────────────┐
              │                         │
       gpiocdev (Linux)            spidev + pigpiod
              │                         │
┌─────────────▼────────────┐ ┌──────────▼──────────────────────┐
│ Telis 4 wired remote     │ │ CC1101 OOK @ 433.42 MHz         │
│ Outputs: Up/Stop/Down/   │ │ Pi drives GDO0 with the full    │
│   Select (60 ms pulses)  │ │ Somfy pulse train (Manchester,  │
│   active-low pulses)     │ │ 640 µs half-symbols, 4 frames). │
│ Inputs: LED1–4 with      │ │ Per-channel virtual remote ID + │
│   300 ms edge debounce   │ │ rolling code in rts.json.       │
└──────────────────────────┘ └─────────────────────────────────┘
```

## Drivers

`CommandRouter` (`src/driver/mod.rs`) is the single seam between the UI layer and the hardware. Three implementations are always compiled in; selection is purely runtime via the config file:

| Driver  | Module                | What it does                                                                                   |
| ------- | --------------------- | ---------------------------------------------------------------------------------------------- |
| `fake`  | `src/driver/fake.rs`  | Records commands in-memory; default for local dev, tests, and CI-style non-Pi builds.          |
| `telis` | `src/driver/telis.rs` | Drives the wired Telis 4 remote: GPIO output pulses + LED edge debouncing for selection.       |
| `rts`   | `src/driver/rts.rs`   | Acts as a virtual RTS remote: per-channel rolling codes + CC1101 OOK transmission via pigpiod. |

All three implement the same shape:

- `execute(command, channel?)` for stateful UI commands (Select mutates selection; directional commands target the current selection).
- `execute_on(channel, command)` for HomeKit's per-accessory commands; never mutates selection on RTS, may move the physical selector on Telis.
- `selected_channel()` / `subscribe_selected_channel()` for the live selection watch channel.

Live driver switching is unsupported within a running process — the driver is constructed once at startup. Operators switch drivers through config. When the config file is absent, Raspberry Pi Linux builds default to `telis`; other targets default to `fake`.

### Driver behavior

`DriverKind::supports_pairing()` is the only driver fact adapters query today (`prog` is unavailable on Telis). `BlindService` and `somfy remote prog` call it before dispatch. Selection and `execute_on` semantics still differ per driver — see the driver bullets above and [HARDWARE.md](HARDWARE.md).

## Module Map

| Area           | Files            | Responsibility                                                                                         |
| -------------- | ---------------- | ------------------------------------------------------------------------------------------------------ |
| Domain core    | `src/core/*`     | Shared vocabulary: `Channel` (L1–L4 / ALL), `Command` (up/down/stop/select/prog).                      |
| Application    | `src/service/*`  | `BlindService`: wire-format validation and UI-style press dispatch for HTTP/WS/CLI.                    |
| Web API        | `src/server.rs`  | Axum HTTP, SSE, WebSocket routes and static app serving.                                               |
| Controller     | `src/controller.rs` | `BlindController`: hardware dispatch, position fan-out, selection/event broadcasting.              |
| Drivers        | `src/driver/*`   | `fake`, `telis`, `rts` implementations behind `CommandRouter`.                                         |
| Telis GPIO     | `src/gpio.rs`    | Linux GPIO input/output mapping and LED debounce logic for the Telis driver.                           |
| RTS protocol   | `src/rts/*`      | RTS frame encoder, rolling-code state, waveform builder, pigpiod socket client, CC1101 driver.         |
| HAP core       | `src/hap/*`      | Generic HAP protocol pieces: TLV, SRP, pair setup/verify, session encryption, HTTP framing.            |
| HomeKit app    | `src/homekit/*`  | Somfy-specific HomeKit wiring: accessory database, target-write planning, position cache, HAP startup. |
| CLI commands   | `src/commands/*` | Install, upgrade, doctor, serve, remote, logs, config, and HomeKit commands.                           |

The important boundaries are `core` (domain types), `service` (application dispatch), `controller` + `driver` (hardware orchestration), and `hap` versus `homekit` (protocol vs project).

## Core (`src/core/`)

`Channel` and `Command` are the shared vocabulary for drivers, GPIO, RTS, HomeKit, and transports. Telis-specific helpers such as `channel_led_gpio()` live in `gpio.rs`.

## BlindService (`src/service/`)

`BlindService` is the single application entry for REST/WebSocket presses:

- **`parse_wire`** — validates JSON/CLI bodies (`command`, optional `channel`; pairing uses `prog` or `prog_long`).
- **`press` / `press_wire`** — UI dispatch: `select` runs directly; directional commands optionally `select` a channel first, then execute on the current selection.
- **`ensure_pairing_for_kind`** — rejects `prog` when `supports_pairing` is false.

`serve` constructs one `BlindService` per process and stores it in `AppState`. The web API reads selection via `BlindService::current_selection()` and `subscribe_selection()` without reaching into `BlindController` directly. HomeKit `TargetPosition` writes use `BlindController::execute_on` with HAP-specific batching and cache coalescing in `homekit/`.

## BlindController

`BlindController` wraps `CommandRouter` and owns cross-cutting state that isn't driver-specific:

- `watch<Channel>` (delegated to the driver) tracks the current selection.
- `broadcast<PositionUpdate>` publishes completed Up/Down movement events.

The selection watch is stateful. A new SSE or WebSocket client subscribes and immediately learns whether the active channel is `L1`, `L2`, `L3`, `L4`, or `ALL`. On Telis the selection follows the physical LEDs; on RTS it is persisted to `rts.json` and survives restart.

The position broadcast is event-like. It fires after every successful `Up` / `Down` and carries an inferred HomeKit position (`100` for Up, `0` for Down). When the command targets `ALL`, `complete_command` fans the update out to `L1`–`L4` so HomeKit's per-channel position cache stays consistent with what a physical RTS remote does over the air.

## Command Serialization

The hardware can only do one thing at a time. Software callers are more flexible: the web UI, REST/WebSocket clients, and HomeKit can all issue commands. Each driver serializes its own hardware transactions:

- **Telis** uses `execute_lock` so cycle-and-press sequences don't interleave at the GPIO level.
- **RTS** serializes presses through the hardware mutex inside the blocking transmitter (the radio + pigpio client live behind a single `Mutex`, run on `spawn_blocking`); the selection state lock is separate, so `select` never blocks behind an in-flight transmission.

The lock is not access control — multiple callers can submit commands. It just guarantees coherent hardware sequences. Queueing keeps software callers simpler than rejecting concurrent commands with a "busy" error.

## Web Flow

The web API is intentionally small. `/command` is the only write endpoint, and it accepts the same shape as WebSocket command messages: a command name (`up`, `down`, `stop`, `select`, `prog`, `prog_long`, …) and an optional `channel`. The handler delegates to `BlindService::press_wire`. Long RF pairing bursts use `prog_long` (CLI: `somfy remote prog … --long`).

Both `GET /events` and `GET /ws` subscribe to `BlindService::subscribe_selection()`, send the current channel immediately, then forward selection changes. Incoming WebSocket commands are spawned as tasks so updates keep flowing while a command is in flight. A per-connection semaphore keeps those spawned commands ordered; the driver locks still protect the hardware globally.

## HomeKit adapter

`src/homekit/mod.rs` boots mDNS, the HAP TCP server, and the position listener. `src/homekit/somfy.rs` implements the accessory app; `target_writes.rs` batches and coalesces `TargetPosition` writes; `position_cache.rs` persists inferred positions. HomeKit commands use `BlindController::execute_on` and the same `CommandRouter` hardware path as web presses.

HomeKit has no direct physical position feedback from the blinds. The adapter therefore keeps a best-effort cache: Up snaps to `100`, Down snaps to `0`, and requested target positions snap to the nearest endpoint. `ALL` is project-level behavior; writing it fans out to individual channels, and individual writes only update `ALL` when every channel matches.

Protocol (pair-setup, encryption, PUT semantics, pairing commands): [HAP.md](HAP.md).

The HAP server is not built on Axum or Hyper because pair-verify upgrades the existing TCP socket into Apple's encrypted frame format. `src/hap/server/mod.rs` owns that socket loop; `src/hap/server/handlers/` contains the protocol routes so the IO loop stays readable.

## Persistence

All writes use atomic temp-file + rename. RTS uses a write-ahead reserve block (default 16 codes) so a crash cannot roll the on-air counter backwards. Burning a few unused codes is acceptable; replaying an old rolling code is what can desync a motor.

## Design Tradeoffs

- **Pluggable driver:** lets the binary drive a wired Telis remote, a CC1101 radio, or nothing at all without changing the UI surface.
- **Telis driver uses real hardware as source of truth:** avoids drift for LED selection, but every command pays physical timing cost.
- **RTS driver persists rolling codes with a reserve block:** a crash loses a handful of unused codes rather than reusing one, which is the irreversible failure mode for RTS pairing.
- **Queued command execution:** lets multiple software clients use the remote without interleaving operations.
- **Inferred blind positions with `ALL` fan-out:** useful for HomeKit UX, but not true physical feedback.
- **Native HAP implementation:** removes Homebridge/Node deployment complexity, but requires maintaining protocol code.
- **pigpiod for RF timing instead of in-process sleeps:** 640 µs Manchester half-symbols don't tolerate Tokio scheduler jitter; pigpiod owns the microsecond budget.

## Good First Places To Read

1. `src/core/` for `Channel` and `Command`.
2. `src/service/mod.rs` for wire validation and UI press dispatch.
3. `src/controller.rs` for hardware orchestration, position fan-out, and concurrency.
4. `src/server.rs` for the web API, SSE stream, and WebSocket flow.
5. `src/homekit/somfy.rs` for the HomeKit adapter, then `src/homekit/target_writes.rs` for write planning.
6. `src/hap/server/mod.rs` and `src/hap/server/handlers/` for the HAP request/session loop.
7. `docs/HARDWARE.md` for wiring, pairing, and the end-to-end data-flow diagram.
8. `docs/HAP.md` for the HAP server and pairing lifecycle.
