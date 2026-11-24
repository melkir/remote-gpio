# Hardware Notes

A deeper look at the two physical setups `somfy` supports — the wired Telis 4 driver and the CC1101 RTS radio driver — and how each turns hardware events into synchronized UI state. For a broader codebase tour, see [ARCHITECTURE.md](ARCHITECTURE.md).

## Telis 4 driver

### Raspberry Pi ↔ Somfy Telis 4

The Telis path treats the physical remote as the source of truth. The Pi taps the Up/Stop/Down/Select contacts and watches the four LED lines to learn which channel is selected. It does not use the remote for RTS pairing; `prog` is an RTS-driver command.

- **Outputs (Pi → Somfy):** simulate button presses (active-low pulses).
- **Inputs (Somfy → Pi):** read the LED selection state.
- **Power:** shared 3.3V and GND — no level shifting needed.

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

Outputs are active-low button taps. The driver asserts the configured GPIO for about 60 ms, then releases it, which is long enough for the Telis remote to see a press without holding the button.

#### Input Debouncing

The driver watches LED lines with edge detection, collecting up to 16 events within a 300 ms window (`gpio::watch_inputs`):

- **16+ edges in 300 ms:** selection is `ALL` (group mode — LEDs blink).
- **Otherwise:** the last edge maps to `L1`–`L4` via `channel_from_gpio`.

The Telis remote has four LEDs, not a separate fifth `ALL` line. The software models the blinking group pattern as `Channel::All`, which is why the API and HomeKit can expose `ALL` like any other target even though the wired remote only exposes it as LED activity.

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

The defaults match this table (`rts.gpio.gdo0 = 18`, `rts.spi_device = "/dev/spidev0.0"`); override them in config if your wiring differs. How a press becomes RF (rolling-code reserve, frame encoding, pigpiod waveform) is covered in [RTS_DRIVER.md](RTS_DRIVER.md), so this page stays focused on setup.

### Configuration

Settings live in `/etc/somfy/config.toml` (or `--config`). Built-in defaults depend on the build target; anything in the config file wins.

```bash
sudo somfy config set-driver rts   # switch driver, set up pigpiod, restart
somfy config show                  # resolved TOML after validation
```

`somfy doctor` checks deployment health (systemd unit, GPIO access, updates, deployed SHA) and driver-specific probes (SPI, GDO0, pigpiod on loopback port `8888`, `rts.json`).

### Pairing

Pairing requires the RTS driver. The Telis driver does not implement `prog`: with the wired Telis setup, pair motors using a physical Somfy remote.

Each channel is paired independently. There are two flows, depending on whether you already have a paired remote.

**Adding the Pi as a new remote (recommended).** Long-press the PROG button on an already-paired remote until the motor jogs (~5 s). Then send the command:

```bash
somfy remote prog L1
```

**Pi as the master remote.** When you do not have another paired remote, the Pi has to put the motor into pair-listen itself. Use `--long`, which extends the burst to 20 frames:

```bash
somfy remote prog L1 --long
```

The motor jogs to acknowledge it has registered the channel. Run the same command again to unregister.

`ALL` is a separate virtual remote — pair it with every motor that should react to all-channel commands.

```bash
for channel in L1 L2 L3 L4; do somfy remote prog "$channel" --long; done; somfy remote prog ALL
```

For the protocol-level RTS reference (frame format, checksum, obfuscation, waveform timings, pigpiod commands), see [RTS_DRIVER.md](RTS_DRIVER.md).

## Timed positioning

Neither driver can read the blind's physical position, so percentages are estimated from per-blind travel times. They drive the PWA and API `target` command as well as HomeKit. The default is 10 seconds each way for every blind; time your blinds and set the real values (in **seconds**; restarts the service unless `--no-restart`):

```bash
sudo somfy config set-positioning L1 --open 8.5 --close 9.2 --slack 2.7
```

That writes the equivalent TOML (in **milliseconds**) to `/etc/somfy/config.toml`:

```toml
[positioning.l1]
open_ms = 8500
close_ms = 9200
slack_ms = 2700
```

How a move is planned:

- **Endpoint targets** (`0` or `100`) just send `down` or `up` and rely on the motor's own limits.
- **Interior targets** (`1`-`99`) send `up` or `down`, then `stop` after the proportional share of the travel time.
- **`slack_ms`** (optional, default `0`) is closed-end slack included in the full-travel times: the planner subtracts it from proportional travel, then adds it back only for upward moves that start from `0`.
- **All four blinds** moving the same direction in one request start with a single `ALL` command, then get individual `stop`s at each blind's own completion time.
