# RTS Backend Design

## Goal

Replace the wired physical Somfy Telis 4 RTS remote with Raspberry
Pi-controlled RF transmission. The Pi acts as a new virtual Somfy RTS remote
paired with each motor or group.

The wired Telis backend remains supported. Only one backend is active per
process, selected at startup by runtime configuration and gated by compile-time
features.

## Backend Model

Use the same command surface for the web API, HomeKit, and CLI:

- **Channel**: logical target, `L1`, `L2`, `L3`, `L4`, or `ALL`.
- **Command**: user intent, `Up`, `Down`, `Stop`, or `Prog`.
- **Backend**: hardware implementation that transmits a command to a channel.

The current code calls the target type `Input` because it represents GPIO LED
inputs in the Telis backend. Rename it to `Channel` before adding the RTS
backend so both hardware implementations share the correct domain vocabulary
from the start.

```text
RemoteControl
  -> ActiveBackend::Fake | ActiveBackend::Telis | ActiveBackend::Rts
```

The active backend exposes two execution shapes plus selection state:

```rust
pub struct CommandOutcome {
    pub inferred_position: Option<u8>,
}

impl ActiveBackend {
    /// Stateful path: directs `command` at the current `selected_channel`.
    /// Used by the HTTP `/command` handler. `Select` mutates selection.
    async fn execute(&self, command: Command) -> Result<CommandOutcome>;

    /// Direct path: directs `command` at `channel` without consulting or
    /// mutating `selected_channel`. Used by HomeKit. Does not emit a
    /// `selection` event.
    async fn execute_on(&self, channel: Channel, command: Command) -> Result<CommandOutcome>;

    fn selected_channel(&self) -> Channel;
    fn subscribe_selected_channel(&self) -> SelectedChannelRx;
}
```

Both backends maintain a `selected_channel` and dispatch movement commands
(`Up`, `Down`, `Stop`) through `execute` to that channel. `Select` is a regular
command on the stateful path. On Telis it cycles the physical SELECT button
until the requested LED is active; on RTS it is a zero-RF state update that
mutates `selected_channel` and broadcasts a `selection` event.

`Prog` requires an explicit channel at the API boundary. The service selects
that channel first, then runs backend-native `Prog` through the stateful path.
Backends decide whether they support it.

Call paths, outside in:

| Caller      | Entry point           | Touches `selected_channel`? |
| ----------- | --------------------- | --------------------------- |
| HTTP selected command | `POST /command` without `channel` | yes (via `execute`) |
| HTTP channel command  | `POST /command` with `channel`    | yes (selects first) |
| HomeKit               | `execute_on(ch, cmd)`             | no                  |

### Position Inference

`CommandOutcome::inferred_position` is computed by `RemoteControl`, not by
the backend. Backends only signal "I sent `Up`/`Down`/`Stop` to channel X"; the
position estimator owns the time-based model and the per-channel state.

`ALL` fans out at the position-tracking layer: a successful `Up`/`Down`
to `ALL` updates the inferred position for every channel paired to react to
all-channel commands (in practice, `L1` through `L4`), matching what the
physical remote does over the air. The backend still emits exactly one frame
on the wire.

Telis behavior stays physical:

```text
execute_on(channel=L2, command=Up)
  -> cycle SELECT until LED L2 is active
  -> press UP GPIO
```

RTS behavior is virtual:

```text
execute_on(channel=L2, command=Up)
  -> load L2 virtual remote identity
  -> allocate one rolling code
  -> encode one RTS frame
  -> transmit that frame plus repeats through CC1101
```

Live backend switching is intentionally unsupported. It adds state and
coordination questions without a real deployment benefit.

## Public API

Use domain names in external JSON.

- `POST /command`: `{ "command": "up" }` for movement commands targeting the
  currently-selected channel.
- `POST /command`: `{ "command": "up", "channel": "L2" }` selects `L2` and
  then sends the movement command, so live clients see the selected channel.
- `POST /command`: `{ "command": "prog", "channel": "L2" }` selects `L2` and
  then runs backend-native programming; `prog` requires a channel.
- `POST /command`: `{ "command": "select", "channel": "L2" }` sets the active
  channel directly. `{ "command": "select" }` with no `channel` advances the
  selection one step through `L1 → L2 → L3 → L4 → ALL → L1`.
- `select` is a public command on both backends. On Telis it drives the
  physical SELECT button until the requested LED is active. On RTS it is a
  zero-RF state update; the next movement command transmits to the new
  channel.
