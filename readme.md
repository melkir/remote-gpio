## RemoteGPIO

A Rust + Preact app that controls a Raspberry Pi-attached Somfy Telis 4 remote over WebSocket, keeping all clients synchronized.

<video src="https://github.com/user-attachments/assets/4dbb72bf-5b67-4a23-8322-f3749d19901c" autoplay loop muted playsinline></video>

### Quick Start

```bash
brew install bun
bun --cwd=app install
bun --cwd=app run build
cargo run
```

Fresh Pi bootstrap:

```bash
curl -fsSL https://raw.githubusercontent.com/melkir/remote-gpio/main/install.sh | sudo bash
```

Day-to-day operation:

```bash
ssh pi sudo somfy upgrade            # latest stable
ssh pi sudo somfy upgrade --channel main
ssh pi somfy doctor
```

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

### More

- [CLAUDE.md](CLAUDE.md) — build commands, architecture, key patterns
- [docs/HARDWARE.md](docs/HARDWARE.md) — wiring diagrams
- [docs/deploy-cli.md](docs/deploy-cli.md) — deployment design and release workflow
