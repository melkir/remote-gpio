# RemoteGPIO

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

The script downloads the latest stable `somfy` binary for `armv7-unknown-linux-gnueabihf.2.31`, drops it in `/usr/local/bin`, and runs `somfy install` to write the systemd unit and start the service. Hardware and driver settings live in `/etc/somfy/config.toml` — see [docs/HARDWARE.md](docs/HARDWARE.md#configuration). The default driver on a Pi is `telis`.

### Upgrade

```bash
sudo somfy upgrade
```

Pulls the latest stable release, swaps the binary, refreshes the systemd unit, restarts the service, and rolls back if the new service fails to start.

### Drivers

`somfy` ships one binary with three drivers. The active driver is chosen at
startup from `/etc/somfy/config.toml`; changing it restarts the service because
the hardware stack is constructed once.

Switch drivers with:

```bash
somfy config set-driver <fake|telis|rts>
somfy doctor
```

`config set-driver` rewrites the config, runs any new-driver prerequisites
(`pigpiod -l` for RTS), and restarts the unit.

| Driver  | Hardware                  | Use case                                           |
| ------- | ------------------------- | -------------------------------------------------- |
| `fake`  | none                      | Local dev — logs commands, no hardware.            |
| `telis` | wired Pi ↔ Telis 4 remote | GPIO drives the physical Telis remote.             |
| `rts`   | CC1101 433.42 MHz radio   | Virtual RTS remote over the air (pairing, `prog`). |

`install` adds the invoking user to the `somfy` group and installs the polkit
rule used by day-to-day commands. `install` and `upgrade` still need root because
they write `/usr/local/bin/somfy` and systemd units.

Wiring, pairing, and bring-up: [docs/HARDWARE.md](docs/HARDWARE.md). Code
structure: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

### API

Default bind: `127.0.0.1:5002`, which works well with a local tunnel such as
cloudflared. Set `[server].bind = "0.0.0.0:5002"` only when you intentionally
want the API reachable on the LAN.

Endpoints:

| Endpoint   | Method    | Description                             |
| ---------- | --------- | --------------------------------------- |
| `/events`  | GET       | SSE stream of channel selection changes |
| `/command` | POST      | Execute command (JSON body)             |
| `/channel` | GET       | Current channel selection (plain text)  |
| `/ws`      | WebSocket | Live selection + commands               |

Example commands:

```json
{"command":"up"}
{"command":"up","channel":"L3"}
{"command":"select","channel":"L3"}
{"command":"prog","channel":"L3","long":true}
```

`prog` requires `driver = "rts"` —
see [RTS pairing](docs/HARDWARE.md#pairing). Request flow and concurrency:
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#web-flow).

CLI equivalents: `somfy remote --help`.

### HomeKit

HomeKit is built into the Rust binary. The service runs a HAP bridge on port
`5010`, advertises it with mDNS, and exposes one `WindowCovering` per channel.
There is no Homebridge or Node process to install.

Pair with:

```bash
somfy homekit status
```

The command prints the QR code and setup code used by the iOS Home app. Other
pairing commands: `somfy homekit --help`. Protocol and persistence:
[docs/HAP.md](docs/HAP.md).

### Configuration

Resolved settings after defaults and validation:

```bash
somfy config show    # full TOML
somfy config path    # file path
```

Hardware settings belong in config, not repeated CLI flags. That keeps
`serve`, `doctor`, `install`, and `upgrade` looking at the same source of truth.

### Versioning

- Push to `main` → CI cross-compiles for armv7 and refreshes a moving `main` prerelease.
- Tag `vX.Y.Z` → stable release + `SHA256SUMS`.
- `somfy --version` and `somfy doctor` report the embedded git SHA and build date.

Release from a clean `main`: `mise run release --execute`. The Pi upgrades with `sudo somfy upgrade` (CI never SSHs to the device).

### Documentation

| Doc                                          | Contents                                              |
| -------------------------------------------- | ----------------------------------------------------- |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Code layout, drivers, concurrency, module map         |
| [docs/HARDWARE.md](docs/HARDWARE.md)         | Wiring, GPIO/RTS setup, pairing, end-to-end data flow |
| [docs/RTS_DRIVER.md](docs/RTS_DRIVER.md)     | RTS frame format, waveform, pigpiod                   |
| [docs/HAP.md](docs/HAP.md)                   | HomeKit protocol and pairing lifecycle                |
| [CLAUDE.md](CLAUDE.md)                       | Contributor/agent conventions                         |
