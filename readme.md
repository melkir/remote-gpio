## RemoteGPIO

RemoteGPIO is a compact Rust + Preact project that exposes a Raspberry Pi–attached remote (for shutters/blinds) over WebSocket and HTTP, keeping a single shared state across all clients.

- Backend: Rust, `axum`, `tokio`, `gpiocdev`
- Frontend: Preact + Vite + Tailwind, PWA-enabled
- Transport: WebSocket for real-time state, HTTP for simple commands
- Target: Raspberry Pi with Linux GPIO character device

<video src="https://github.com/user-attachments/assets/4dbb72bf-5b67-4a23-8322-f3749d19901c" autoplay loop muted playsinline></video>

Before the WebSocket connects, a loading indicator appears at the top.
After the connection is established, the client can control the server in real time over WebSocket. Because only one physical remote is attached to the Raspberry Pi, the selected shutter state is global and synchronized across all WebSocket clients. The server keeps this shared state by reading the GPIO pins.


### Build

You need to have [zig](https://ziglang.org/) and [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild) installed for cross-compilation.

```bash
brew install zig
cargo install cargo-zigbuild
```

### Usage

The included [remote-gpio.sh](https://github.com/melkir/remote-gpio/blob/main/remote-gpio.sh) script automates build and deploy:

- Builds the frontend (`bun run build`) and cross-compiles the Rust binary using `cargo-zigbuild`.
- Syncs artifacts to your Pi via `rsync`.
- Starts the app with `RUST_LOG=info` and offers an interactive loop to rebuild/restart quickly.
- A `delete` command cleans up the remote directory on the Pi.

Update `RASPBERRY_PI_IP` and `REMOTE_DIR` in `remote-gpio.sh`, then:

```bash
./remote-gpio.sh start
```

If you are done, you can remove the application from the Raspberry Pi.

```bash
./remote-gpio.sh delete
```

### Troubleshooting

If the pins are already in use, you can check the list of open files on the Raspberry Pi.

```bash
lsof | grep gpio
```

---

## Raspberry Pi ↔ Somfy Telis 4: Wiring & Power

This project assumes a simple hard-wire between a Raspberry Pi and a Somfy Telis 4 remote:

- Outputs (Pi → Somfy) simulate button presses (active-low)
- Inputs (Somfy → Pi) read LED selection state
- Power is shared at 3.3V and GND (no level shifting needed)

### Connection Table

| Raspberry Pi Pin # | GPIO #  | Direction | Somfy Telis 4 Point | Function |
|--------------------|---------|-----------|----------------------|----------|
| Pin 17             | 3.3V    | Power     | +3V                  | Power supply to remote |
| Pin 6              | GND     | Power     | 0V / OV              | Ground |
| Pin 37             | GPIO26  | Output    | UP                   | Raise blinds |
| Pin 35             | GPIO19  | Output    | STOP                 | Stop movement |
| Pin 33             | GPIO13  | Output    | DOWN                 | Lower blinds |
| Pin 31             | GPIO6   | Output    | SELECT               | Select next blind |
| Pin 40             | GPIO21  | Input     | LED1                 | Selection indicator 1 |
| Pin 38             | GPIO20  | Input     | LED2                 | Selection indicator 2 |
| Pin 36             | GPIO16  | Input     | LED3                 | Selection indicator 3 |
| Pin 32             | GPIO12  | Input     | LED4                 | Selection indicator 4 |

### ASCII Wiring Diagram

```text
   Raspberry Pi 3 GPIO Header                Somfy Telis 4 PCB
   ┌───────────────────────────┐             ┌───────────────────┐
   │ Pin 17 (3.3V)  ───────────┼────────────▶│ +3V               │
   │ Pin 6  (GND)   ───────────┼────────────▶│ 0V / OV           │
   │ Pin 37 (GPIO26)───────────┼────────────▶│ UP                │
   │ Pin 33 (GPIO13)───────────┼────────────▶│ DOWN              │
   │ Pin 35 (GPIO19)───────────┼────────────▶│ STOP              │
   │ Pin 31 (GPIO6) ───────────┼────────────▶│ SELECT            │
   │ Pin 40 (GPIO21)◀──────────┼─────────────│ LED1              │
   │ Pin 38 (GPIO20)◀──────────┼─────────────│ LED2              │
   │ Pin 36 (GPIO16)◀──────────┼─────────────│ LED3              │
   │ Pin 32 (GPIO12)◀──────────┼─────────────│ LED4              │
   └───────────────────────────┘             └───────────────────┘
```

## Hardware model: inputs vs outputs

- Inputs (LED selection feedback): `L1=21`, `L2=20`, `L3=16`, `L4=12`. These reflect which channel (or multiple) is selected on the physical remote.
- Outputs (button presses): `Select=6`, `Down=13`, `Stop=19`, `Up=26`. These emulate remote button presses.

Outputs are driven as active-low pulses. The code asserts the line for ~60 ms, then releases, which mimics a button tap.

See GPIO mappings and logic in code: [src/gpio.rs](https://github.com/melkir/remote-gpio/blob/main/src/gpio.rs).

```rust
// Enums mapping inputs and outputs to GPIO offsets
#[derive(Copy, Clone, Debug, Deserialize, PartialEq, Eq)]
pub enum Input { L1 = 21, L2 = 20, L3 = 16, L4 = 12, ALL }

#[derive(Debug)]
pub enum Output { Select = 6, Down = 13, Stop = 19, Up = 26 }

// Active-low pulse for ~60ms, then release
pub fn trigger_output(output: Output) -> Result<()> {
    let offset = output as u32;
    let mut value = Value::Active;
    let req = Request::builder()
        .on_chip("/dev/gpiochip0")
        .with_line(offset)
        .as_output(value)
        .as_active_low()
        .request()?;
    thread::sleep(Duration::from_millis(60));
    req.set_lone_value(value.not())?;
    Ok(())
}
```

---

## Reading selection with debounced edge detection

The backend watches input lines on `/dev/gpiochip0` with edge detection. It collects up to 16 events within a 300 ms window:

- If multiple edges appear quickly, selection is treated as `ALL`.
- Otherwise, the last edge’s offset is mapped back to the chosen `L1`–`L4`.

This logic produces the authoritative selection that the server broadcasts to clients.

```rust
// Debounce/aggregation window: 300ms, up to 16 edge events
let timeout_duration = Duration::from_millis(300);
while event_count < 16 && start_time.elapsed() < timeout_duration {
    if let Some(Ok(event)) = events.next().await { /* ... */ }
}
// If many edges -> ALL, else map last edge offset to L1..L4
```

---

## The state engine: `RemoteControl`

`RemoteControl` is the brain:

- On startup, it “presses” Select, reads the LEDs, and seeds a `watch` channel with the current selection.
- It exposes async methods `select()`, `up()`, `down()`, `stop()` that trigger GPIO outputs.
- Whenever selection changes, it broadcasts the new value via the `watch` channel so every WebSocket client stays in sync.

---

## Web API: WebSocket + HTTP

The server listens on `0.0.0.0:5002` and serves both API and static assets.

- WebSocket: `GET /ws`
  - On connect, the server immediately sends the current LED selection.
  - Then it streams future updates when selection changes.
  - It accepts JSON commands like:

    ```json
    { "command": "select" }
    { "command": "select", "led": "L3" }
    { "command": "up" }
    { "command": "stop" }
    { "command": "down" }
    ```

  - Special case: `led: "ALL"` tells the system to cycle until it detects multiple LEDs (group mode).

- HTTP:
  - `GET /led` returns the current selection as a string (`L1`, `L2`, `L3`, `L4`, or `ALL`).
  - `POST /command` accepts the same JSON payload as the WebSocket.

Example:

```bash
curl -X POST http://<pi>:5002/command \
  -H 'Content-Type: application/json' \
  -d '{"command":"select","led":"L2"}'
```

Static files are served from `dist`, so you can host the UI from the same port.

Implementation details:
- Server routes and WebSocket handling: [src/server.rs](https://github.com/melkir/remote-gpio/blob/main/src/server.rs)
- Remote control state engine: [src/remote.rs](https://github.com/melkir/remote-gpio/blob/main/src/remote.rs)

---

## Frontend: a tiny, tactile PWA

The Preact app connects to the WebSocket:

- Displays a slim, color-coded connection status bar.
- Offers big circular buttons for Up, Stop, and Down.
- Shows LED indicators for `L1`–`L4`; clicking a dot selects that channel.
- The center Select button cycles selection; long-press actions send the `ALL` intent.
- Haptics provide short and long feedback on press and finish (mobile-friendly).

It uses `react-use-websocket` with auto-reconnect and a heartbeat. The PWA config allows installation and fullscreen usage.

See the UI interaction code: [app/src/App.tsx](https://github.com/melkir/remote-gpio/blob/main/app/src/App.tsx)

---

## Concurrency model

The WebSocket handler runs two tasks:

- A sender task that forwards selection updates (from the `watch` channel) to the client.
- A receiver task that parses inbound JSON commands and executes them via `RemoteControl`.

A `tokio::select!` ensures that if one side closes, the other task is cancelled cleanly.

---

## Why this approach?

- Single source of truth: The Pi reads real GPIO states and broadcasts them to all clients, so every UI is consistent.
- Low-latency control: WebSockets deliver immediate feedback and actions.
- Small, focused codebase: It’s easy to audit, extend (e.g., add safety interlocks), or port to different pins/devices.

If you’re looking for a clean pattern for hardware-backed, multi-client control with synchronized state, this project demonstrates a pragmatic, production-ready baseline.

