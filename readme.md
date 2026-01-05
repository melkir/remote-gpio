## RemoteGPIO

A Rust + Preact app that controls a Raspberry Pi-attached Somfy Telis 4 remote over WebSocket, keeping all clients synchronized.

<video src="https://github.com/user-attachments/assets/4dbb72bf-5b67-4a23-8322-f3749d19901c" autoplay loop muted playsinline></video>

### Quick Start

**Prerequisites:** [zig](https://ziglang.org/) and [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild)

```bash
brew install zig && cargo install cargo-zigbuild
```

**Deploy and run:**

```bash
# Edit RASPBERRY_PI_IP in remote-gpio.sh first
./remote-gpio.sh build    # Build frontend + cross-compile, deploy to Pi
./remote-gpio.sh start    # Run interactively (r=rebuild, q=quit)
./remote-gpio.sh setup    # Install as systemd service (auto-starts on boot)
./remote-gpio.sh delete   # Remove from Pi
```

### API

Server listens on `0.0.0.0:5002`.

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/ws` | WebSocket | Real-time LED state + accepts commands |
| `/led` | GET | Current selection (`L1`-`L4` or `ALL`) |
| `/command` | POST | Execute command |

**Commands:**
```json
{"command": "up"}
{"command": "down"}
{"command": "stop"}
{"command": "select"}
{"command": "select", "led": "L3"}
```

### Architecture

```
Preact PWA ←──WebSocket──→ Axum Server ←──gpiocdev──→ GPIO Pins ←──→ Somfy Remote
                              ↓
                        watch::channel
                        (broadcasts LED state)
```

- **Single source of truth:** Pi reads GPIO and broadcasts to all clients
- **Non-blocking:** Async GPIO timing, commands spawned to not block LED updates
- **PWA:** Installable, works offline, haptic feedback

See [docs/HARDWARE.md](docs/HARDWARE.md) for detailed wiring diagrams, code snippets, and architecture explanation.

### Troubleshooting

```bash
# Check if pins are in use
lsof | grep gpio

# Service logs
journalctl --user -u remote-gpio -f
```
