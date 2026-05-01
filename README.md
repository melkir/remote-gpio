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

The script downloads the latest stable `somfy` binary for `armv7-unknown-linux-gnueabihf.2.31`, drops it in `/usr/local/bin`, and runs `somfy install` to write the systemd unit and start the service. Persistent hardware choices live in `/etc/somfy/config.toml`; see [Configuration](docs/HARDWARE.md#configuration).
Pi builds default to the wired `telis` driver when no config file exists. For
hardware-free testing on the Pi, create `/etc/somfy/config.toml` with
`driver = "fake"` before installing.

HomeKit pairing is built in — see [HomeKit](#homekit) below.

### Upgrade

```bash
sudo somfy upgrade
```

The upgrade command pulls the latest stable release, swaps the binary, refreshes the systemd unit, restarts the service, and rolls back if the new service fails to start.

### Drivers

`somfy` ships three drivers, selected by the resolved config file:

| Driver | Hardware                  | Use case                                               |
| ------- | ------------------------- | ------------------------------------------------------ |
| `fake`  | none                      | Local dev — logs commands, no hardware.                |
| `telis` | wired Pi ↔ Telis 4 remote | Original setup: GPIO drives the physical Telis remote. |
| `rts`   | CC1101 433.42 MHz radio   | Pi acts as a virtual RTS remote, no Telis 4 needed.    |

Built-in defaults are target-aware: Raspberry Pi Linux builds select `telis`;
local development and CI-style non-Pi builds select `fake`. A config file always
wins over the built-in default.

Switch drivers by editing `/etc/somfy/config.toml`, then refresh and restart the service:

```bash
sudo somfy config validate
sudo somfy install
sudo systemctl restart somfy
sudo somfy doctor
```

#### RTS driver

The RTS driver transmits Somfy RTS frames directly through a CC1101 module. Each channel (`L1`–`L4` + `ALL`) is a separate virtual remote with its own 24-bit ID and rolling code, persisted to `/var/lib/somfy/rts.json`.

Enable SPI on the Pi, select RTS in `/etc/somfy/config.toml`, then install. When the resolved config selects RTS, `somfy install` installs `pigpio`, configures `pigpiod` to listen on localhost only, enables `pigpiod`, and refreshes the `somfy` unit.

```bash
sudo raspi-config            # enable SPI
sudo somfy install
sudo somfy doctor
```

Pair each channel once (motor in programming mode, then):

```bash
sudo somfy remote prog L1
sudo somfy remote up L1
sudo somfy remote down L1
sudo somfy remote stop L1
# repeat for L2, L3, L4, ALL as needed
```

If the original Telis remote's Prog button is wired to the Pi, configure
`telis.gpio.prog`. `somfy remote prog <channel>` then presses the wired Telis
Prog button first and sends the matching RTS virtual remote's Prog command. Run
the same command again to remove that virtual remote from the motor.

Inspect driver behavior through service logs:

```bash
somfy logs --debug
```

Wiring and register details: [docs/HARDWARE.md](docs/HARDWARE.md#cc1101-rts-driver).

### API

Server listens on `0.0.0.0:5002`.

| Endpoint   | Method    | Description                                  |
| ---------- | --------- | -------------------------------------------- |
| `/ws`      | WebSocket | Real-time selection state + accepts commands |
| `/events`  | GET       | SSE stream of channel selection changes      |
| `/channel` | GET       | Last channel selection                       |
| `/command` | POST      | Execute command                              |

```json
{"command": "up"}
{"command": "down"}
{"command": "stop"}
{"command": "up", "channel": "L3"}
{"command": "select"}
{"command": "select", "channel": "L3"}
{"command": "prog", "channel": "L3"}
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
