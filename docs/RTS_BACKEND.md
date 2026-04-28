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
  RTS backend this is an RF frame transmitted via CC1101.

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
rts = ["dep:spidev", "dep:gpiocdev"]  # gpiocdev for the CC1101 data pin; timing crate TBD
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
- Use a CC1101 transceiver module as the 433.42 MHz RF frontend, configured
  over SPI. The data-path strategy (CC1101 packet/FIFO engine vs. async mode
  with GPIO-driven waveform on the data pin) is unvalidated for Somfy RTS —
  see Transmitter Notes.
- Generate valid Somfy RTS 56-bit frames.
- Maintain one virtual remote identity per channel (`remote_id` +
  `rolling_code`).
- Persist rolling codes atomically with a write-ahead buffer (see below).
- Add a pairing flow using `PROG`.

**Rolling code persistence.** Each frame increments the on-wire rolling code
by `+1` (Somfy protocol requirement). To survive crashes and limit SD-card
writes, persist `next_code + N` to disk up front and serve codes from RAM;
re-persist only after burning through the buffer. ESPSomfy uses `N = 16`,
which is a sensible default. A crash then loses at most `N` codes, which the
motor's acceptance window absorbs without desync. Losing or rolling back the
*persisted* value below the motor's last-seen code makes the virtual remote
stop working until re-paired.

Minimum viable command set: `Up`, `Down`, `My / Stop`, `Prog`. One virtual
remote per channel (`L1`–`L4`, `ALL`), persisted as below. IDs are example
values — generate randomly per install to avoid collision with other RTS
networks in range.

```json
{
  "L1": { "id": 12345, "rolling_code": 1 },
  "L2": { "id": 12346, "rolling_code": 1 },
  "L3": { "id": 12347, "rolling_code": 1 },
  "L4": { "id": 12348, "rolling_code": 1 },
  "ALL": { "id": 12349, "rolling_code": 1 }
}
```

CC1101 is the 433.42 MHz RF frontend, configured over SPI (`spidev`). How the
RTS waveform is fed to it is the open question.

**Two candidate data paths:**

1. **CC1101 packet/FIFO engine.** Configure CC1101 for OOK + Manchester at
   ~1 kbps, write the encoded bytes to the TX FIFO, let the radio handle air
   timing. Simplest if it works. Risk: CC1101's packet engine has fixed
   preamble/sync formats and may not reproduce the Somfy wake-up burst,
   hardware-sync, software-sync, and inter-frame-gap timings exactly.
2. **CC1101 async mode + GPIO-driven waveform.** Configure CC1101 as a dumb
   OOK transmitter; the Pi drives the data pin with precise microsecond
   timing for the wake-up burst, sync pulses, Manchester payload, and gaps.
   This is what real-world implementations (ESPSomfy-RTS and similar) do.
   Requires a precise timing mechanism — `pigpiod` waveforms via `apigpio`,
   or an equivalent — because `gpiocdev` alone is too coarse.

**Timing tolerance.** PushStack reports motors accept pulses 20% longer or
15% shorter than nominal. With a 640us base pulse that is ±~100us of slack,
which changes the calculus:

- Path 1 (FIFO) is more plausible than worst-case analysis suggests — the
  motor will likely accept CC1101 packet-engine output even if it doesn't
  exactly reproduce the Somfy preamble shape.
- Path 2 (GPIO async) does not strictly require `pigpiod`'s DMA precision.
  User-space timing on a quiet Pi can hit ±100us; `pigpiod` remains the
  safest choice under scheduler load (HTTP server, HomeKit, etc. all share
  the process), but it is not the only viable mechanism.

Plan path 2 as the default and use `pigpiod` if the simpler timing approach
proves jittery under realistic load. Drop to path 1 only if a hardware spike
confirms it works against real motors.

**At backend startup, regardless of path:**

- Open `/dev/spidev0.0` and configure CC1101 registers for 433.42 MHz OOK.
  Use ESPSomfy-RTS register values as the reference — they are validated
  against real motors.

**Per transmission:**

- Build the 7-byte RTS frame (encoder module).
- Wrap with wake-up burst, hardware/software sync, Manchester encoding, and
  repeat-with-gap.
- Push to CC1101 via the chosen data path.

Serialize transmissions behind a mutex (one radio, one in-flight frame). The
wired Telis backend keeps using `gpiocdev`.

## Test Strategy

Most of the backend can be tested before RF hardware arrives. Hardware testing
should only need to validate RF range, wiring, and pairing.

**Golden tests** (frame encoding):

- Given a fixed command, remote ID, and rolling code, assert the generated
  7-byte RTS frame matches known output from PushStack/homebridge-rpi-rts.
  Checksum, obfuscation, and byte order are covered by the vectors.

If we end up on the GPIO-driven data path (likely — see Transmitter Notes),
the wake-up burst, sync pulses, Manchester encoding, and repeat-with-gap are
also our code and worth a structural test (right pulse counts, right gap
durations) — but not byte-for-byte golden, since timing tolerances exist.
SPI/FIFO mechanics and CC1101 register writes stay untested at the unit
level; they only prove themselves on real hardware.

**Transmission abstraction** (integration tests):

```rust
trait Transmission {
    async fn send(&self, frame: &[u8]) -> Result<()>;
}
```

```text
RecordingTransmission -> stores frames in memory for tests
LoggingTransmission   -> prints/debugs frames locally
Cc1101Transmission    -> sends via CC1101 over SPI on real Raspberry Pi hardware
```

**Persistence tests**:

- Missing state initialization.
- Independent rolling codes per logical channel.
- Rolling code increments after a successful transmit.
- Failed-transmit behavior is intentional and tested.
- State writes are atomic.

## Implementation Path

Hardware-on-Pi testing happens late — most of the work is offline-verifiable.
Renames (`Input`→`Channel`, `Output`→`TelisButton`, `RemoteControl`→
`Controller`) are housekeeping that can land alongside whichever phase is
convenient.

1. **Encoder + golden tests.** Rust RTS frame encoder. Golden tests for frame
   bytes against PushStack/homebridge-rpi-rts known-good output. PushStack
   notes the lower 4 bits of the key byte can be a constant zero instead of
   being derived from the rolling code — pick the simpler form unless a
   golden vector forces otherwise.
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

4. **CC1101 driver.** SPI register setup (cribbed from ESPSomfy-RTS). Build
   both data paths behind the `Transmission` trait if cheap; otherwise start
   with the GPIO-driven path (path 2) as the safe default. Decide between
   FIFO and GPIO-driven during the first hardware spike. The driver code
   itself can be exercised against recording/logging fakes until a Pi with
   CC1101 is available for end-to-end pairing.
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

   Best mature reference, and the source for CC1101 register values. Also
   the reference for the GPIO-driven data path (CC1101 in async mode + timed
   waveform on the data pin). Useful for robust RTS command handling,
   pairing flows, and edge cases. Do not start by porting it — much larger
   than needed.

4. pigpio / `pigpiod_if2` — <https://github.com/joan2937/pigpio>

   If the GPIO-driven CC1101 data path is the one we end up using, this is
   the Raspberry Pi timing primitive. Drive it via `apigpio` (async Rust
   client) or a small local FFI wrapper.
