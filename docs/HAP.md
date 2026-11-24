# Native HomeKit (HAP) Server

`somfy serve` can run a native HomeKit Accessory Protocol server alongside the HTTP, SSE, and WebSocket API. The Home app talks straight to the Rust binary — no Homebridge, no Node.

## Enabling HomeKit

HomeKit is **off by default**. Turn it on in `/etc/somfy/config.toml`, then restart the service:

```toml
homekit = true
```

```bash
sudo somfy restart
```

While it is disabled, `somfy homekit status` just reports `HomeKit is disabled in <config path>`. If the HAP server fails to start, `serve` logs a warning and keeps running without HomeKit.

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

`status` creates `hap.json` if needed, prints the setup URI/code, and renders the QR while the bridge is unpaired. After `reset` or `unpair`, run `sudo somfy restart` so the in-memory HAP server advertises and enforces the updated state.

`somfy doctor` does not report pairing state. It stays focused on process and deployment health; pairing lives under `somfy homekit ...` so the two don't drift apart.

## Wire layout

- **Port `5010`** — dedicated TCP listener. Kept separate from the loopback HTTP listener (`127.0.0.1:5002`, served by axum) because post-`Pair-Verify` traffic upgrades the socket into HAP's custom AEAD framing, which doesn't fit axum's request/response model.
- **mDNS** — `_hap._tcp.local.` advertised via `mdns-sd`. TXT record carries `id`, `c#`, `s#`, `sf`, `ff=0`, `ci=2` (Bridge), `md`, `pv=1.1`, and the setup hash `sh`. The `Announcement` guard's `Drop` impl unregisters and shuts the daemon's worker threads.
- **Accessory database** — Bridge (`aid=1`) plus 4 bridged `WindowCovering` accessories (`aid=2..5`), one per Somfy LED selector (`L1`–`L4`). `ALL` is not an accessory: it is a fan-out that addresses all four (see `blinds_for_channel`). IIDs are stable across runs; `config_number` must bump if the schema ever changes.

## Persistent state

