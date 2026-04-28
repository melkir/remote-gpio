# RTS Backend Notes

## Goal

Replace the wired physical Somfy Telis 4 RTS remote with Raspberry
Pi-controlled RF transmission.

The Pi acts as a new virtual Somfy RTS remote, paired with each motor/group.

Only one hardware backend needs to be active at a time. Backend selection should
happen once at startup: either the existing wired Telis backend or the new RTS
backend.

## Common Model

Use shared vocabulary above both hardware implementations:

- **Channel**: the logical target, currently `L1`, `L2`, `L3`, `L4`, or `ALL`.
  In the Telis backend this maps to the selected LED. In the RTS backend this
  maps to a virtual remote identity.
- **Command**: the user intent, such as `Up`, `Down`, `My`, `Stop`, or `Prog`.
- **Transmission**: the backend-specific act that sends a command to a channel.
  In the Telis backend this is a button press on the physical remote. In the RTS
  backend this is an RF waveform.

The existing code calls the target type `Input` because it currently represents
GPIO LED inputs. For the RTS migration, `Channel` is the better domain term. The
rename can be done separately from the first RTS prototype if keeping the diff
small is useful.

## Core Requirements

- Confirm devices are Somfy RTS, not io-homecontrol.
- Add RF hardware capable of 433.42 MHz, preferably CC1101.
- Generate valid Somfy RTS 56-bit frames.
- Maintain one virtual remote identity per channel/group:
  - `remote_id`
  - `rolling_code`
- Persist rolling codes atomically and back them up.
- Add a pairing flow using `PROG`.
- Select exactly one backend at startup:
  - `telis`: current wired physical remote.
  - `rts`: virtual RTS remote identities and RF waveform transmission.
- Keep the current wired Telis backend available as a fallback during migration.

## Important Constraints

- Rolling code increments by `+1`.
- Losing or rolling back the stored rolling code can make the virtual remote
  stop working.
- Timing/RF reliability is the main implementation risk on Raspberry Pi.
- Normal GPIO APIs such as `gpiocdev` are appropriate for the current
  physical-remote button presses, but not for microsecond-level RTS waveform
  transmission.

## Recommended Minimal Scope

Implement only:

```text
Up
Down
My / Stop
Prog
```

Use one virtual remote per logical channel:

```text
L1
L2
L3
L4
ALL
```

Store something like:

```json
{
  "L1": { "id": 12345, "rolling_code": 1 },
  "L2": { "id": 12346, "rolling_code": 1 },
  "L3": { "id": 12347, "rolling_code": 1 },
  "L4": { "id": 12348, "rolling_code": 1 },
  "ALL": { "id": 12349, "rolling_code": 1 }
}
```

## Transmitter Interface

Use `pigpiod` waveform generation for the first Raspberry Pi RTS transmission
implementation.

The current wired Telis backend can keep using `gpiocdev`. It only needs
millisecond-scale button presses and LED edge watching. The RTS backend needs
precise pulse timing around `640us`, `2560us`, and `4550us`, so it should build
the full waveform and hand it to `pigpiod`.

Use `apigpio` as the first learning/prototyping interface if it works on the
target Pi. It exposes the right concept for this project: an async Rust client
for `pigpiod` waveform generation. Even though it has not been actively
maintained recently, the underlying pigpio waveform API is stable enough that it
may still be practical.

Before committing to it long-term, audit the crate:

- Does it expose the exact wave operations needed by RTS?
- Is its GPL-3.0 license acceptable for this project?
- Is the implementation small enough to understand and debug?
- Does it add meaningful risk compared with a local wrapper?

If the audit or prototype exposes problems, replace it with a tiny internal Rust
wrapper around the `pigpiod_if2` C API. A local wrapper would keep the surface
small and explicit.

Required `pigpiod_if2` surface:

```c
pigpio_start
pigpio_stop
set_mode
gpio_write
wave_clear
wave_add_generic
wave_create
wave_send_once
wave_tx_busy
wave_delete
```

Required pulse shape:

```c
typedef struct {
  uint32_t gpioOn;
  uint32_t gpioOff;
  uint32_t usDelay;
} gpioPulse_t;
```

The safe Rust wrapper can stay narrow:

```rust
#[repr(C)]
struct GpioPulse {
    gpio_on: u32,
    gpio_off: u32,
    us_delay: u32,
}

struct Pigpio {
    pi: i32,
}
```

`send_waveform` should serialize access and run:

```text
wave_clear
wave_add_generic
wave_create
wave_send_once
poll wave_tx_busy until done
wave_delete
```

Pigpio waveform state is global inside `pigpiod`, so RTS transmission must be
guarded by a mutex. Use `wave_clear` at backend startup to clean stale waveform
state from a previous run.

