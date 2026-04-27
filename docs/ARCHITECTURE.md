# Architecture Guide

This guide explains how the codebase fits together and the tradeoffs behind the
main moving parts. It is meant for someone reading the project for the first
time.

## Mental Model

`somfy` turns one physical Somfy Telis 4 remote into several software-facing
interfaces:

- HTTP/SSE for the web UI.
- WebSocket for bidirectional API clients.
- Native HomeKit Accessory Protocol (HAP) for Apple Home.
- GPIO reads and writes against the physical remote.

The physical remote remains the source of truth for LED selection. The software
does not emulate a remote from scratch; it presses the real buttons and reads
the real LEDs.

## Module Map

| Area           | Files            | Responsibility                                                                                  |
| -------------- | ---------------- | ----------------------------------------------------------------------------------------------- |
| Web API        | `src/server.rs`  | Axum HTTP, SSE, WebSocket routes and static app serving.                                        |
| Remote control | `src/remote.rs`  | Serializes physical button sequences, tracks selected LED, emits movement events.               |
| GPIO           | `src/gpio.rs`    | Low-level Linux GPIO input/output mapping and debounce logic.                                   |
| HAP core       | `src/hap/*`      | Generic-ish HAP protocol pieces: TLV, SRP, pair setup/verify, session encryption, HTTP framing. |
| HomeKit app    | `src/homekit/*`  | Somfy-specific HomeKit wiring: accessory database, state paths, position cache, HAP startup.    |
| CLI commands   | `src/commands/*` | Install, upgrade, doctor, serve, and HomeKit lifecycle commands.                                |

The important boundary is `hap` versus `homekit`: `hap` should stay about the
protocol, while `homekit` knows this project exposes Somfy blinds over that
protocol.

## RemoteControl

`RemoteControl` is the central coordination point for the physical remote.

It has two different async streams:

- `watch<Input>` tracks the current LED selection.
- `broadcast<PositionUpdate>` publishes completed Up/Down movement events.

They are deliberately separate.

The selection watch channel is stateful. A new SSE or WebSocket client can
subscribe and immediately know whether the remote is on `L1`, `L2`, `L3`, `L4`,
or `ALL`. Selection changes can happen without blind movement, for example when
the user presses `select`.

The position broadcast channel is event-like. It only fires after a successful
`Up` or `Down` command and carries an inferred HomeKit position: `100` for Up,
`0` for Down. It does not represent the current selected LED and should not be
used as a state cache.

## Command Serialization

The physical remote can only do one thing at a time. Software callers are more
flexible: the web UI, REST/WebSocket clients, and HomeKit can all issue commands.

`RemoteControl::execute` uses `execute_lock` to serialize the full hardware
transaction:

1. Optionally cycle the remote to the requested LED.
2. Press the requested button.
3. Publish the inferred position update if the command was Up or Down.

The lock is not access control. Multiple people can observe the remote and
multiple callers can submit commands. The lock only guarantees that command
sequences do not interleave at the GPIO level.

This removes the physical limitation of one hand touching the remote while
keeping the important invariant: every button press applies to the LED selected
for that command.

The alternative would be rejecting concurrent commands with a “busy” error.
That would more closely mimic a physical remote, but it would make the software
less useful. Queueing command sequences is simpler for users and still keeps the
hardware behavior coherent.

## Web Flow

The web server exposes one command endpoint and two live-state transports:

- `GET /led` for the current selection.
- `POST /command` for one command.
- `GET /events` for the Preact PWA's SSE stream of LED updates.
- `GET /ws` for bidirectional clients that want live LED updates and command messages.

The Preact PWA uses SSE for live state and `POST /command` for actions. `/ws`
stays available for bidirectional clients. Both live transports subscribe to
`RemoteControl::subscribe_selection()`, send the current LED immediately, and
then forward selection changes. Incoming WebSocket commands are spawned as tasks
so LED updates keep flowing while a command is cycling through the physical
remote.

## HomeKit Flow

`src/homekit/mod.rs` starts the HomeKit subsystem:

1. Load or initialize persistent HAP identity from `hap.json`.
2. Build the setup URI and log/render the QR code if unpaired.
3. Advertise `_hap._tcp.local.` through mDNS.
4. Create `SomfyHapApp`, the Somfy-specific implementation of the HAP runtime trait.
5. Start the generic HAP TCP server.
6. Listen for `PositionUpdate` events from `RemoteControl` and mirror them into HomeKit events.

HomeKit has no direct physical position feedback from the blinds. The app keeps
a best-effort cache:

- Up means position `100`.
- Down means position `0`.
- Target values below `50` snap to `0`; values `50` and above snap to `100`.
- The cache is persisted to `positions.json`.
- Restart never replays cached positions to GPIO.

The `ALL` accessory is also custom project behavior. Writing `ALL` propagates
to the individual blinds, and individual writes update `ALL` only when every
individual blind matches.

## HAP Core

The HAP server is intentionally not built on Axum or Hyper. After pair-verify,
HAP switches the existing TCP socket into its own encrypted frame format, so a
normal HTTP server abstraction does not fit cleanly.

Instead:

- `httparse` parses plain HTTP request bytes.
- The same parser is reused after decrypting HAP frames.
- `http::StatusCode` provides canonical response phrases.
- `EncryptedReader` and `EncryptedWriter` own the post-verify AEAD framing.

`handle_connection` owns the socket loop. It waits for either a request or a
HomeKit event. Request routing is delegated to helpers so the loop stays focused
on IO and event multiplexing.

## Persistence

There are two state files:

- `hap.json` contains the HomeKit identity, setup code, signing key, config/state numbers, and paired controllers.
- `positions.json` contains the HomeKit position cache.

Both files are stored under the systemd state directory in production and under
`./hap-state` in debug builds. Writes are atomic via temp file plus rename.

Identity stability matters. Changing the HAP identity forces re-pairing.
Changing accessory IDs or characteristic IDs can make Home lose room, name, and
automation associations.

## Design Tradeoffs

- **Real hardware as source of truth:** avoids drift for LED selection, but all commands must respect physical timing.
- **Queued command execution:** lets multiple software clients use the remote without interleaving GPIO operations.
- **Inferred blind positions:** useful for HomeKit UX, but not true physical feedback.
- **Native HAP implementation:** removes Homebridge/Node deployment complexity, but requires maintaining protocol code.
- **Generic HAP core plus Somfy adapter:** keeps protocol code cleaner while making project-specific quirks explicit.

## Good First Places To Read

1. `src/remote.rs` for the command and concurrency model.
2. `src/server.rs` for the web API, SSE stream, and WebSocket flow.
3. `src/homekit/somfy.rs` for the HomeKit accessory model.
4. `src/hap/server.rs` for the HAP request/session loop.
5. `docs/HARDWARE.md` for GPIO wiring and debounce details.
