# Native HomeKit (HAP) Server

`somfy serve` runs a native HomeKit Accessory Protocol server alongside the HTTP/WebSocket API. The Home app talks straight to the Rust binary — no Homebridge, no Node.

## Wire layout

- **Port `5010`** — dedicated TCP listener. Kept off `:5002` because post-`Pair-Verify` traffic upgrades the socket into HAP's custom AEAD framing, which doesn't fit axum's request/response model.
- **mDNS** — `_hap._tcp.local.` advertised via `mdns-sd`. TXT record carries `id`, `c#`, `s#`, `sf`, `ci=14` (Bridge), `md`, `pv=1.1`. The `Announcement` guard's `Drop` impl unregisters and shuts the daemon's worker threads.
- **Accessory database** — Bridge (`aid=1`) plus 5 bridged `WindowCovering` accessories (`aid=2..6`), one per Somfy LED selector (`L1`–`L4`, `ALL`). IIDs are stable across runs; `config_number` must bump if the schema ever changes.

## Persistent state

`$STATE_DIRECTORY` (set by systemd via `StateDirectory=somfy`, falls back to `./hap-state` for `cargo run`):

| File             | Owner          | Contents                                                                                |
| ---------------- | -------------- | --------------------------------------------------------------------------------------- |
| `hap.json`       | `state.rs`     | device id, setup code, Ed25519 long-term signing key, `c#`/`s#`, paired controllers     |
| `positions.json` | `positions.rs` | aid → last-known position (0 or 100). Reload is **read-only** — never replayed to GPIO. |

Both files are written atomically (tmp + `rename`) with mode `0600`. systemd preserves them across `somfy upgrade`.

## Crypto + protocol

| Concern             | Implementation                                                                                                                              |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| TLV8 codec          | `src/hap/tlv.rs` with fragment reassembly (HAP §14.1).                                                                                      |
| SRP-6a / SHA-512    | In-tree `src/hap/srp.rs` over the 3072-bit group (RFC 5054). The upstream `srp` crate ships only the simplified M1 form, which iOS rejects. |
| Pair-Setup (M1–M6)  | `src/hap/pair_setup.rs` — username `Pair-Setup`, AccessoryX/iOSX derived per spec, signed Ed25519 proofs.                                   |
| Pair-Verify (M1–M4) | `src/hap/pair_verify.rs` — X25519 ECDH, Ed25519 mutual auth, HKDF-SHA512 → session keys.                                                    |
| Session framing     | `src/hap/session.rs` — ChaCha20-Poly1305 with 2-byte length AAD, per-direction nonces, max plaintext 1024.                                  |
| HTTP                | Hand-rolled on `tokio` + `httparse` (no axum). Both plain and encrypted readers feed the same parser.                                       |

## Connection lifecycle

`src/hap/server.rs::handle_connection` runs a single `tokio::select!`:

1. **Plain phase** — `POST /pair-setup`, `POST /pair-verify`. After M4 verifies, the reader/writer are upgraded to encrypted halves and the connection switches to the control channel.
2. **Control phase** — `GET /accessories`, `GET /characteristics`, `PUT /characteristics`, `POST /pairings`. All require an encrypted writer; otherwise we return `401`.
3. **Event push** — every connection holds a per-socket `HashSet<(aid, iid)>` of subscribed characteristics and a `broadcast::Receiver<Vec<(u64, u8)>>`. After a successful `PUT`, the handler updates the commanded aid and broadcasts that direct change; subscribers fan it out as `EVENT/1.0` frames over the same encrypted writer.

EVENT push is what resolves the iOS "Closing…" / "Opening…" spinner (waits on `PositionState=2` + `CurrentPosition` matching `TargetPosition`). The **All Blinds** tile is tracked as its own physical remote selection, not as a derived aggregate of the four individual blinds.

## PUT semantics

`handle_put_characteristics` distinguishes three shapes per entry:

- `{aid, iid, ev: true|false}` — toggle subscription on the per-connection set. No GPIO action.
- `{aid, iid, value: N}` where the snapped value (`< 50` → 0, `≥ 50` → 100) **matches the cached position** — no-op. iOS replays the last-known `TargetPosition` right after pairing; without this the bridge would fire UP on every registration.
- `{aid, iid, value: N}` with a real change — funnels through `RemoteControl::execute(Some(led), command)`, the single command layer shared by REST, WebSocket, and HAP. Then updates that aid's cached position, persists `positions.json`, and broadcasts a change event.

## Doctor

`somfy doctor` includes a `hap_state` check: missing → advisory ("created on first serve"), unreadable / malformed → blocking, otherwise OK with `id=… paired=N`.

## Lifecycle

`serve` selects between the HTTP server and a shutdown signal handler that catches both SIGINT (Ctrl-C) and SIGTERM (`systemctl stop somfy`). On either path the `Announcement` guard drops, which calls `daemon.unregister(...)` then `daemon.shutdown()`, terminating the mdns-sd worker threads. The HAP listener task is detached on `tokio::spawn` and dies with the runtime.

## Dependencies

`mdns-sd`, `ed25519-dalek`, `x25519-dalek`, `chacha20poly1305`, `hkdf`, `rand`, `httparse`, `num-bigint`. All pure Rust, all cross-compile cleanly for `armv7-unknown-linux-gnueabihf`.

## Pairing

Setup code is logged at startup:

```bash
ssh pi journalctl -u somfy | grep "setup code"
```

In iOS Home → Add Accessory → More Options → enter the code. The Bridge appears as **Somfy XXXXXX** with five `WindowCovering` tiles inside.
