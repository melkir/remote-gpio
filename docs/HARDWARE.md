# Hardware & Architecture Details

Detailed documentation for the RemoteGPIO project.

## Raspberry Pi ↔ Somfy Telis 4 Wiring

This project hard-wires a Raspberry Pi to a Somfy Telis 4 remote:

- **Outputs (Pi → Somfy):** Simulate button presses (active-low)
- **Inputs (Somfy → Pi):** Read LED selection state
- **Power:** Shared 3.3V and GND (no level shifting needed)

### Connection Table

| Raspberry Pi Pin # | GPIO #  | Direction | Somfy Telis 4 Point | Function |
|--------------------|---------|-----------|---------------------|----------|
| Pin 17             | 3.3V    | Power     | +3V                 | Power supply to remote |
| Pin 6              | GND     | Power     | 0V / OV             | Ground |
| Pin 37             | GPIO26  | Output    | UP                  | Raise blinds |
| Pin 35             | GPIO19  | Output    | STOP                | Stop movement |
| Pin 33             | GPIO13  | Output    | DOWN                | Lower blinds |
| Pin 31             | GPIO6   | Output    | SELECT              | Select next blind |
| Pin 40             | GPIO21  | Input     | LED1                | Selection indicator 1 |
| Pin 38             | GPIO20  | Input     | LED2                | Selection indicator 2 |
| Pin 36             | GPIO16  | Input     | LED3                | Selection indicator 3 |
| Pin 32             | GPIO12  | Input     | LED4                | Selection indicator 4 |

### ASCII Wiring Diagram

```
  Raspberry Pi                          Somfy Telis 4
  ┌────────────────┐                   ┌─────────────┐
  │ Pin 17 (3.3V)  │ ────────────────▶ │ +3V         │
  │ Pin 6  (GND)   │ ────────────────▶ │ 0V          │
  │ Pin 37 (GPIO26)│ ────────────────▶ │ UP          │
  │ Pin 33 (GPIO13)│ ────────────────▶ │ DOWN        │
  │ Pin 35 (GPIO19)│ ────────────────▶ │ STOP        │
  │ Pin 31 (GPIO6) │ ────────────────▶ │ SELECT      │
  │ Pin 40 (GPIO21)│ ◀──────────────── │ LED1        │
  │ Pin 38 (GPIO20)│ ◀──────────────── │ LED2        │
  │ Pin 36 (GPIO16)│ ◀──────────────── │ LED3        │
  │ Pin 32 (GPIO12)│ ◀──────────────── │ LED4        │
  └────────────────┘                   └─────────────┘
```

---

## GPIO Implementation

### Output Pulses

Outputs are driven as active-low pulses. The code asserts the line for ~60ms, then releases—mimicking a button tap.

```rust
pub async fn trigger_output(output: Output) -> Result<()> {
    let req = Request::builder()
        .on_chip("/dev/gpiochip0")
        .with_line(output as u32)
        .as_output(Value::Active)
        .as_active_low()
        .request()?;

    tokio::time::sleep(Duration::from_millis(60)).await;
    req.set_lone_value(Value::Inactive)?;
    Ok(())
}
```

### Input Debouncing

The backend watches input lines with edge detection, collecting up to 16 events within a 300ms window:

- **Multiple rapid edges:** Selection is `ALL` (group mode—LEDs blink)
- **Single edge:** Maps to `L1`–`L4`

```rust
let timeout_duration = Duration::from_millis(300);
while event_count < 16 && start_time.elapsed() < timeout_duration {
    if let Some(Ok(event)) = events.next().await {
        last_event = Some(event.offset);
        event_count += 1;
    }
}
// 16+ edges in 300ms = ALL, otherwise map last edge to L1-L4
```

---

## Architecture

### Data Flow

```
┌─────────────────────────────────────────────────────────────┐
│                     FRONTEND (Preact/Vite)                  │
│  WebSocket connection with auto-reconnect + heartbeat       │
│  Displays LED indicators & control buttons                  │
│  Haptic feedback on interactions                            │
└──────────────────────────┬──────────────────────────────────┘
                           │
         WebSocket (ws://host/ws) + HTTP (POST /command)
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                  BACKEND (Axum/Tokio)                       │
│                                                             │
│  Routes:                                                    │
│  ├─ GET /led      → Current LED selection                   │
│  ├─ POST /command → Execute button commands                 │
│  ├─ GET /ws       → WebSocket upgrade                       │
│  └─ /*            → Static files from dist/                 │
│                                                             │
│  RemoteControl:                                             │
│  └─ watch::channel broadcasts LED state to all clients      │
└──────────────────────────┬──────────────────────────────────┘
                           │
                    gpiocdev (Linux GPIO char device)
                           │
┌──────────────────────────▼──────────────────────────────────┐
│              HARDWARE (Raspberry Pi GPIO)                   │
│                                                             │
│  Output pins → Button presses → Somfy Telis 4               │
│  Input pins  ← LED feedback   ← Somfy Telis 4               │
└─────────────────────────────────────────────────────────────┘
```

### Concurrency Model

The WebSocket handler uses a single `tokio::select!` loop that concurrently:

1. **Watches LED changes** via `watch::channel` and forwards to client
2. **Receives messages** (pings, commands) from client

Command processing is spawned as a separate task to avoid blocking LED updates. This ensures all clients see intermediate selection states in real-time.

```rust
loop {
    tokio::select! {
        result = rx_led.changed() => {
            // Forward LED state to client
        }
        msg = stream.next() => {
            // Handle ping/pong or spawn command processing
        }
    }
}
```

### State Engine: RemoteControl

`RemoteControl` is the central coordinator:

1. **Startup:** Presses SELECT, reads LEDs, seeds `watch::channel`
2. **Methods:** `select()`, `up()`, `down()`, `stop()` trigger GPIO
3. **Broadcasts:** LED changes propagate to all WebSocket clients

---

## Frontend

The Preact PWA features:

- **Connection status bar:** Color-coded (connecting/connected/error)
- **Control buttons:** Large circular Up, Stop, Down buttons
- **LED indicators:** Clickable dots for L1–L4; center button for SELECT
- **Long-press:** Sends `ALL` intent for group mode
- **Haptics:** 100ms on press, 200ms on finish
- **Auto-reconnect:** Exponential backoff (1s→2s→4s→8s→10s max)

---

## Why This Design?

- **Single source of truth:** Pi reads real GPIO states and broadcasts—every UI stays consistent
- **Low-latency:** WebSockets deliver immediate feedback
- **Non-blocking:** Async GPIO timing doesn't stall the runtime
- **Small footprint:** Easy to audit, extend, or port to different hardware
