# Native HomeKit (HAP) Server

`somfy serve` runs a native HomeKit Accessory Protocol server alongside the HTTP, SSE, and WebSocket API. The Home app talks straight to the Rust binary — no Homebridge, no Node.

## Pairing

Show the setup code and pairing QR:

```bash
somfy homekit status
```

In iOS Home → Add Accessory → scan the QR code. The Bridge appears as **Somfy XXXXXX** with four `WindowCovering` tiles inside.

Pairing lifecycle commands are exposed by the CLI:

```bash
somfy homekit --help
```

## Doctor

`somfy doctor` deliberately stays focused on process and deployment health: systemd unit drift, service state, GPIO access, updates, and deployed version. HomeKit pairing state lives under `somfy homekit ...` so diagnostics and pairing lifecycle commands do not drift apart.

## Wire layout

- **Port `5010`** — dedicated TCP listener. Kept separate from the loopback HTTP listener (`127.0.0.1:5002`) because post-`Pair-Verify` traffic upgrades the socket into HAP's custom AEAD framing, which doesn't fit axum's request/response model.
- **mDNS** — `_hap._tcp.local.` advertised via `mdns-sd`. TXT record carries `id`, `c#`, `s#`, `sf`, `ci=2` (Bridge), `md`, `pv=1.1`. The `Announcement` guard's `Drop` impl unregisters and shuts the daemon's worker threads.
- **Accessory database** — Bridge (`aid=1`) plus 5 bridged `WindowCovering` accessories (`aid=2..6`), one per Somfy LED selector (`L1`–`L4`, `ALL`). IIDs are stable across runs; `config_number` must bump if the schema ever changes.

## Persistent state

`$STATE_DIRECTORY` (set by systemd via `StateDirectory=somfy`; otherwise defaults to `/var/lib/somfy` in release builds and `./hap-state` in debug builds; override with `SOMFY_STATE_DIR`):

| File             | Owner                  | Contents                                                                                 |
| ---------------- | ---------------------- | ---------------------------------------------------------------------------------------- |
| `hap.json`       | `state.rs`             | device id, setup code, Ed25519 long-term signing key, `c#`/`s#`, paired controllers      |
| `positions.json` | `positioning/state.rs` | aid → last estimated position (0-100). Reload is **read-only** — never replayed to GPIO. |

Both files are written atomically (tmp + `rename`) with mode `0600`. systemd preserves them across `somfy upgrade`.

## Crypto + protocol

| Concern             | Implementation                                                                                                                                         |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| TLV8 codec          | `src/hap/tlv.rs` with fragment reassembly (HAP §14.1).                                                                                                 |
| SRP-6a / SHA-512    | In-tree `src/hap/srp.rs` over the 3072-bit group (RFC 5054). The upstream `srp` crate ships only the simplified M1 form, which iOS rejects.            |
| Pair-Setup (M1–M6)  | `src/hap/pair_setup.rs` — username `Pair-Setup`, AccessoryX/iOSX derived per spec, signed Ed25519 proofs.                                              |
| Pair-Verify (M1–M4) | `src/hap/pair_verify.rs` — X25519 ECDH, Ed25519 mutual auth, HKDF-SHA512 → session keys.                                                               |
| Session framing     | `src/hap/session.rs` — ChaCha20-Poly1305 with 2-byte length AAD, per-direction nonces, max plaintext 1024.                                             |
| HTTP                | Hand-rolled on `tokio` + `httparse` (no axum). Both plain and encrypted readers feed the same parser; `http::StatusCode` owns response status phrases. |
| App wiring          | `src/homekit/mod.rs` wires mDNS advertisement, HAP state, controller position events, and the generic HAP server.                                      |

## Connection lifecycle

`src/hap/server/mod.rs::handle_connection` runs a single `tokio::select!`:

1. **Plain phase** — `POST /pair-setup`, `POST /pair-verify`. After M4 verifies, the reader/writer are upgraded to encrypted halves and the connection switches to the control channel.
2. **Control phase** — `GET /accessories`, `GET /characteristics`, `PUT /characteristics`, `POST /pairings`. All require an encrypted writer; otherwise we return `401`.
3. **Event push** — every connection holds a per-socket `HashSet<(aid, iid)>` of subscribed characteristics and a `broadcast::Receiver<Vec<CharacteristicEvent>>`. When the controller publishes position deltas, a sink installed at `homekit::start` maps them to characteristic events on that broadcast channel; HAP connections fan matching subscriptions out as `EVENT/1.0` frames over the same encrypted writer.

EVENT push is what resolves the iOS "Closing…" / "Opening…" spinner (waits on `PositionState=2` + `CurrentPosition` matching `TargetPosition`) and what keeps grouped Home writes reflected on each individual blind tile.

## PUT semantics

`handle_put_characteristics` distinguishes three shapes per entry:

- `{aid, iid, ev: true|false}` — toggle subscription on the per-connection set. No GPIO action.
- `{aid, iid, value: N}` where `N` matches the estimated current position — no-op unless it cancels a pending timed move, in which case the controller sends `stop`.
- `{aid, iid, value: N}` with a real change — asks the shared controller to move from the estimated current position to `N`. The controller sends `up` or `down`, emits `TargetPosition` plus moving `PositionState`, and for interior targets (`1..99`) sends `stop` after the configured proportional travel time. Endpoint targets (`0` or `100`) rely on the motor's own limits. Completion updates `CurrentPosition`, persists `positions.json`, and emits stopped events. HAP EVENT frames for those updates are published only from the controller position sink (not duplicated on the PUT write outcome).

## Timed positioning

Somfy RTS/Telis motors do not report physical position, so percentages are estimated from configured travel time. The defaults are 10 seconds open and close for every blind. Override per blind:

```toml
[positioning.l1]
open_ms = 8500
close_ms = 9200

[positioning.l2]
open_ms = 7000
close_ms = 8000
```

The controller supports different timings per blind. When Home writes all four blinds to the same direction in one batch, it can start them with one `ALL` command, then issue individual `stop` commands for interior targets at each blind's calculated completion time.

## Lifecycle

`serve` selects between the HTTP server and a shutdown signal handler that catches both SIGINT (Ctrl-C) and SIGTERM (`systemctl stop somfy`). On either path the `Announcement` guard drops, which calls `daemon.unregister(...)` then `daemon.shutdown()`, terminating the mdns-sd worker threads. The HAP listener task is detached on `tokio::spawn` and dies with the runtime.

## Dependencies

`mdns-sd`, `ed25519-dalek`, `x25519-dalek`, `chacha20poly1305`, `hkdf`, `rand`, `httparse`, `http`, `num-bigint`. All pure Rust, all cross-compile cleanly for `armv7-unknown-linux-gnueabihf`.

`status` creates `hap.json` if needed, prints the setup URI/code, and renders the QR while the bridge is unpaired. Resetting or removing pairings requires `sudo somfy restart` so the in-memory HAP server advertises and enforces the updated state.

## Accessory Identity Stability

Home remembers accessories by the persisted identity in `hap.json` and by stable accessory/characteristic IDs from `homekit/somfy.rs`. Treat these values as compatibility surfaces:

- Keep `device_id`, `setup_id`, and the long-term signing key stable across upgrades. Regenerating them is a factory reset and forces re-pairing.
- Keep AIDs and IIDs stable once shipped. Changing them can make Home lose room/name/automation associations.
- Bump `config_number` only when the exposed accessory schema changes.
- Avoid renaming the bridge model/name constants casually. User-visible names can be changed in Home, but changing advertised defaults can make debugging paired devices harder.
