# Hardware Notes

A deeper look at the two physical setups `somfy` supports — the wired Telis 4 driver and the CC1101 RTS radio driver — and how each turns hardware events into synchronized UI state. For a broader codebase tour, see [ARCHITECTURE.md](ARCHITECTURE.md).

## Telis 4 driver

### Raspberry Pi ↔ Somfy Telis 4

The Telis path treats the physical remote as the source of truth. The Pi taps
the Up/Stop/Down/Select contacts and watches the four LED lines to learn which
channel is selected. It does not use the remote for RTS pairing; `prog` is an
RTS-driver command.

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
  │ Pin 40 (GPIO21)│ ◀──────────────── │ LED1        │
  │ Pin 38 (GPIO20)│ ◀──────────────── │ LED2        │
  │ Pin 36 (GPIO16)│ ◀──────────────── │ LED3        │
  │ Pin 32 (GPIO12)│ ◀──────────────── │ LED4        │
  └────────────────┘                   └─────────────┘
```

### GPIO Implementation

#### Output Pulses

Outputs are active-low button taps. The driver asserts the configured GPIO for
about 60 ms, then releases it, which is long enough for the Telis remote to see a
press without holding the button. Implementation: `gpio::trigger_output` in
`src/gpio.rs` (maps `TelisButton` to BCM pins from config).

#### Input Debouncing

The driver watches LED lines with edge detection, collecting up to 16 events within a 300 ms window (`gpio::watch_inputs`):

- **16+ edges in 300 ms:** selection is `ALL` (group mode — LEDs blink).
- **Otherwise:** the last edge maps to `L1`–`L4` via `channel_from_gpio`.

## CC1101 RTS driver

The RTS driver skips the wired remote and transmits Somfy RTS frames directly at 433.42 MHz. Each `Channel` (`L1`–`L4`, `ALL`) is a separate virtual remote with its own 24-bit ID and rolling-code counter persisted to `$STATE_DIRECTORY/rts.json`.

### Wiring

| CC1101 | Raspberry Pi                       | Notes                                   |
| ------ | ---------------------------------- | --------------------------------------- |
| VCC    | 3.3V only                          | The CC1101 is **not** 5V tolerant.      |
| GND    | GND                                |                                         |
| SCLK   | SPI0 SCLK / BCM11                  |                                         |
| MOSI   | SPI0 MOSI / BCM10                  |                                         |
| MISO   | SPI0 MISO / BCM9                   |                                         |
| CSN    | SPI0 CE0 / BCM8 (`/dev/spidev0.0`) |                                         |
| GDO0   | BCM18                              | Drives the OOK data line in async mode. |

A 433.42 MHz tuned antenna on the CC1101 ANT pad is required for usable range.

### Software path

`RtsDriver::transmit` follows the same high-level sequence for every press:

1. Reserve the next rolling code for the target channel.
2. Encode and obfuscate the 7-byte RTS frame.
3. Build a pigpiod waveform on GDO0.
4. Put the CC1101 into TX while pigpiod clocks the pulse train.
5. Commit the in-memory rolling code only after a successful transmission.

Frame layout, Manchester timings, and pigpiod commands are kept in
[RTS_DRIVER.md](RTS_DRIVER.md) so this hardware page can stay focused on setup.

CC1101, pigpiod, and GDO0 are initialized once at driver startup (`WVCLR` clears
stale waves). With `driver = "rts"`, `sudo somfy install` provisions `pigpiod -l`
(loopback only).

### Configuration

Settings live in `/etc/somfy/config.toml` (or `--config`). Built-in defaults are
target-aware: Pi Linux → `telis`, other targets → `fake`. A config file always
wins, and `somfy config set-driver rts` is the preferred way to switch because
it also installs/configures RTS prerequisites.

```bash
somfy config show   # resolved TOML after validation
somfy config path
```

`somfy doctor` checks deployment health (systemd unit, GPIO access, updates, deployed
SHA) and driver-specific probes (SPI, GDO0, pigpiod on loopback port `8888`,
`rts.json`). pigpiod is unauthenticated — it must stay loopback-only
(`pigpiod -l`).

### Pairing

Pairing requires `driver = "rts"` in config (`somfy config set-driver rts`). The
Telis driver does not implement `prog`; if you are using the wired Telis setup,
put the motor into pair-listen with a physical Somfy remote. If the Pi is meant
to be the master RTS remote, use `--long`.

Each channel is paired independently. There are two useful flows depending on
whether you already have a paired remote.

**Adding the Pi as a new remote (recommended).** Long-press the PROG button on an already-paired remote until the motor jogs (~5 s). Then within 2 minutes:

```bash
somfy remote prog L1
somfy remote up L1   # confirm direction
```

The short 4-frame `prog` is enough — the motor is already in pair-listen.

**Pi as the only / master remote.** When you do not have another paired remote,
the Pi has to put the motor into pair-listen itself. Use `--long`, which extends
the burst to 20 frames:

```bash
somfy remote prog L1 --long
somfy remote up L1   # confirm direction
```

The motor jogs to acknowledge it has registered the channel. Run the same command
again to unregister.

`somfy remote prog <channel>` sends the RTS Prog frame for that virtual channel.
With `driver = "telis"`, the CLI and service return an error pointing you at the
RTS driver.

`ALL` is a separate virtual remote — pair it with every motor that should react to all-channel commands.

### Bring-up checklist

The CC1101 register set in `src/rts/cc1101.rs` is a starting point and has **not** been validated against a scope or SDR yet. Before relying on it:

1. Confirm motors are Somfy RTS (not io-homecontrol).
2. With `somfy serve` running and the RTS driver selected by config, scope GDO0 during a `somfy remote up L1`. Wake-up should be ~9.4 ms high / ~89.6 ms low; Manchester half-symbols 640 µs.
3. With an SDR (rtl-sdr, HackRF) tuned to 433.42 MHz, verify carrier presence and absence between frames.
4. If pairing fails, capture frames with an existing real Somfy remote and compare obfuscated bytes — the encoder has golden tests, but key-byte values can vary by motor generation.

## Data Flow

```
┌─────────────────────────────────────────────────────────────┐
│                     FRONTEND (Preact / Vite PWA)            │
│  EventSource (browser-managed reconnect)                    │
│  Channel indicators (L1–L4 / ALL) + Up / Stop / Down        │
└──────────────────────────┬──────────────────────────────────┘
                           │
            SSE (GET /events) + HTTP (POST /command)
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                  BACKEND (Axum / Tokio)                     │
│                                                             │
│  Routes:                                                    │
│  ├─ GET  /channel  → currently-selected channel (text)      │
│  ├─ POST /command  → execute up/down/stop/select/prog       │
│  ├─ GET  /events   → SSE: selection updates                 │
│  ├─ GET  /ws       → WebSocket: bidirectional API           │
│  └─ /*             → embedded Preact PWA                    │
│                                                             │
│  RemoteControl → CommandRouter (fake / telis / rts)         │
│   broadcasts the selected Channel via watch::channel        │
└──────────────────────────┬──────────────────────────────────┘
                           │
              ┌────────────┴────────────┐
              │                         │
       gpiocdev (Linux)            spidev + pigpiod
              │                         │
┌─────────────▼────────────┐ ┌──────────▼──────────────────────┐
│ Telis 4 wired remote     │ │ CC1101 OOK @ 433.42 MHz         │
│ Outputs: Up/Stop/Down/   │ │ Pi drives GDO0 with the full    │
│   Select (60 ms pulses)  │ │ Somfy pulse train (Manchester,  │
│   active-low pulses)     │ │ 640 µs half-symbols, 4 frames). │
│ Inputs: LED1–4 with      │ │ Per-channel virtual remote ID + │
│   300 ms edge debounce   │ │ rolling code in rts.json.       │
└──────────────────────────┘ └─────────────────────────────────┘
```

For the protocol-level RTS reference (frame format, checksum, obfuscation, waveform timings, pigpiod commands), see [RTS_DRIVER.md](RTS_DRIVER.md).
