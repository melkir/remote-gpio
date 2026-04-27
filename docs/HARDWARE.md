# Hardware Notes

A deeper look at how `somfy` is wired to the Somfy Telis 4 and how the backend turns GPIO edges into synchronized UI state. For a broader codebase tour, see [ARCHITECTURE.md](ARCHITECTURE.md).

## Raspberry Pi ↔ Somfy Telis 4

- **Outputs (Pi → Somfy):** simulate button presses (active-low pulses).
- **Inputs (Somfy → Pi):** read the LED selection state.
- **Power:** shared 3.3V and GND — no level shifting needed.

### Connection Table

| Pi Pin | GPIO   | Direction | Somfy Point | Function              |
| ------ | ------ | --------- | ----------- | --------------------- |
| 17     | 3.3V   | Power     | +3V         | Power supply          |
| 6      | GND    | Power     | 0V          | Ground                |
| 37     | GPIO26 | Output    | UP          | Raise blinds          |
| 35     | GPIO19 | Output    | STOP        | Stop movement         |
| 33     | GPIO13 | Output    | DOWN        | Lower blinds          |
| 31     | GPIO6  | Output    | SELECT      | Select next blind     |
| 40     | GPIO21 | Input     | LED1        | Selection indicator 1 |
| 38     | GPIO20 | Input     | LED2        | Selection indicator 2 |
| 36     | GPIO16 | Input     | LED3        | Selection indicator 3 |
| 32     | GPIO12 | Input     | LED4        | Selection indicator 4 |

### Wiring Diagram

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

## GPIO Implementation

### Output Pulses

Outputs are driven as active-low pulses. The code asserts the line for ~60ms, then releases — mimicking a button tap.

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

- **Multiple rapid edges:** selection is `ALL` (group mode — LEDs blink).
- **Single edge:** maps to `L1`–`L4`.

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

1. **Watches LED changes** via `watch::channel` and forwards to the client.
2. **Receives messages** (pings, commands) from the client.

Command processing is spawned as a separate task to avoid blocking LED updates. This ensures all clients see intermediate selection states in real time.

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

### State Engine: `RemoteControl`

`RemoteControl` is the central coordinator:

1. **Startup:** presses SELECT, reads LEDs, seeds the `watch::channel`.
2. **Methods:** `select()`, `up()`, `down()`, `stop()` trigger GPIO.
3. **Broadcasts:** LED changes propagate to all WebSocket clients.

## Frontend

The Preact PWA features:

- **Connection status bar:** color-coded (connecting / connected / error).
- **Control buttons:** large circular Up, Stop, Down.
- **LED indicators:** clickable dots for L1–L4; center button for SELECT.
- **Long-press:** sends `ALL` intent for group mode.
- **Haptics:** 100ms on press, 200ms on finish.
- **Auto-reconnect:** exponential backoff (1s → 2s → 4s → 8s → 10s max).

## Why This Design

- **Single source of truth:** the Pi reads real GPIO state and broadcasts — every UI stays consistent.
- **Low latency:** WebSockets deliver immediate feedback.
- **Non-blocking:** async GPIO timing doesn't stall the runtime.
- **Small footprint:** easy to audit, extend, or port to different hardware.