- `stop` is the UI spelling for the middle button. It maps to the RTS
  middle-button frame and to the Telis physical stop button.
- `GET /channel`: returns the currently-selected channel as plain text, e.g.
  `L2`. Both backends always report a current selection. The active backend is
  intentionally not exposed over HTTP so the UI stays backend-agnostic;
  `somfy doctor` reports the configured backend for diagnostics.
- `GET /events`: emits `selection` events on both backends whenever the active
  channel changes, including from `select` commands and (Telis only) physical
  remote presses. Event payloads are the plain channel name, e.g. `event.data === "L2"`.

HomeKit should use `Channel` and `Command` internally. Its external HomeKit
accessory shape does not need to change.

## Features

Compile-time features describe what the binary can do. Runtime configuration
selects the active backend from the compiled set.

```toml
[features]
default = ["fake"]
fake = []
telis = ["dep:gpiocdev"]
rts = ["dep:spidev"]
```

The current `hw` feature should be replaced with explicit backend names. If
runtime config selects a backend that was not compiled in, startup should fail
clearly:

```text
backend "rts" was selected, but this binary was built without the "rts" feature
```

## Runtime Configuration

Use a small config file plus built-in defaults. Do not expose persistent
hardware settings as repeated command flags or environment variables.

```toml
backend = "rts"

[rts]
spi_device = "/dev/spidev0.0"
gdo0_gpio = 18
pigpiod_addr = "127.0.0.1:8888"
frame_count = 4

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

If the config file is absent, defaults are enough for local development and the
documented Raspberry Pi wiring. A system install should point the service at the
config file; the unit should not carry backend or GPIO options in `ExecStart`.

Doctor should report the configured backend and run only the checks relevant to
that backend.

## Hardware Path

Use a CC1101 module as the 433.42 MHz RF frontend. Configure it over SPI with
`spidev`, then use CC1101 asynchronous serial mode so GDO0 is the OOK data
input. The Pi drives GDO0 with a complete Somfy RTS pulse train.

Default wiring assumptions:

| CC1101 | Raspberry Pi                           |
| ------ | -------------------------------------- |
| VCC    | 3.3V only                              |
| GND    | GND                                    |
| SCLK   | SPI0 SCLK / BCM11                      |
| MOSI   | SPI0 MOSI / BCM10                      |
| MISO   | SPI0 MISO / BCM9                       |
| CSN    | SPI0 CE0 / BCM8, `/dev/spidev0.0`      |
| GDO0   | BCM18, configurable as `rts.gdo0_gpio` |

Use BCM numbering everywhere in config, docs, and doctor output. The CC1101
needs an antenna tuned for 433.42 MHz.

Minimum CC1101 configuration target:

- `PKTCTRL0.PKT_FORMAT = 0b11` for asynchronous serial mode.
- ASK/OOK modulation. Do not enable CC1101 packet handling, whitening, CRC, or
  radio-side Manchester handling; the application generates the full RTS pulse
  train.
- Frequency registers set for 433.42 MHz with the module's crystal frequency.
- Initial raw data rate around 2.4 kBaud. This gives the CC1101 async input
  sampler enough resolution for 640us half-symbols; validate the final register
  values with a scope or SDR during hardware bring-up.
- Strobe to TX only while a pigpio waveform is being transmitted, then return
  the radio to idle.

Use `pigpiod` waveforms for timing. `gpiocdev` is suitable for the Telis
backend's millisecond button pulses, but RTS uses 640us Manchester half-symbols
while the process also runs HTTP, SSE/WebSocket, and HomeKit. Timing should be
owned by `pigpiod`, not by sleeps in the Rust async scheduler.

Production installs should run `pigpiod` in localhost-only mode:

```bash
pigpiod -l
```

For the RTS backend, `somfy doctor` should check:

- the selected backend was compiled into the binary;
- the configured SPI device can be opened by the service user;
- the configured GDO0 GPIO is a valid BCM GPIO number;
- `pigpiod` is reachable at the configured localhost address;
- the daemon appears to run in localhost-only mode;
- `$STATE_DIRECTORY/rts.json` is readable, owned by the service user, and
  matches a supported `schema_version`.

## pigpiod Client

Implement a small local Rust client for the `pigpiod` socket interface. Do not
link the direct `libpigpio` C interface into the service.

The local client should expose only the commands needed by the RTS transmitter:

- `MODES` / `WRITE`: prepare GDO0 as an output and set the idle level.
- `WVCLR` / `WVNEW`: clear stale waveform state.
- `WVAG`: upload a `gpioPulse_t[]` pulse train.
- `WVCRE`: create a waveform and return its wave id.
- `WVTX`: send the waveform once.
- `WVBSY`: wait for waveform completion.
- `WVDEL`: delete the created wave id after completion or failed start.
- `WVHLT`: abort an in-flight waveform during shutdown.

The socket protocol uses a fixed command struct. Encode the fields as
little-endian `u32` values, matching the Raspberry Pi/pigpio command ABI:

```text
cmd: u32
p1:  u32
p2:  u32
p3:  u32  # request extension length or response result
```

Commands with data, such as `WVAG`, append a binary extension after the command
header. For this backend the only required extension is an array of pigpio
pulses:

```text
gpioPulse_t {
  gpioOn:  u32,
  gpioOff: u32,
  usDelay: u32,
}
```

Keep all wave operations behind the RTS transmission mutex. `pigpiod` waveform
construction and transmission state is global across clients, so concurrent
wave construction can corrupt another command's waveform. Call `WVCLR` on
backend startup to clean up stale waves from prior process exits.

Per transmission, use:

```text
WVNEW -> WVAG -> WVCRE -> WVTX -> wait with WVBSY -> WVDEL
```

Always attempt `WVDEL` if `WVCRE` returned a wave id. Use `WVHLT` only for
shutdown or explicit abort of an in-flight transmission.

This keeps cross-compilation simple and preserves the one-binary deployment
model; only the `pigpiod` daemon is required on the Pi.

## RTS Protocol

Confirm the motors are Somfy RTS, not io-homecontrol. The RTS backend only
implements the 56-bit RTS frame format.

Radio settings:

- Frequency: 433.42 MHz.
- Modulation: ASK/OOK.
- Encoding: Manchester, rising edge is `1`, falling edge is `0`.
- Payload length: 56 bits, transmitted MSB first.

Minimum command set:

| Command | RTS code | Notes                                        |
| ------- | -------: | -------------------------------------------- |
| `Stop`  |    `0x1` | Somfy middle-button frame.                   |
| `Up`    |    `0x2` | Move up/open.                                |
| `Down`  |    `0x4` | Move down/close.                             |
| `Prog`  |    `0x8` | Pair or unpair a virtual remote.             |

Unobfuscated frame layout:

| Byte | Meaning                                  | Byte order                                                                                  |
| ---: | ---------------------------------------- | ------------------------------------------------------------------------------------------- |
|    0 | Key byte                                 | Use `0xA0` initially; lower nibble can be constant unless golden tests force another value. |
|    1 | Command high nibble, checksum low nibble | Command is `rts_code << 4`.                                                                 |
| 2..3 | Rolling code                             | Big-endian `u16`.                                                                           |
| 4..6 | Remote address                           | Little-endian 24-bit ID.                                                                    |

Checksum:

1. Build the unobfuscated 7-byte frame with byte 1 low nibble set to `0`.
2. XOR every byte and every byte shifted right by four.
3. Keep the low nibble.
4. OR the checksum into byte 1 low nibble.

Obfuscation:

```text
for i in 1..7:
  frame[i] = frame[i] ^ frame[i - 1]
