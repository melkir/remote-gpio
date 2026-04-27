## RemoteGPIO

Self-hosted Somfy Telis 4 controller for Raspberry Pi with native HomeKit, a Preact PWA, and live SSE/WebSocket sync.

<video src="https://github.com/user-attachments/assets/4dbb72bf-5b67-4a23-8322-f3749d19901c" autoplay loop muted playsinline></video>

The Raspberry Pi is wired directly to the remote's button and LED traces, so the software presses the real remote and reads its current selection. The Rust service exposes that state to the web app and HomeKit, while deployment stays simple: one self-updating binary on the Pi, with no CI access to the device.

### Quick Start

```bash
brew install mise
mise install
mise run dev
```

### Install on a Pi

Commands in this section run on the Pi. From another machine, wrap them with SSH; `-t` allocates a terminal for commands that may prompt through `sudo`:

```bash
ssh -t pi '<command>'
```

Fresh bootstrap:

```bash
curl -fsSL https://raw.githubusercontent.com/melkir/remote-gpio/main/install.sh | sudo bash
```

The script downloads the latest stable `somfy` binary for `armv7-unknown-linux-gnueabihf.2.31`, drops it in `/usr/local/bin`, and runs `somfy install` to write the systemd unit and start the service. HomeKit pairing is built in — see [HomeKit](#homekit) below.

### Upgrade

```bash
sudo somfy upgrade
```

The upgrade command pulls the latest stable release, swaps the binary, refreshes the systemd unit, restarts the service, and rolls back if the new service fails to start.

### API

Server listens on `0.0.0.0:5002`.

| Endpoint   | Method    | Description                            |
| ---------- | --------- | -------------------------------------- |
| `/ws`      | WebSocket | Real-time LED state + accepts commands |
| `/events`  | GET       | SSE stream of LED selection changes    |
| `/led`     | GET       | Current selection (`L1`-`L4` or `ALL`) |
| `/command` | POST      | Execute command                        |

```json
{"command": "up"}
{"command": "down"}
{"command": "stop"}
{"command": "select"}
{"command": "select", "led": "L3"}
```

### HomeKit

`somfy serve` runs a native HAP server on port `5010`, advertised via mDNS as a Bridge with one `WindowCovering` per LED selector (`L1`–`L4` + `ALL`). No Homebridge, no plugin, no Node — Siri, the Home app, and HomePod talk directly to the Rust binary.

Pair on first install:

```bash
somfy homekit status
```

In the iOS Home app: **Add Accessory → scan the QR code** (or enter the setup code shown by the command). State (paired controllers, last-known position) lives under `/var/lib/somfy/`; `somfy upgrade` preserves it across binary swaps.

For pairing management, reset, and other HomeKit subcommands, ask the installed binary:

```bash
somfy homekit --help
```

See [docs/HAP.md](docs/HAP.md) for the protocol implementation, persistence layout, and connection lifecycle.
For a newcomer-oriented walkthrough of the whole codebase, see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

### Versioning

- Push to `main` → CI cross-compiles for armv7 and refreshes a moving `main` prerelease.
- Push a tag `vX.Y.Z` → CI publishes a stable release plus a `SHA256SUMS` file.
- The binary embeds its git SHA and build date via `vergen`, so `somfy --version` and `somfy doctor` always report what's actually running.

Release from a clean, up-to-date `main`:

```bash
mise run release --execute
```

CI never touches the Pi. Deployment is a pull from the device over SSH.

### More

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — codebase tour, data flow, concurrency model, and major tradeoffs.
- [docs/HARDWARE.md](docs/HARDWARE.md) — wiring, GPIO timing, concurrency model, and the "why" behind the design.
- [CLAUDE.md](CLAUDE.md) — build commands, repo layout, and patterns worth knowing before editing.
