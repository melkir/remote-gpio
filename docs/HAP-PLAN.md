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

Homebridge plugin still ships and runs in parallel — cutover happens at the end of Phase 8.

---

## Next steps (TODO)

The remaining work is consolidation and polish, driven by [TODO.md](TODO.md).

### Phase 4 — Single command layer

Eliminate `execute_blind_command` in `src/hap/server.rs:198`. Both HAP and the existing HTTP/WebSocket path should funnel through one function on `RemoteControl` (or a thin wrapper above it) that takes `(Input, Command)`. The `process_command` in `src/server.rs:90` is the closer match — extract its core into `RemoteControl` and call it from both sides.

**Constraint:** no internal HTTP hops between subsystems — direct function calls only.

**Non-goal:** merging the HAP listener into `:5002`. HAP keeps its dedicated port because post-Pair-Verify traffic upgrades the connection into custom encrypted framing that does not fit axum's request/response model.

### Phase 5 — Suppress UP-on-registration

Investigate whether iOS's initial `/accessories` read or its first `PUT` triggers an unwanted physical command. Suspected cause: iOS writes `TargetPosition` to whatever it last cached, and our PUT handler unconditionally maps that to up/down.

**Fix path:**

- Treat the first-ever `PUT TargetPosition` after pairing as a no-op if the value matches our cached/persisted position (Phase 6 supplies the persisted position).
- Confirm `GET /accessories` and `GET /characteristics` are pure reads — they already are, but worth a code audit while we're in here.

### Phase 6 — Persist last-known state

`positions: Mutex<HashMap<u64, u8>>` lives only in memory. On restart, the Home app sees a default that may not match physical reality. Persist the position map to a sibling file (`positions.json`) on every successful PUT, reload on boot.

**Critically:** reload is read-only. We do **not** replay the saved position to GPIO on startup.

### Phase 7 — Lifecycle & cleanup

- Verify `somfy doctor` covers HAP (port bind, state file readable, paired-controllers count).
- Confirm Ctrl-C, SIGTERM, and `systemctl stop somfy` all unwind without leaving orphan threads. The mdns-sd daemon spawns OS threads — verify they exit on `Announcement::drop`.
- Confirm the dedicated HAP listener shuts down cleanly alongside the main HTTP server.

### Phase 8 — Retire Homebridge

- Delete `homebridge/`, the second systemd unit, the `paths-ignore` carve-out in CI.
- Remove the plugin CI deploy as well as the requirement of Node 24.
- README: replace "install Homebridge plugin" with "scan QR / enter setup code".
- Add instructions on the PR to uninstall homebridge from the system (e.g. hb-service remove homebridge-somfy-remote/apt-get uninstall homebridge)

### Phase 9 — Event notifications (now blocking the cutover)

Push EVENT/1.0 frames on subscribed characteristics. **No longer optional**:

- Closing/opening a blind in Home shows "Closing…" / "Opening…" indefinitely. iOS waits for an EVENT on `PositionState` (→ stopped) and `CurrentPosition` (→ matches target) before resolving the spinner. We never send them.
- The "All Blinds" tile updates `positions` for the four siblings via `propagate_positions`, but iOS doesn't observe those changes without EVENTs, so the per-blind tiles stay stale until the user backs out and re-enters the Home view.
- Physical-remote changes propagating to Home — original Phase 9 motivation — is the same mechanism.

Move this ahead of Phase 8 (Homebridge retirement). The plugin handled both of the above via `updateValue` in `homebridge/src/index.ts:147` — without events we'd ship a regression.

---

## Open questions (parked, not in any phase yet)

- **Dev mode dual-client.** `mise dev` runs both `server-dev` (binary serves `app/dist/` from disk via the debug branch in `src/embed.rs:36`) and `app-dev` (Vite hot-reload on its own port). Two URLs serve the same UI, only the Vite one hot-reloads. Options: gate static-asset serving off in debug builds (force `cargo run` users to hit Vite); drop the disk-read path in `embed.rs`; or document the convention. Decide before touching either.
- **`mise dev` Ctrl-C noise.** `[app-dev] ERROR sh exited with non-zero status: no exit status` after `task failed.` — Vite returns non-zero on SIGINT and mise surfaces it. Cosmetic; either swallow with a wrapper or leave a note in the README.

---

## Verification checklist (carried over from TODO.md)

- [ ] No duplicated command paths or logic
- [ ] REST / WebSocket / HAP all use the same command layer
- [ ] No commands triggered during accessory registration
- [ ] HAP reflects correct state after server restart
- [ ] Server exits cleanly without errors

---

## Risks & mitigations

- **Shared command abstraction** must stay transport-agnostic. Keep HAP protocol concerns in `src/hap/*` and expose only a small in-process API for command execution/state updates.
- **Position persistence races** with concurrent PUTs — single `Mutex` write before the file write should be enough; no need for a lock file.
- **State file** must survive `somfy upgrade` — already covered by `StateDirectory=somfy` (systemd preserves it across binary swaps).

## Dependencies (final list)

`mdns-sd`, `ed25519-dalek`, `x25519-dalek`, `chacha20poly1305`, `hkdf`, `rand`, `httparse`, `num-bigint` — all pure Rust, all cross-compile clean for armv7. The `srp` crate was dropped in favor of an in-tree RFC 5054 implementation.
