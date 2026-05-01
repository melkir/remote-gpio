# Hardware Notes

A deeper look at the two physical setups `somfy` supports — the wired Telis 4 driver and the CC1101 RTS radio driver — and how each turns hardware events into synchronized UI state. For a broader codebase tour, see [ARCHITECTURE.md](ARCHITECTURE.md).

## Telis 4 driver

### Raspberry Pi ↔ Somfy Telis 4

- **Outputs (Pi → Somfy):** simulate button presses (active-low pulses).
- **Inputs (Somfy → Pi):** read the LED selection state.
- **Power:** shared 3.3V and GND — no level shifting needed.

#### Connection Table

| Pi Pin | GPIO   | Direction | Somfy Point | Function              |
| ------ | ------ | --------- | ----------- | --------------------- |
| 17     | 3.3V   | Power     | +3V         | Power supply          |
| 6      | GND    | Power     | 0V          | Ground                |
| 37     | GPIO26 | Output    | UP          | Raise blinds          |
| 35     | GPIO19 | Output    | STOP        | Stop movement         |
| 33     | GPIO13 | Output    | DOWN        | Lower blinds          |
| 31     | GPIO6  | Output    | SELECT      | Select next blind     |
| 29     | GPIO5  | Output    | PROG        | Optional RTS sync     |
| 40     | GPIO21 | Input     | LED1        | Selection indicator 1 |
| 38     | GPIO20 | Input     | LED2        | Selection indicator 2 |
| 36     | GPIO16 | Input     | LED3        | Selection indicator 3 |
| 32     | GPIO12 | Input     | LED4        | Selection indicator 4 |

#### Wiring Diagram

```
  Raspberry Pi                          Somfy Telis 4
  ┌────────────────┐                   ┌─────────────┐
  │ Pin 17 (3.3V)  │ ────────────────▶ │ +3V         │
  │ Pin 6  (GND)   │ ────────────────▶ │ 0V          │
  │ Pin 37 (GPIO26)│ ────────────────▶ │ UP          │
  │ Pin 33 (GPIO13)│ ────────────────▶ │ DOWN        │
  │ Pin 35 (GPIO19)│ ────────────────▶ │ STOP        │
  │ Pin 31 (GPIO6) │ ────────────────▶ │ SELECT      │
  │ Pin 29 (GPIO5) │ ────────────────▶ │ PROG        │
  │ Pin 40 (GPIO21)│ ◀──────────────── │ LED1        │
  │ Pin 38 (GPIO20)│ ◀──────────────── │ LED2        │
  │ Pin 36 (GPIO16)│ ◀──────────────── │ LED3        │
  │ Pin 32 (GPIO12)│ ◀──────────────── │ LED4        │
  └────────────────┘                   └─────────────┘
```

### GPIO Implementation

#### Output Pulses

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

#### Input Debouncing

The driver watches input lines with edge detection, collecting up to 16 events within a 300ms window:

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

## CC1101 RTS driver

The RTS driver skips the wired remote and transmits Somfy RTS frames directly at 433.42 MHz. Each `Channel` (`L1`–`L4`, `ALL`) is a separate virtual remote with its own 24-bit ID and rolling-code counter persisted to `$STATE_DIRECTORY/rts.json`.

### Wiring

| CC1101 | Raspberry Pi                           | Notes                                    |
| ------ | -------------------------------------- | ---------------------------------------- |
| VCC    | 3.3V only                              | The CC1101 is **not** 5V tolerant.       |
| GND    | GND                                    |                                          |
| SCLK   | SPI0 SCLK / BCM11                      |                                          |
| MOSI   | SPI0 MOSI / BCM10                      |                                          |
| MISO   | SPI0 MISO / BCM9                       |                                          |
| CSN    | SPI0 CE0 / BCM8 (`/dev/spidev0.0`)     |                                          |
| GDO0   | BCM18                                  | Drives the OOK data line in async mode. |

A 433.42 MHz tuned antenna on the CC1101 ANT pad is required for usable range.

### Software path

The Pi drives the CC1101 in async serial OOK mode and uses `pigpiod` waveforms to clock the Somfy pulse train onto GDO0. Each press emits four frames (one initial + three repeats), Manchester-encoded with 640 µs half-symbols.

```
RtsDriver::transmit(channel, command)
  -> reserve rolling code (atomic write to rts.json on block boundaries)
  -> encode 7-byte RTS frame (key, command/checksum, rolling code BE, remote ID LE)
  -> obfuscate (XOR cascade)
  -> build pigpio gpioPulse_t list
  -> CC1101 SRES + STX
  -> WVNEW / WVAG / WVCRE / WVTX, poll WVBSY, WVDEL
  -> CC1101 SIDLE
  -> commit rolling code in memory
```