```

The loop mutates in place, so `frame[i - 1]` is already obfuscated.

## Rolling Codes

Each channel has its own virtual remote identity:

```json
{
  "schema_version": 1,
  "selected_channel": "L1",
  "channels": {
    "L1": { "remote_id": 12345, "reserved_until": 1 },
    "L2": { "remote_id": 12346, "reserved_until": 1 },
    "L3": { "remote_id": 12347, "reserved_until": 1 },
    "L4": { "remote_id": 12348, "reserved_until": 1 },
    "ALL": { "remote_id": 12349, "reserved_until": 1 }
  }
}
```

`selected_channel` persists across restarts so the UI's notion of the active
channel survives process exits. Default to `L1` when the file is missing or
the field is absent. Persist on every change with the same atomic-rename
write used for rolling-code reserve blocks; selects are infrequent compared
to movement commands, so the extra writes are cheap.

Generate random 24-bit remote IDs per install. Avoid `0` and avoid reusing an
ID already present in the local RTS state file.

Rolling-code rules:

- Allocate one rolling code per command press.
- Repeat frames for the same press reuse the same rolling code.
- Keep the next code to put on wire in memory as `next_on_wire`.
- Increment `next_on_wire` only after a successful transmission.
- Losing or rolling back the persisted value below the motor's last-seen code
  can make the virtual remote stop working until re-paired.

Use a write-ahead buffer to reduce SD-card writes and survive crashes:

1. Keep `next_on_wire` in memory.
2. Persist `reserved_until = next_on_wire + reserve_size` before using a new
   reserve block.
3. Serve codes from memory until the reserve block is exhausted.
4. On restart, use persisted `reserved_until` as the new `next_on_wire`,
   intentionally skipping any unused reserved codes.

Use `reserve_size = 16` as the initial default.

Persist `$STATE_DIRECTORY/rts.json` with an atomic write: write a temporary file
in the same directory, flush it, rename it over the old file, and flush the
directory when the platform supports it. Create the file owned by the service
user with mode `0600`.

## Pairing

The RTS backend behaves like a new physical remote. A channel only works after
that channel's virtual remote ID has been paired with the target motor or group.

Pairing flow:

1. Put the motor or group into programming mode with an already-paired remote
   or with the motor's physical programming control.
2. Run `somfy remote prog L1` for the virtual channel that should control it.
3. Test with `somfy remote up L1`, `somfy remote down L1`, and
   `somfy remote stop L1`.
4. Repeat for `L2`, `L3`, `L4`, and `ALL` as needed.

`ALL` is a separate virtual remote identity. Pair it with every motor or group
that should react to the all-channel command. Deleting `rts.json` or changing a
channel's `remote_id` means that channel must be paired again.

If the original Telis Prog button is wired to the Pi, configure `telis.gpio.prog`.
Then the sync or unsync step can use the same backend-neutral command:

```bash
somfy remote prog L1
```

The command selects the requested Telis channel, holds the wired Prog button,
waits briefly, and transmits the RTS Prog frame for the same virtual channel.
Run it again to remove that virtual remote from the motor. If `telis.gpio.prog`
is not configured, `prog` uses backend-native behavior.

## Backend Startup

At RTS backend startup:

1. Load or initialize RTS state from `$STATE_DIRECTORY/rts.json`, including
   `selected_channel` (default `L1` if missing).
2. Open `/dev/spidev0.0`.
3. Configure CC1101 for 433.42 MHz OOK asynchronous serial transmission.
4. Connect to `pigpiod` on localhost.
5. Clear stale pigpiod waves.
6. Configure the Pi GPIO wired to CC1101 GDO0 as an output and drive it
   idle-low.

Serialize RF transmissions behind one transmitter mutex. The radio and pigpiod
waveform state are single shared hardware resources, so movement commands
— and on Telis the `select` button cycle — must take this mutex. On RTS,
`select` is a state-only operation that does not touch the radio; it acquires
the state lock for `selected_channel` but skips the transmitter mutex so an
in-flight Up/Down cannot delay a UI selection update.

## Waveform

Centralize RTS timing constants in one waveform module so hardware testing can
tune them without touching the encoder or backend state logic.

Initial default timings:

| Segment                |  Duration |
| ---------------------- | --------: |
| Wake-up high           |  `9415us` |
| Wake-up low            | `89565us` |
| Hardware sync high     |  `2560us` |
| Hardware sync low      |  `2560us` |
| Software sync high     |  `4550us` |
| Software sync low      |   `640us` |
| Manchester half-symbol |   `640us` |
| Inter-frame gap        | `30415us` |

Frame sequence:

1. First frame only: wake-up high, then wake-up low.
2. Hardware sync: two high/low cycles for the first frame, seven high/low
   cycles for repeats.
3. Software sync: high, then low.
4. 56 Manchester-encoded bits, MSB first.
5. Inter-frame gap.

For Manchester output:

- Bit `1`: low for one half-symbol, then high for one half-symbol.
- Bit `0`: high for one half-symbol, then low for one half-symbol.

For pigpio pulses, use `1 << rts.gdo0_gpio` as the GPIO mask. `gpioOn` drives
the CC1101 GDO0 data line high, `gpioOff` drives it low, and `usDelay` is the
duration before the next pulse. Idle state is low.

Default to four total frames per command press: one initial frame plus three
repeat frames.

## Transmission Flow

`RtsBackend::transmit` is the internal RF function called by both `execute`
(after resolving `selected_channel`) and `execute_on`. It does not consult or
mutate selection state.

```text
RtsBackend::transmit(channel, command)
  -> lock transmitter mutex
  -> load channel state
  -> reserve rolling-code block if needed
  -> build 7-byte RTS frame
  -> build pigpio pulse list
  -> switch CC1101 to TX
  -> WVNEW/WVAG/WVCRE through pigpiod socket
  -> WVTX once
  -> wait for WVBSY=false
  -> WVDEL created wave id
  -> switch CC1101 idle
  -> advance in-memory rolling code
  -> signal sent (channel, command) for position inference
