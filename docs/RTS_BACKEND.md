# RTS Backend Notes

## Goal

Replace the wired physical Somfy Telis 4 RTS remote with Raspberry
Pi-controlled RF transmission. The Pi acts as a new virtual Somfy RTS remote,
paired with each motor/group.

Only one hardware backend is active at a time, selected once at startup: either
the wired Telis backend or the RTS backend. Both are intended to remain
supported long-term so different deployments can pick the right hardware.

## Common Model

Shared vocabulary across both hardware implementations:

- **Channel**: the logical target — `L1`, `L2`, `L3`, `L4`, or `ALL`. In the
  Telis backend this maps to the selected LED. In the RTS backend it maps to a
  virtual remote identity.
- **Command**: the user intent — `Up`, `Down`, `My`, `Stop`, or `Prog`.
- **Transmission**: the backend-specific act that sends a command to a channel.
  In the Telis backend this is a button press on the physical remote. In the
  RTS backend this is an RF waveform.

The existing code calls the target type `Input` because it currently represents
GPIO LED inputs. `Channel` is the better domain term, but the rename can land
separately from the first RTS prototype.

## Backend Selection

Backend selection is runtime configuration, not hot-swapping. The service
initializes one backend and all API/HomeKit calls go through the same command
surface.

```text
RemoteControl
  -> TelisBackend | RtsBackend
```

The behavior split stays inside the selected backend:

```text
TelisBackend:
  transmit(channel=L2, command=Up)
    -> cycle SELECT until LED L2 is active
    -> press UP GPIO

RtsBackend:
  transmit(channel=L2, command=Up)
    -> load L2 virtual remote state
    -> send RTS UP frame
    -> increment and persist L2 rolling code
```

Live switching between backends is not supported — it would add coordination
and state questions without real benefit.

Compile-time features control which backend implementations are included in the
binary; runtime config selects exactly one active backend from the compiled set.

```toml
[features]
default = ["fake"]
fake = []
telis = ["dep:gpiocdev"]
rts = ["dep:spidev"]
```

The current `hw` feature should not remain. Use explicit backend names so the
binary's hardware capabilities are clear from the build command.

If runtime config selects a backend that was not compiled in, startup should
fail clearly:

```text
backend "rts" was selected, but this binary was built without the "rts" feature
```

## Requirements

- Confirm devices are Somfy RTS, not io-homecontrol.
- Use a CC1101 transceiver module on 433.42 MHz, driven over SPI. CC1101
  handles modulation in hardware — the Pi only feeds frame bytes, no
  microsecond pulse timing.
- Generate valid Somfy RTS 56-bit frames.
- Maintain one virtual remote identity per channel (`remote_id` +
  `rolling_code`).
- Persist rolling codes atomically with a write-ahead buffer (see below).
- Add a pairing flow using `PROG`.

**Rolling code persistence.** Each frame increments the on-wire rolling code
by `+1` (Somfy protocol requirement). To survive crashes and limit SD-card
writes, persist `next_code + N` to disk up front and serve codes from RAM;
re-persist only after burning through the buffer. A crash then loses at most
`N` codes, which the motor's acceptance window absorbs without desync. Losing
or rolling back the *persisted* value below the motor's last-seen code makes
the virtual remote stop working until re-paired.

Minimum viable command set: `Up`, `Down`, `My / Stop`, `Prog`. One virtual
remote per channel (`L1`–`L4`, `ALL`), persisted as:

```json
{
  "L1": { "id": 12345, "rolling_code": 1 },
  "L2": { "id": 12346, "rolling_code": 1 },
  "L3": { "id": 12347, "rolling_code": 1 },
  "L4": { "id": 12348, "rolling_code": 1 },
  "ALL": { "id": 12349, "rolling_code": 1 }
}
```

## Transmitter Notes

Drive a CC1101 module over SPI (`spidev`) in FIFO mode. CC1101 handles
433.42 MHz OOK modulation in hardware; the Pi configures the radio, writes
the Manchester-encoded frame bytes to the TX FIFO, and CC1101 transmits
autonomously. No GPIO bit-banging, no microsecond waveform generation, no
`pigpiod` dependency.