CC1101, pigpiod TCP, and GDO0 are configured once at driver startup; per-press cost is just waveform upload + transmit. Stale waves from a prior crash are cleared with `WVCLR` during init.

When the resolved config selects the RTS driver, `sudo somfy install`
provisions the runtime dependency by installing the `pigpio` package, writing a
systemd drop-in that starts `pigpiod -l`, and enabling `pigpiod`.

### Configuration

Hardware settings should come from built-in defaults or `/etc/somfy/config.toml`,
not repeated CLI flags or environment variables. Built-in driver defaults are
target-aware: Raspberry Pi Linux builds select `telis`, while local development
and CI-style non-Pi builds select `fake`. A config file always wins.

```toml
driver = "rts"

[rts]
spi_device = "/dev/spidev0.0"
gdo0_gpio = 18
pigpiod_addr = "127.0.0.1:8888"

[telis.gpio]
up = 26
stop = 19
down = 13
select = 6
led1 = 21
led2 = 20
led3 = 16
led4 = 12
# prog = 5
```

`somfy doctor` validates SPI access, GDO0 BCM range, pigpiod reachability,
pigpiod localhost-only mode, and `rts.json` schema.

### Pairing

Each channel is paired independently. With the motor in programming mode (already-paired remote, or motor's prog button):

```bash
sudo somfy remote prog L1
sudo somfy remote up L1   # confirm direction
```

If the Telis remote's Prog button is also wired to the Pi, configure
`telis.gpio.prog`. `somfy remote prog <channel>` then selects the requested
Telis channel, holds Prog, waits briefly, and transmits the RTS Prog frame for
the same virtual channel. Run it again to remove that virtual remote from the
motor.

```bash
sudo somfy remote prog L1
```

`ALL` is a separate virtual remote — pair it with every motor that should react to all-channel commands.

### Bring-up checklist

The CC1101 register set in `src/rts/cc1101.rs` is a starting point and has **not** been validated against a scope or SDR yet. Before relying on it:

1. Confirm motors are Somfy RTS (not io-homecontrol).
2. With `somfy serve` running and the RTS driver selected by config, scope GDO0 during a `somfy remote up L1`. Wake-up should be ~9.4 ms high / ~89.6 ms low; Manchester half-symbols 640 µs.
3. With an SDR (rtl-sdr, HackRF) tuned to 433.42 MHz, verify carrier presence and absence between frames.
4. If pairing fails, capture frames with an existing real Somfy remote and compare obfuscated bytes — the encoder has golden tests, but key-byte values can vary by motor generation.

## Architecture

### Data Flow

```
┌─────────────────────────────────────────────────────────────┐
│                     FRONTEND (Preact/Vite)                  │
│  EventSource connection with browser-managed reconnect      │
│  Displays LED indicators & control buttons                  │
│  Haptic feedback on interactions                            │
└──────────────────────────┬──────────────────────────────────┘
                           │
            SSE (GET /events) + HTTP (POST /command)
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                  BACKEND (Axum/Tokio)                       │
│                                                             │
│  Routes:                                                    │
│  ├─ GET /channel  → Current channel selection               │
│  ├─ POST /command → Execute button commands                 │
│  ├─ GET /events   → Server-sent channel selection events    │
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

The SSE endpoint and WebSocket handler both subscribe to the same
`watch::channel`, which sends the current LED first and then every selection
change. The Preact PWA uses SSE plus `POST /command`; WebSocket remains available
for bidirectional clients. See [ARCHITECTURE.md](ARCHITECTURE.md) for the
transport-level details.

### State Engine: `RemoteControl`

`RemoteControl` is the central coordinator:

1. **Startup:** presses SELECT, reads LEDs, seeds the `watch::channel`.
2. **Methods:** `select()`, `up()`, `down()`, `stop()` trigger GPIO.
3. **Broadcasts:** LED changes propagate to all SSE and WebSocket clients.

## Frontend

The Preact PWA features:

- **Connection status bar:** color-coded (connecting / connected / error).
- **Control buttons:** large circular Up, Stop, Down.
- **LED indicators:** clickable dots for L1–L4; center button for SELECT.
- **Long-press:** sends `ALL` intent for group mode.
- **Haptics:** 100ms on press, 200ms on finish.
- **Auto-reconnect:** browser-managed `EventSource` reconnect.

## Why This Design

- **Single source of truth:** the Pi reads real GPIO state and broadcasts — every UI stays consistent.
- **Low latency:** SSE and WebSocket subscribers get immediate LED feedback.
- **Non-blocking:** async GPIO timing doesn't stall the runtime.
- **Small footprint:** easy to audit, extend, or port to different hardware.