```

If waveform upload or transmission fails, do not advance the in-memory rolling
code. Always attempt to delete a created wave id. The persisted reserve may
still skip ahead on restart, which is safe.

## Tests

Most code is testable without RF hardware.

Encoder tests:

- Command code mapping.
- Checksum generation.
- Obfuscation.
- Byte order for rolling code and remote ID.
- Golden vectors from PushStack/homebridge-rpi-rts.

Waveform tests:

- First frame contains wake-up and two hardware sync cycles.
- Repeat frames omit wake-up and contain seven hardware sync cycles.
- Manchester pulse order for known bytes.
- Total pulse count and total duration for a known frame.

State tests:

- Missing state initializes all channels.
- Schema version is required and unknown versions fail clearly.
- Remote IDs are independent per channel.
- Rolling codes are independent per channel.
- Reserve block persistence is atomic.
- Failed transmit behavior does not advance in-memory `next_on_wire`.
- Restart uses persisted `reserved_until` as the next on-wire code.
- `selected_channel` defaults to `L1` when missing and survives restart.

API/domain tests:

- `POST /command` accepts channels for movement and `prog`, selects that
  channel first, and rejects the old `led` field.
- `select` with an explicit channel sets the selection.
- `select` without a channel cycles `L1 → L2 → L3 → L4 → ALL → L1`.
- Movement commands (`up`, `down`, `stop`) target the currently selected
  channel, after applying any explicit request channel.
- `stop` maps to the backend command used for the middle button.
- `GET /channel` returns the current channel as plain text on both backends.
- `execute_on(channel, command)` remains the HomeKit direct path: it does not
  change `selected_channel` and does not emit a `selection` event.
- Inferred position fans out to all paired channels when `ALL` is targeted.

pigpiod client tests:

- Encodes fixed command headers correctly.
- Encodes `gpioPulse_t[]` extensions correctly.
- Maps negative pigpio response codes into `anyhow` errors.
- Serializes fake request/response streams for `WVNEW`, `WVAG`, `WVCRE`,
  `WVTX`, `WVBSY`, and `WVDEL`.

Hardware tests on a Pi still need to validate CC1101 wiring, RF range, pairing,
and the exact timing constants.

## Implementation Path

1. **Domain cleanup.** Rename `Input` to `Channel`, split Telis-specific GPIO
   button names from backend commands, and update HTTP/WS/HomeKit internals
   to the new `channel` payload name. `select` remains a public command with
   backend-specific cost (RF cycling on Telis, state-only on RTS); movement
   commands stop carrying a `channel` field and target the current selection.
2. **RTS encoder.** Implement the pure 7-byte encoder with golden tests.
3. **RTS state.** Add versioned `$STATE_DIRECTORY/rts.json`, random remote IDs,
   atomic writes, and rolling-code reservation.
4. **Waveform builder.** Convert encoded frames into `gpioPulse_t`-style pulse
   vectors with structural tests.
5. **pigpiod socket client.** Implement the minimal command subset and fake
   stream tests, including `WVDEL` cleanup.
6. **CC1101 driver.** Configure SPI registers for 433.42 MHz async OOK and
   expose TX/idle operations.
7. **Remote CLI and logs.** Add backend-neutral remote commands and structured
   debug logs. Remote commands use the same command vocabulary as the web API
   and HomeKit. Frame and waveform details belong in service logs rather than a
   public RTS command group.

   ```text
   somfy remote up L1
   somfy remote down L1
   somfy remote stop L1
   somfy remote prog L1
   ```

8. **Backend selection.** Wire `RtsBackend` into `RemoteControl`, add runtime
   config loading, and update install/doctor docs for CC1101, `spidev`, and
   `pigpiod`.

## References

- PushStack Somfy RTS Protocol:
  <https://pushstack.wordpress.com/somfy-rts-protocol/>
- homebridge-rpi-rts:
  <https://github.com/wibberryd/homebridge-rpi-rts/blob/master/RpiGpioRts.js>
- ESPSomfy-RTS:
  <https://github.com/rstrouse/ESPSomfy-RTS>
- pigpio:
  <https://github.com/joan2937/pigpio>
- pigs socket command reference:
  <https://manpages.ubuntu.com/manpages/jammy/man1/pigs.1.html>
- TI CC1101 datasheet:
  <https://www.ti.com/lit/gpn/CC1101>