Configuration steps at backend startup:

- Open `/dev/spidev0.0` and configure CC1101 registers for 433.42 MHz, OOK,
  ~1 kbps Manchester-like timing matching Somfy RTS.
- Use ESPSomfy-RTS register values as the reference — they are validated
  against real motors.

Per-transmission steps:

- Build the 7-byte RTS frame (encoder module).
- Write the wake-up burst, hardware sync, software sync, and Manchester-coded
  payload to the CC1101 TX FIFO.
- Repeat the frame the standard number of times with the inter-frame gap.

Serialize transmissions behind a mutex (one radio, one in-flight frame). The
wired Telis backend keeps using `gpiocdev`.

## Test Strategy

Most of the backend can be tested before RF hardware arrives. Hardware testing
should only need to validate RF range, wiring, pairing, and final timing.

**Golden tests** (frame encoding):

- Given a fixed command, remote ID, and rolling code, assert the generated
  7-byte RTS frame matches known output from PushStack/homebridge-rpi-rts.
- Assert checksum and obfuscation are stable.
- Assert remote address byte order and rolling-code byte order are correct.
- Convert the frame into pulses and assert wake-up pulse, hardware sync,
  software sync, Manchester bit count, inter-frame gap, and repeat count.

**Transmission abstraction** (integration tests):

```rust
trait Transmission {
    async fn send(&self, pulses: &[Pulse]) -> Result<()>;
}
```

```text
RecordingTransmission -> stores pulses in memory for tests
LoggingTransmission   -> prints/debugs frames locally
Cc1101Transmission    -> sends via CC1101 over SPI on real Raspberry Pi hardware
```

**Persistence tests**:

- Missing state initialization.
- Independent rolling codes per logical channel.
- Rolling code increments after a successful transmit.
- Failed-transmit behavior is intentional and tested.
- State writes are atomic.

Optional debug command for comparing output against reference implementations:

```text
somfy rts dump L1 up --format json
```

## Implementation Path

Hardware-on-Pi testing happens late — most of the work is offline-verifiable.
Renames (`Input`→`Channel`, `Output`→`TelisButton`, `RemoteControl`→
`Controller`) are housekeeping that can land alongside whichever phase is
convenient.

1. **Encoder + golden tests.** Rust RTS frame encoder. Golden tests for frame
   bytes against PushStack/homebridge-rpi-rts known-good output.
2. **Persistence.** RTS state under `/var/lib/somfy`, separate from HomeKit
   state. Atomic writes with the write-ahead-buffer scheme. Persistence tests
   for rolling-code behavior.
3. **Transmission abstraction + CLI.** `Transmission` trait with recording and
   logging implementations. Tiny CLI:

   ```text
   somfy rts dump L1 up --format json
   somfy rts prog L1
   somfy rts send L1 up | down | my
   ```

4. **CC1101 driver.** SPI register setup (cribbed from ESPSomfy-RTS) and
   frame-to-FIFO transmission. No physical hardware needed for the driver
   code itself — it can be exercised with a recording/logging fake until a
   Pi with CC1101 is available for end-to-end pairing.
5. **Backend selection + cleanup.** Add startup-time `backend = "telis" | "rts"`
   config. Wire RTS backend into `RemoteControl`. Replace the `hw` feature with
   explicit `fake` / `telis` / `rts` features. Update hardware/install docs,
   including CC1101 wiring and `spidev` enablement.

## Best Resources

1. PushStack Somfy RTS Protocol —
   <https://pushstack.wordpress.com/somfy-rts-protocol/>

   Best protocol explanation: frame format, checksum, obfuscation, timings,
   rolling code, button codes.

2. homebridge-rpi-rts —
   <https://github.com/wibberryd/homebridge-rpi-rts/blob/master/RpiGpioRts.js>

   Best minimal implementation. Shows the basic flow: load rolling code, build
   frame, send waveform, increment and save.

3. ESPSomfy-RTS — <https://github.com/rstrouse/ESPSomfy-RTS>

   Best mature reference, and the source for CC1101 register values. Useful
   for robust RTS command handling, pairing flows, and edge cases. Do not
   start by porting it — much larger than needed.
