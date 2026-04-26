# Plan: Replace Homebridge with Native Rust HAP

Companion to [HAP.md](HAP.md) — the project-specific execution plan.

## Current state (working)

Phases 1–6 + Phase 9 are landed and verified end-to-end against an iPhone:

- ✅ Persistent state at `$STATE_DIRECTORY/hap.json` (device id, Ed25519 LTSK, setup code, paired controllers).
- ✅ mDNS advertisement of `_hap._tcp` via `mdns-sd`.
- ✅ TLV8 codec (`src/hap/tlv.rs`) with fragment reassembly.
- ✅ Custom RFC 5054 SRP-6a/SHA-512 implementation (`src/hap/srp.rs`) — the upstream `srp` crate only ships the simplified M1 form, which iOS rejects.
- ✅ Pair-Setup (M1–M6) and Pair-Verify (M1–M4) state machines.
- ✅ ChaCha20-Poly1305 session framing (`src/hap/session.rs`).
- ✅ Bridge accessory + 5 bridged `WindowCovering` accessories serving `GET /accessories`, `GET /characteristics`, `PUT /characteristics`.
- ✅ Ctrl-C handler in `serve` so the mDNS daemon unregisters cleanly.
- ✅ Single command layer (`RemoteControl::execute`) shared by REST/WS/HAP.
- ✅ Suppress UP-on-registration: PUT entries without a `value` (event subscriptions) and writes that match the cached position are no-ops.
- ✅ Position cache persisted to `positions.json` next to `hap.json`; reload is read-only.
- ✅ EVENT/1.0 push: per-connection subscription set, broadcast channel fans out CurrentPosition / TargetPosition / PositionState updates so the "Closing…" spinner resolves and All-Blinds propagates to siblings live.

Homebridge plugin still ships and runs in parallel — cutover happens at the end of Phase 8.

---

## Next steps (TODO)

The remaining work is consolidation and polish, driven by [TODO.md](TODO.md).

### Phase 7 — Lifecycle & cleanup

- Verify `somfy doctor` covers HAP (port bind, state file readable, paired-controllers count).
- Confirm Ctrl-C, SIGTERM, and `systemctl stop somfy` all unwind without leaving orphan threads. The mdns-sd daemon spawns OS threads — verify they exit on `Announcement::drop`.
- Confirm the dedicated HAP listener shuts down cleanly alongside the main HTTP server.

### Phase 8 — Retire Homebridge

- Delete `homebridge/`, the second systemd unit, the `paths-ignore` carve-out in CI.
- Remove the plugin CI deploy as well as the requirement of Node 24.
- README: replace "install Homebridge plugin" with "scan QR / enter setup code".
- Add instructions on the PR to uninstall homebridge from the system (e.g. hb-service remove homebridge-somfy-remote/apt-get uninstall homebridge)

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