## Backend Selection

Backend selection should be runtime configuration, not hot-swapping. The service
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

Do not support live switching between backends. It would add coordination and
state questions without helping the migration.

Compile-time features should control which backend implementations and
dependencies are included in the binary. Runtime config then selects exactly one
active backend from the compiled set.

Final feature shape:

```toml
[features]
default = ["fake"]
fake = []
telis = ["dep:gpiocdev"]
rts = ["dep:apigpio"]
```

Expected builds:

```text
cargo build --release --no-default-features --features telis
cargo build --release --no-default-features --features rts
cargo build --release --no-default-features --features telis,rts
cargo build
```

The `hw` feature should not remain in the final model. Use explicit backend
feature names instead, so the binary's hardware capabilities are clear from the
build command.

If runtime config selects a backend that was not compiled in, startup should
fail clearly:

```text
backend "rts" was selected, but this binary was built without the "rts" feature
```

## Best Resources

1. PushStack Somfy RTS Protocol
   <https://pushstack.wordpress.com/somfy-rts-protocol/>

   Best protocol explanation: frame format, checksum, obfuscation, timings,
   rolling code, button codes.

2. homebridge-rpi-rts
   <https://github.com/wibberryd/homebridge-rpi-rts/blob/master/RpiGpioRts.js>

   Best minimal implementation. Shows the exact basic flow:

   - load rolling code from file
   - build RTS frame
   - send waveform
   - increment and save rolling code

3. ESPSomfy-RTS
   <https://github.com/rstrouse/ESPSomfy-RTS>

   Best mature implementation/reference. Useful for:

   - CC1101 setup
   - robust RTS command handling
   - pairing flows
   - practical edge cases

4. pigpio / `pigpiod_if2`
   <https://github.com/joan2937/pigpio>

   Best Raspberry Pi timing primitive for this backend. Use the daemon waveform
   API through a small local FFI wrapper.

Do not start by porting all of ESPSomfy. It is much larger than needed.

## Test Strategy

Most of the backend can be tested before RF hardware is available. The hardware
test should only need to validate RF range, wiring, pairing, and final timing.

Start with golden tests:

- Given a fixed command, remote ID, and rolling code, assert the generated
  7-byte RTS frame matches known output from PushStack/homebridge-rpi-rts.
- Assert checksum and obfuscation are stable.
- Assert remote address byte order and rolling-code byte order are correct.
- Convert the frame into pulses and assert wake-up pulse, hardware sync,
  software sync, Manchester bit count, inter-frame gap, and repeat count.

Use a transmission abstraction for integration tests:

```rust
trait Transmission {
    async fn send(&self, pulses: &[Pulse]) -> Result<()>;
}
```

Initial implementations:

```text
RecordingTransmission -> stores pulses in memory for tests
LoggingTransmission   -> prints/debugs frames locally
PigpioTransmission    -> sends via pigpiod on real Raspberry Pi hardware
```

State persistence tests should cover:

- Missing state initialization.
- Independent rolling codes per logical channel.
- Rolling code increments after a successful transmit.
- Failed transmit behavior is intentional and tested.
- State writes are atomic.

An optional debug command can help compare output with reference
implementations:

```text
somfy rts dump L1 up --format json
```

## Suggested Implementation Path

1. Add an RTS encoder module in Rust.
2. Add golden tests for frame encoding and waveform generation.
3. Add persistent RTS state under `/var/lib/somfy`, separate from HomeKit state.
4. Add persistence tests for rolling-code behavior.
5. Add a transmission abstraction with recording/logging implementations.
6. Add a tiny CLI first:

```text
somfy rts dump L1 up --format json
somfy rts prog L1
somfy rts send L1 up
somfy rts send L1 down
somfy rts send L1 my
```

7. Prototype real waveform transmission with `apigpio`.
8. Test pairing and movement on one blind.
9. Audit `apigpio` and decide whether to keep it or replace it with a small
   `pigpiod_if2` wrapper.
10. Add startup-time backend selection:

```text
backend = "telis" | "rts"
```

11. Rename `Input` to `Channel`.
12. Rename `Output` to `TelisButton` when extracting Telis-specific internals.
13. Add explicit `fake`, `telis`, and `rts` compile-time features.
14. Remove the generic `hw` feature in the final feature model.
15. Wire RTS backend into existing `RemoteControl`.
16. Optionally rename `RemoteControl` to `Controller` once the backend boundary
   is clear.
17. Update hardware/install docs, including the `pigpiod` service/library
   dependency.

## Pragmatic Take

This is a good upgrade path. It removes the fragile wired remote and simplifies
hardware long-term, while preserving the existing app/HomeKit work. The hard
part is RF reliability and rolling-code persistence, not the Somfy protocol
itself.
