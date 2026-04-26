# Plan: Replace Homebridge with Native Rust HAP

Companion to [HAP.md](HAP.md) — the project-specific execution plan.

## Current state (working)

Phases 1–3 are landed and verified end-to-end against an iPhone:

- ✅ Persistent state at `$STATE_DIRECTORY/hap.json` (device id, Ed25519 LTSK, setup code, paired controllers).
- ✅ mDNS advertisement of `_hap._tcp` via `mdns-sd`.
- ✅ TLV8 codec (`src/hap/tlv.rs`) with fragment reassembly.
- ✅ Custom RFC 5054 SRP-6a/SHA-512 implementation (`src/hap/srp.rs`) — the upstream `srp` crate only ships the simplified M1 form, which iOS rejects.
- ✅ Pair-Setup (M1–M6) and Pair-Verify (M1–M4) state machines.
- ✅ ChaCha20-Poly1305 session framing (`src/hap/session.rs`).
- ✅ Bridge accessory + 5 bridged `WindowCovering` accessories serving `GET /accessories`, `GET /characteristics`, `PUT /characteristics`.
- ✅ Ctrl-C handler in `serve` so the mDNS daemon unregisters cleanly.

Homebridge plugin still ships and runs in parallel — cutover happens at the end of Phase 5.

---

## Next steps (TODO)

The remaining work is consolidation and polish, driven by [TODO.md](TODO.md).

### Phase 4 — Merge HAP into the main `:5002` server

Today HAP runs on a separate axum-less TCP listener at `:5010`. The split exists because post-Pair-Verify traffic is wrapped in custom AEAD framing — that can't ride axum's HTTP stack as-is. Goal: collapse to a single listener while keeping the encrypted protocol working.

**Approach:**

- Promote port `5002` to dispatch by Content-Type / first bytes:
  - HAP clients announce themselves with `Content-Type: application/pairing+tlv8` on `POST /pair-setup`. Detect at accept time and fork into the existing `handle_connection` loop.
  - Browser/CLI traffic stays on axum.
- Implementation: wrap `TcpListener::accept` ourselves. Peek the first ~32 bytes; if the request line targets a HAP route, run our connection state machine; else hand the socket to axum via `axum::serve` over a custom `Listener` impl.
- mDNS announcement updates: `port` becomes `5002`, drop the `5010` reference.
- Drop `src/hap/server.rs::serve` as a separate `tokio::spawn`; HAP serving becomes part of the axum bind.

**Exit criteria:** `lsof -iTCP -sTCP:LISTEN` on the running binary shows only `:5002`. iPhone pairing still works. Browser UI still works.

### Phase 5 — Single command layer

Eliminate `execute_blind_command` in `src/hap/server.rs:198`. Both HAP and the existing HTTP/WebSocket path should funnel through one function on `RemoteControl` (or a thin wrapper above it) that takes `(Input, Command)`. The `process_command` in `src/server.rs:90` is the closer match — extract its core into `RemoteControl` and call it from both sides.

**Constraint:** no internal HTTP hops between subsystems — direct function calls only.

### Phase 6 — Suppress UP-on-registration

Investigate whether iOS's initial `/accessories` read or its first `PUT` triggers an unwanted physical command. Suspected cause: iOS writes `TargetPosition` to whatever it last cached, and our PUT handler unconditionally maps that to up/down.

**Fix path:**

- Treat the first-ever `PUT TargetPosition` after pairing as a no-op if the value matches our cached/persisted position (Phase 7 supplies the persisted position).
- Confirm `GET /accessories` and `GET /characteristics` are pure reads — they already are, but worth a code audit while we're in here.

### Phase 7 — Persist last-known state

`positions: Mutex<HashMap<u64, u8>>` lives only in memory. On restart, the Home app sees a default that may not match physical reality. Persist the position map to a sibling file (`positions.json`) on every successful PUT, reload on boot.

**Critically:** reload is read-only. We do **not** replay the saved position to GPIO on startup.

### Phase 8 — Lifecycle & cleanup

- Verify `somfy doctor` covers HAP (port bind, state file readable, paired-controllers count).
- Confirm Ctrl-C, SIGTERM, and `systemctl stop somfy` all unwind without leaving orphan threads. The mdns-sd daemon spawns OS threads — verify they exit on `Announcement::drop`.
- Remove anything in cleanup that only existed because HAP ran on a second port.

### Phase 9 — Retire Homebridge

- Delete `homebridge/`, the second systemd unit, the `paths-ignore` carve-out in CI.
- README: replace "install Homebridge plugin" with "scan QR / enter setup code".
- `somfy uninstall` stops both units if present (back-compat with deployed Pis).

### Phase 10 — Event notifications (deferred)

Push EVENT/1.0 frames on subscribed characteristics when `RemoteControl.receiver` fires, so physical-remote changes propagate to Home. Useful but not required for the cutover.

---

## Verification checklist (carried over from TODO.md)

- [ ] Only one server running on `:5002`
- [ ] No duplicated command paths or logic
- [ ] REST / WebSocket / HAP all use the same command layer
- [ ] No commands triggered during accessory registration
- [ ] HAP reflects correct state after server restart
- [ ] Server exits cleanly without errors

---

## Risks & mitigations

- **Axum + raw socket dispatch** is the trickiest part of Phase 4. Fallback: keep `:5010` and just bind it to `127.0.0.1`-aware mDNS advertisement, accepting the two-port reality.
- **Position persistence races** with concurrent PUTs — single `Mutex` write before the file write should be enough; no need for a lock file.
- **State file** must survive `somfy upgrade` — already covered by `StateDirectory=somfy` (systemd preserves it across binary swaps).

## Dependencies (final list)

`mdns-sd`, `ed25519-dalek`, `x25519-dalek`, `chacha20poly1305`, `hkdf`, `rand`, `httparse`, `num-bigint` — all pure Rust, all cross-compile clean for armv7. The `srp` crate was dropped in favor of an in-tree RFC 5054 implementation.