HomeKit state lives in `hap.json` (device ID, setup code, Ed25519 long-term signing key, `c#`/`s#`, paired controllers); blind positions live in `positions.json` and are never replayed to hardware on reload. Both sit in the service state directory, written atomically with mode `0600`, and survive `somfy upgrade`. Location and the full file list: [ARCHITECTURE.md](ARCHITECTURE.md#persistence).

## Crypto + protocol

| Concern             | Implementation                                                                                                                                         |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| TLV8 codec          | `src/hap/tlv.rs` with fragment reassembly (HAP §14.1).                                                                                                 |
| SRP-6a / SHA-512    | In-tree `src/hap/srp.rs` over the 3072-bit group (RFC 5054). The upstream `srp` crate ships only the simplified M1 form, which iOS rejects.            |
| Pair-Setup (M1–M6)  | `src/hap/pair_setup.rs` — username `Pair-Setup`, AccessoryX/iOSX derived per spec, signed Ed25519 proofs.                                              |
| Pair-Verify (M1–M4) | `src/hap/pair_verify.rs` — X25519 ECDH, Ed25519 mutual auth, HKDF-SHA512 → session keys.                                                               |
| Session framing     | `src/hap/session.rs` — ChaCha20-Poly1305 with 2-byte length AAD, per-direction nonces, max plaintext 1024. Each frame is written in one call (see below). |
| HTTP                | Hand-rolled on `tokio` + `httparse` (no axum). Both plain and encrypted readers feed the same parser; `http::StatusCode` owns response status phrases. |
| App wiring          | `src/homekit/mod.rs` wires mDNS advertisement, HAP state, controller position events, and the generic HAP server.                                      |

## Connection lifecycle

`src/hap/server/mod.rs::handle_connection` runs a single `tokio::select!` that races request reads against event pushes. A connection goes through three phases:

1. **Plain phase** — `POST /pair-setup`, `POST /pair-verify`. After M4 verifies, the reader/writer are upgraded to encrypted halves and the connection switches to the control channel.
2. **Control phase** — `GET /accessories`, `GET /characteristics`, `PUT /characteristics`, `POST /pairings`. All require an encrypted writer; otherwise we return `401`.
3. **Event push** — every connection holds a per-socket `HashSet<(aid, iid)>` of subscribed characteristics and a `broadcast::Receiver<Vec<CharacteristicEvent>>`. The controller publishes position deltas only on `BlindController::subscribe_positions` (never on the HAP stack). A bridge task started at `homekit::start` subscribes there, maps deltas to characteristic events, and forwards them on the HAP runtime broadcast channel; connections fan matching subscriptions out as `EVENT/1.0` frames over the same encrypted writer. If that subscription lags, the bridge warns and pushes a full snapshot resync so iOS does not miss `CurrentPosition` / `TargetPosition` / `PositionState` updates.

EVENT push is what resolves the iOS "Closing…" / "Opening…" spinner (waits on `PositionState=2` + `CurrentPosition` matching `TargetPosition`) and what keeps grouped Home writes reflected on each individual blind tile.

### Socket invariants

- **Reads must be cancellation-safe.** The request-reading branch is dropped whenever an EVENT push wins the `select!`, possibly mid-frame. `EncryptedReader` therefore accumulates wire bytes in a struct field using `read`, which guarantees nothing was consumed when it is cancelled. `read_exact` is **not** safe here: it discards a partial fill, which desyncs the stream and makes every later frame fail to decrypt. `fill_frame` also stops at the current frame's boundary so pipelined frames stay separate.
- **Writes go out in one call.** The socket is unbuffered, so each encrypted frame (length prefix + ciphertext + tag) is staged contiguously and written once. A separate 2-byte length write would become a tiny segment that Nagle holds back.

## PUT semantics

`handle_put_characteristics` distinguishes three shapes per entry:

- `{aid, iid, ev: true|false}` — toggle subscription on the per-connection set. No GPIO action.
- `{aid, iid, value: N}` where `N` matches the estimated current position — no-op unless it cancels a pending timed move, in which case the controller sends `stop`.
- `{aid, iid, value: N}` with a real change — asks the shared controller to move from the estimated current position to `N`. The controller sends `up` or `down`, emits `TargetPosition` plus moving `PositionState`, and stops the motor per the [timed positioning](HARDWARE.md#timed-positioning) rules. Completion updates `CurrentPosition`, persists `positions.json`, and emits stopped events. HAP EVENT frames for those updates are published only from the position bridge (not duplicated on the PUT write outcome).

## Lifecycle

`serve` races the HTTP server against a shutdown signal handler that catches both SIGINT (Ctrl-C) and SIGTERM (`systemctl stop somfy`). On shutdown it aborts the HAP listener and position-bridge tasks held in `HomekitHandles`, then drops the `Announcement` guard, which calls `daemon.unregister(...)` then `daemon.shutdown()` to stop the mdns-sd worker threads.

## Dependencies

`mdns-sd`, `ed25519-dalek`, `x25519-dalek`, `chacha20poly1305`, `hkdf`, `sha2`, `rand`, `httparse`, `http`, `num-bigint`, `qrcode`. All pure Rust, all cross-compile cleanly for `armv7-unknown-linux-gnueabihf`.

## Accessory Identity Stability

Home remembers accessories by the persisted identity in `hap.json` and by stable accessory IDs (`BLINDS` in `positioning/state.rs`) and characteristic IDs (`IID_*` in `homekit/accessory_db.rs`). Treat these values as compatibility surfaces:

- Keep `device_id`, `setup_id`, and the long-term signing key stable across upgrades. Regenerating them is a factory reset and forces re-pairing.
- Keep AIDs and IIDs stable once shipped. Changing them can make Home lose room/name/automation associations.
- Bump `config_number` only when the exposed accessory schema changes.
- Avoid renaming the bridge model/name constants casually. User-visible names can be changed in Home, but changing advertised defaults can make debugging paired devices harder.
