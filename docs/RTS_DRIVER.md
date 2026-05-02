# RTS Protocol Reference

A self-contained reference for the Somfy RTS frame format, waveform timings, and the pigpiod commands the driver uses.

For wiring, bring-up, and pairing flow, see [HARDWARE.md](HARDWARE.md#cc1101-rts-driver). For the driver's place in the codebase, see [ARCHITECTURE.md](ARCHITECTURE.md#drivers).

## Radio

- Frequency: **433.42 MHz** (note: not 433.92).
- Modulation: ASK / OOK.
- Encoding: Manchester. Rising edge = `1`, falling edge = `0`.
- Payload: 56 bits, MSB first.
- Per press: 4 total frames (1 initial + 3 repeats). The constant is fixed in `src/rts/waveform.rs`.

## Frame Layout (7 bytes, unobfuscated)

| Byte | Meaning                                       | Notes                                                                          |
| ---: | --------------------------------------------- | ------------------------------------------------------------------------------ |
|    0 | Key byte                                      | `0xA0` works for the motors tested. Lower nibble can vary by motor generation. |
|    1 | Command (high nibble) + checksum (low nibble) | Command is `rts_code << 4`.                                                    |
| 2..3 | Rolling code                                  | Big-endian `u16`.                                                              |
| 4..6 | Remote address                                | Little-endian 24-bit ID.                                                       |

### Command codes

| Command | RTS code | Notes                            |
| ------- | -------: | -------------------------------- |
| `Stop`  |    `0x1` | Somfy middle-button frame.       |
| `Up`    |    `0x2` | Move up / open.                  |
| `Down`  |    `0x4` | Move down / close.               |
| `Prog`  |    `0x8` | Pair or unpair a virtual remote. |

### Checksum

1. Build the unobfuscated 7-byte frame with byte 1 low nibble set to `0`.
2. XOR every byte and every byte shifted right by 4.
3. Keep the low nibble.
4. OR the result into byte 1 low nibble.

### Obfuscation

```text
for i in 1..7:
    frame[i] = frame[i] ^ frame[i - 1]
```

The loop mutates in place, so `frame[i - 1]` is already obfuscated.

## Waveform Timings

Defined in `src/rts/waveform.rs`. Patent-derived (US7860481 B2). Not user-configurable.

| Segment                |   Duration |
| ---------------------- | ---------: |
| Wake-up high           |  `9415 µs` |
| Wake-up low            | `89565 µs` |
| Hardware sync high     |  `2560 µs` |
| Hardware sync low      |  `2560 µs` |
| Software sync high     |  `4550 µs` |
| Software sync low      |   `640 µs` |
| Manchester half-symbol |   `640 µs` |
| Inter-frame gap        | `30415 µs` |

### Frame sequence

1. **First frame only:** wake-up high, then wake-up low.
2. **Hardware sync:** 2 high/low cycles for the first frame, **7** for repeats.
3. **Software sync:** high, then low.
4. **Payload:** 56 Manchester-encoded bits, MSB first.
5. **Inter-frame gap.**

Manchester output:

- Bit `1`: low for one half-symbol, then high for one half-symbol.
- Bit `0`: high for one half-symbol, then low for one half-symbol.

Idle state is low. For pigpio pulses the GPIO mask is `1 << rts.gdo0_gpio`.

## Rolling Codes & State

Each channel (`L1`–`L4`, `ALL`) is an independent virtual remote with its own 24-bit ID and its own rolling-code counter. State is persisted to `$STATE_DIRECTORY/rts.json`:

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

### Write-ahead reserve

To minimize SD-card writes and survive crashes:

1. Keep `next_on_wire` in memory.
2. Persist `reserved_until = next_on_wire + reserve_size` before serving the next reserve block.
3. Serve codes from memory until the reserve is exhausted.
4. On restart, treat persisted `reserved_until` as the new `next_on_wire`, intentionally skipping any unused reserved codes.

`DEFAULT_RESERVE_SIZE = 16`. A crash mid-transmit may burn up to that many codes per channel — that's intentional and within the receiver window.

The file is rewritten via tmp + atomic rename + fsync, mode `0600`, owned by the service user.

### Rules

- One rolling code per command press.
- Repeat frames within the same press reuse the same code.
- Increment `next_on_wire` only after a successful transmission.
- Rolling back the persisted code below the motor's last-seen value can desync the virtual remote until re-pairing.

## Pigpiod Client

`pigpiod` owns the GDO0 timing because RTS uses 640 µs half-symbols while the same process serves HTTP/SSE/WS/HomeKit. Doing this from Rust async sleeps is not reliable enough.

The driver speaks the pigpiod socket protocol directly (no `libpigpio` linkage). Localhost-only — both `RtsDriver::new` and the doctor probe reject non-loopback `pigpiod_addr` (pigpiod is unauthenticated).

### Commands used

| Cmd     | Purpose                                 |
| ------- | --------------------------------------- |
| `MODES` | Set GDO0 to output                      |
| `WRITE` | Drive GDO0 to idle low                  |
| `WVCLR` | Clear stale waves on startup            |
| `WVNEW` | Begin a new waveform                    |
| `WVAG`  | Append a `gpioPulse_t[]` extension      |
| `WVCRE` | Build the wave, returns wave id         |
| `WVTX`  | Transmit the wave once                  |
| `WVBSY` | Poll until transmission completes       |
| `WVDEL` | Free the wave id                        |
| `WVHLT` | Abort an in-flight wave (shutdown only) |

### Wire format

Fixed-size little-endian command header:

```text
cmd: u32
p1:  u32
p2:  u32
p3:  u32   # request extension length, or response result
```

Commands with binary data (e.g. `WVAG`) append an extension. The only extension this driver uses is an array of `gpioPulse_t`:

```text
gpioPulse_t {
    gpioOn:  u32,   # mask of pins to drive high
    gpioOff: u32,   # mask of pins to drive low
    usDelay: u32,   # microseconds before the next pulse
}
```

Per transmission:

```text
WVNEW → WVAG → WVCRE → WVTX → poll WVBSY → WVDEL
```

Always attempt `WVDEL` if `WVCRE` returned a wave id, even after a failed `WVTX`. All wave operations sit behind the driver's transmission mutex — pigpiod waveform construction state is global across clients.

## Transmission Flow

```text
RtsDriver::transmit(channel, command)
  → lock transmitter mutex
  → load channel state, reserve rolling-code block if needed
  → encode 7-byte RTS frame (key, cmd|checksum, rolling code BE, remote ID LE)
  → obfuscate (XOR cascade)
  → build pigpio pulse list
  → CC1101 SRES + STX
  → WVNEW / WVAG / WVCRE / WVTX, poll WVBSY, WVDEL
  → CC1101 SIDLE
  → on success, advance in-memory rolling code
  → signal sent (channel, command) for position inference
```

If waveform upload or transmission fails, the in-memory rolling code is **not** advanced. The persisted reserve may still skip ahead on restart, which is safe.

## CC1101 Configuration

Bring-up notes (see also [HARDWARE.md](HARDWARE.md#bring-up-checklist)):

- `PKTCTRL0.PKT_FORMAT = 0b11` — asynchronous serial mode (data on GDO0).
- ASK / OOK modulation.
- Frequency registers: 433.42 MHz, computed against the module's crystal.
- Initial raw data rate ~2.4 kBaud — gives the async sampler enough resolution for 640 µs half-symbols.
- **Disable** packet handling, whitening, CRC, and radio-side Manchester. The application generates the full pulse train.
- Strobe to TX only while a wave is transmitting; return to idle (`SIDLE`) afterward.

The register set in `src/rts/cc1101.rs` is a starting point and has not been validated against a scope — see the bring-up checklist before relying on it for new motor generations.

## External References

- PushStack Somfy RTS Protocol writeup: <https://pushstack.wordpress.com/somfy-rts-protocol/>
- homebridge-rpi-rts: <https://github.com/wibberryd/homebridge-rpi-rts/blob/master/RpiGpioRts.js>
- ESPSomfy-RTS: <https://github.com/rstrouse/ESPSomfy-RTS>
- pigpio: <https://github.com/joan2937/pigpio>
- pigs socket command reference: <https://manpages.ubuntu.com/manpages/jammy/man1/pigs.1.html>
- TI CC1101 datasheet: <https://www.ti.com/lit/gpn/CC1101>
- Somfy RTS waveform patent (US7860481 B2): <https://patents.google.com/patent/US7860481B2/>
