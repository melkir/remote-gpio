# Plan: Replace Homebridge with Native Rust HAP

Companion to [HAP.md](HAP.md) — the project-specific execution plan.

## Current state

- `homebridge/` is a thin Node plugin: 5 `WindowCovering` accessories (L1–L4 + ALL) → `POST /command` shim. Runs as a separate `homebridge` systemd unit on the Pi.
- Backend already exposes the only logic we need: `process_command(rc, "up|down|stop|select", led)` in [src/server.rs](../src/server.rs).

## Strategy

Add an in-process HAP server as a new `somfy hap` subcommand (or boot it from `serve` behind a flag), reusing `Arc<AppState>` directly — no HTTP hop. Keep Homebridge working until pairing is stable; cut over by stopping the `homebridge` unit.

## Phases

### Phase 1 — Skeleton & discovery

- New module `src/hap/` (`mod.rs`, `accessory.rs`, `mdns.rs`, `http.rs`, `pairing.rs`, `session.rs`).
- mDNS via `mdns-sd` crate (pure Rust, no Avahi dep — keeps cross-build simple). Advertise `_hap._tcp` with `id`, `md=Somfy Telis 4`, `ci=14` (Window Covering category), `sf=1`, `c#`, `s#`.
- Persist accessory state (device id, long-term Ed25519 keypair, paired controllers) to `/var/lib/somfy/hap.json`.
- Exit criteria: device shows up as "uncertified" in iOS Home; pairing fails gracefully.

### Phase 2 — Unencrypted accessory model

- Axum sub-router on a separate port (e.g. 5010) producing HAP TLV/JSON.
- 5 accessories mirroring the Homebridge layout. Characteristics: `CurrentPosition`, `TargetPosition`, `PositionState`, plus required `AccessoryInformation` service.
- `GET /accessories`, `GET /characteristics`, `PUT /characteristics` wired to the existing `RemoteControl`.
- Exit criteria: `curl` against `/accessories` matches HAP-NodeJS shape byte-for-byte for required fields.

### Phase 3 — Pairing (the hard part)

- Pair-Setup (M1–M6): SRP-6a (3072-bit, group 3072), HKDF-SHA512, Ed25519 LTSK exchange. Crates: `srp` (or roll on `num-bigint`), `ed25519-dalek`, `hkdf`, `sha2` (already a dep), `chacha20poly1305`.
- Pair-Verify (M1–M4): X25519 ECDH (`x25519-dalek`) → session keys.
- Encrypted session framing on every byte after Pair-Verify (frame = 2-byte AAD len + ciphertext + 16-byte tag, per-message nonces).
- TLV8 codec — write a small `tlv.rs`, no crate needed.
- Exit criteria: iPhone completes pairing with the 8-digit setup code printed at startup; persists across server restarts.

### Phase 4 — Events & cutover

- Event notifications: when `RemoteControl.receiver` fires, push EVENT/1.0 frames to subscribed sessions so the Home app reflects state changes from the physical remote.
- Apply the same ALL↔individual sync logic the Homebridge plugin has ([homebridge/src/index.ts](../homebridge/src/index.ts)).
- Update `somfy install` to register the HAP port in firewall/avahi if needed; drop the `homebridge` unit from the install path.
- `somfy doctor` gains an `hap` check (port bind, state file readable, pair count).

### Phase 5 — Retire Homebridge

- Delete `homebridge/`, the second systemd unit, and the `paths-ignore` carve-out in CI.
- README: replace "install Homebridge plugin" section with "scan QR / enter setup code".

## Risks & mitigations

- **Pairing crypto correctness** is the whole project. Test against `hap-controller` (Python) on a laptop before pointing a real iPhone at it — much faster iteration.
- **TLV8 + chunking edge cases** are where everyone gets stuck; lift the wire-level test vectors from HAP-NodeJS.
- **mdns-sd** behavior on a Pi with both IPv4/IPv6 needs verification; fall back to `zeroconf` (Avahi binding) if iOS won't resolve.
- **State file** must survive upgrades — add to the `somfy upgrade` swap path.

## Dependencies to add

`mdns-sd`, `srp`, `ed25519-dalek`, `x25519-dalek`, `chacha20poly1305`, `hkdf`, `rand` — all pure Rust, all cross-compile clean for armv7.
