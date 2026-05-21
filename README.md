# RemoteGPIO

Self-hosted Somfy controller for Raspberry Pi with native HomeKit, a Preact PWA, and live SSE/WebSocket sync.

<video src="https://github.com/user-attachments/assets/4dbb72bf-5b67-4a23-8322-f3749d19901c" autoplay loop muted playsinline></video>

The service models one Somfy installation behind a small command surface. It can press a wired Telis 4 remote, transmit RTS frames through a CC1101 radio, or run with a fake driver for local development. The web API, PWA, and HomeKit all use the same command router; hardware selection stays in config.

### Quick Start

```bash
brew install mise
mise install
mise run dev
```

### Install on a Pi

```bash
curl -fsSL https://raw.githubusercontent.com/melkir/remote-gpio/main/install.sh | sudo bash
```

The script downloads the latest stable Pi binary, installs it at `/usr/local/bin/somfy`, writes the systemd unit, and starts the service.

### Upgrade

```bash
sudo somfy upgrade
```

Pulls the latest stable release, swaps the binary, refreshes the systemd unit, restarts the service, and rolls back if the new service fails to start.

### Versioning

- Push to `main` → CI cross-compiles for armv7 and refreshes a moving `main` prerelease.
- Tag `vX.Y.Z` → stable release + `SHA256SUMS`.
- `somfy --version` and `somfy doctor` report the embedded git SHA and build date.

Release from a clean `main`: `mise run release --execute`. The Pi upgrades with `sudo somfy upgrade` (CI never SSHs to the device).

### Documentation

| Doc                                          | Contents                                              |
| -------------------------------------------- | ----------------------------------------------------- |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | System boundaries, runtime flows, drivers, tradeoffs  |
| [docs/HARDWARE.md](docs/HARDWARE.md)         | Wiring, GPIO/RTS setup, pairing, end-to-end data flow |
| [docs/RTS_DRIVER.md](docs/RTS_DRIVER.md)     | RTS frame format, waveform, pigpiod                   |
| [docs/HAP.md](docs/HAP.md)                   | HomeKit protocol and pairing lifecycle                |
| [CLAUDE.md](CLAUDE.md)                       | Contributor/agent conventions                         |
