# TODO

## RTS Backend

Track implementation progress for [docs/RTS_BACKEND.md](docs/RTS_BACKEND.md).

### Phase 1: Domain Cleanup

- [x] Rename `Input` to `Channel` across Rust, HomeKit, server, and frontend-facing payloads.
- [x] Split Telis GPIO button/output names from domain backend commands.
- [x] Update HTTP and WebSocket command payloads from `led` to `channel`.
- [x] Add `GET /channel` returning the current channel as plain text (backend-agnostic; backend identity lives in `somfy doctor`).
- [x] Change SSE `selection` events to plain text channel-name payloads (e.g. `L2`).
- [x] Keep `select` as a public command and reject stale `led` payloads.
- [x] Ensure directional commands target the current selected channel instead of carrying a channel.
- [x] Update HomeKit internals to use `Channel`.
- [x] Add or update tests for command parsing, selection behavior, API payloads, and position propagation.

### Phase 2: Backend Abstraction

- [x] Introduce backend abstraction behind `RemoteControl`.
- [x] Move current physical Telis behavior into a `TelisBackend`.
- [x] Add or preserve a `FakeBackend` for local/default builds.
- [x] Add `CommandOutcome` plumbing for inferred position updates.
- [x] Implement stateful `execute(command)` for HTTP UI commands.
- [x] Implement stateless `execute_on(channel, command)` for HomeKit and CLI commands.
- [x] Ensure `execute_on` does not mutate or broadcast selected-channel state.

### Phase 3: RTS Pure Logic

- [ ] Refine backend internals into controller + transport layers: backend controllers own command/selection semantics; transports perform concrete IO such as Telis GPIO presses, RTS waveform transmission, or logging/fake recording.
- [ ] Add transport-level fakes/logging so tests can assert generated protocol operations without reimplementing backend behavior.
- [ ] Add RTS command code mapping.
- [ ] Implement 7-byte RTS frame encoding.
- [ ] Implement checksum generation.
- [ ] Implement frame obfuscation.
- [ ] Add golden/unit tests for command codes, byte order, checksum, and obfuscation.
- [ ] Add versioned `$STATE_DIRECTORY/rts.json` state.
- [ ] Generate independent random 24-bit remote IDs per channel.
- [ ] Persist and restore `selected_channel`.
- [ ] Implement rolling-code reservation with atomic writes.
- [ ] Add state tests for missing files, schema versions, independent codes, restart behavior, and failed transmit behavior.

### Phase 4: Waveform and Hardware Clients

- [ ] Add RTS waveform builder that emits `gpioPulse_t`-style pulse vectors.
- [ ] Add waveform tests for wake-up, sync cycles, Manchester ordering, pulse counts, and duration.
- [ ] Implement minimal `pigpiod` socket client.
- [ ] Support `MODES`, `WRITE`, `WVCLR`, `WVNEW`, `WVAG`, `WVCRE`, `WVTX`, `WVBSY`, `WVDEL`, and `WVHLT`.
- [ ] Add fake stream tests for pigpio command encoding and error mapping.
- [ ] Add CC1101 SPI driver behind the `rts` feature.
- [ ] Configure CC1101 for 433.42 MHz ASK/OOK asynchronous serial transmission.
- [ ] Expose CC1101 TX and idle operations.

### Phase 5: Runtime Wiring

- [ ] Replace Cargo feature `hw` with explicit `fake`, `telis`, and `rts` features.
- [ ] Add `somfy serve --backend` with `SOMFY_BACKEND` fallback.
- [ ] Add RTS runtime options for SPI device, GDO0 GPIO, pigpiod address, and frame count.
- [ ] Fail startup clearly when a selected backend was not compiled into the binary.
- [ ] Wire `RtsBackend` into `RemoteControl`.
- [ ] Add `somfy rts dump CHANNEL COMMAND --format json`.
- [ ] Add `somfy rts send CHANNEL up|down|my`.
- [ ] Add `somfy rts prog CHANNEL`.
- [ ] Route RTS CLI commands through `execute_on`.
- [ ] Update `somfy install --backend rts` to write the selected backend into the unit.
- [ ] Update `somfy doctor` with backend-specific checks.

### Phase 6: Validation and Docs

- [ ] Run `mise run check` after each major phase.
- [ ] Validate SPI access on a Raspberry Pi.
- [ ] Validate `pigpiod` connectivity and localhost-only mode.
- [ ] Validate CC1101 register configuration with scope or SDR.
- [ ] Validate RTS waveform timing constants with hardware.
- [ ] Validate pairing and command behavior for `L1`, `L2`, `L3`, `L4`, and `ALL`.
- [ ] Update `README.md` with RTS setup and pairing flow.
- [ ] Update `docs/HARDWARE.md` with CC1101 wiring.
- [ ] Update `docs/ARCHITECTURE.md` once the backend abstraction is stable.
