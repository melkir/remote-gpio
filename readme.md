## RemoteGPIO

A Rust + Preact app that controls a Raspberry Pi-attached Somfy Telis 4 remote over WebSocket, keeping all clients synchronized.

<video src="https://github.com/user-attachments/assets/4dbb72bf-5b67-4a23-8322-f3749d19901c" autoplay loop muted playsinline></video>

A small study in wiring consumer hardware to the web: the Pi taps the Telis 4's button and LED traces directly, a Rust backend turns GPIO edges into broadcast state, and a Preact PWA stays in sync across every open tab. Deployment is a single self-updating binary, no CI access to the device.

### Quick Start

```bash
brew install mise
mise install
mise run dev
```

`mise tasks` lists everything (`dev`, `check`, `build`, `cross-build`, `fmt`). `mise install` provisions Rust (with the armv7 target), Bun, Zig, and `cargo-zigbuild`.

### Install on a Pi

Fresh bootstrap:

```bash
curl -fsSL https://raw.githubusercontent.com/melkir/remote-gpio/main/install.sh | sudo bash
```

The script downloads the latest stable `somfy` binary for `armv7-unknown-linux-gnueabihf.2.31`, drops it in `/usr/local/bin`, and runs `somfy install` to write the systemd unit and start the service.

### Day-to-day Operation

```bash
ssh pi sudo somfy upgrade                 # latest stable release
ssh pi sudo somfy upgrade --channel main  # moving prerelease built from main
ssh pi sudo somfy upgrade --version v0.2.0  # pin or roll back
ssh pi somfy doctor                       # health check (GPIO, unit, version)
ssh pi somfy --version                    # embedded git SHA + build date
```

Read-only verbs (`doctor`, `upgrade --check`, `--version`) work without sudo. Anything that writes to `/usr/local/bin` or `/etc/systemd/system` requires it.

### API

Server listens on `0.0.0.0:5002`.

| Endpoint   | Method    | Description                            |
| ---------- | --------- | -------------------------------------- |
| `/ws`      | WebSocket | Real-time LED state + accepts commands |
| `/led`     | GET       | Current selection (`L1`-`L4` or `ALL`) |
| `/command` | POST      | Execute command                        |

```json
{"command": "up"}
{"command": "down"}
{"command": "stop"}
{"command": "select"}
{"command": "select", "led": "L3"}
```

### Versioning

- Push to `main` → CI cross-compiles for armv7 and refreshes a moving `main` prerelease.
- Push a tag `vX.Y.Z` → CI publishes a stable release plus a `SHA256SUMS` file.
- The binary embeds its git SHA and build date via `vergen`, so `somfy --version` and `somfy doctor` always report what's actually running.

CI never touches the Pi. Deployment is a pull from the device over SSH.

### More

- [docs/HARDWARE.md](docs/HARDWARE.md) — wiring, GPIO timing, concurrency model, and the "why" behind the design.
- [CLAUDE.md](CLAUDE.md) — build commands, repo layout, and patterns worth knowing before editing.
