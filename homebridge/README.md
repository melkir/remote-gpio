# homebridge-somfy-remote

Homebridge plugin that exposes a Raspberry Pi-attached Somfy Telis 4 remote (driven by [`somfy`](../README.md)) as HomeKit `WindowCovering` accessories so Siri, the iOS Home app, and HomePod can control the blinds.

Talks to the existing `somfy serve` HTTP API — no changes to the Rust side. Each accessory maps a HomeKit position to `POST /command`:

- Target ≥ 50 → `{"command": "up", "led": "<LED>"}`
- Target < 50 → `{"command": "down", "led": "<LED>"}`

Current position snaps to the target immediately (no progress simulation, since the hardware gives no position feedback).

## Install

Run the bootstrap script with `--with-homekit` to add the Homebridge apt repo, install Homebridge, and install this plugin via `hb-service add`:

```bash
curl -fsSL https://raw.githubusercontent.com/melkir/remote-gpio/main/install.sh | sudo bash -s -- --with-homekit
```

Safe to re-run on a box that already has `somfy` installed; `somfy install` is idempotent. Updates flow through the Homebridge UI at `http://<pi>:8581` → **Plugins**, which polls the npm registry where CI publishes each tagged release.

For local development, `mise run homebridge-pack` writes a tarball to `target/homebridge/` so you can `sudo hb-service add /path/to/homebridge-somfy-remote.tgz` into a dev Homebridge.

## Config

Add to Homebridge's `config.json` under `platforms`:

```json
{
  "platform": "SomfyRemote",
  "name": "Somfy Remote",
  "baseUrl": "http://localhost:5002"
}
```

Defaults provision one accessory per LED (L1–L4) plus an "All Blinds" entry that targets the remote's ALL mode. Override with a `blinds` array if you want custom names:

```json
{
  "platform": "SomfyRemote",
  "baseUrl": "http://localhost:5002",
  "blinds": [
    { "name": "Living Room", "led": "L1" },
    { "name": "Kitchen", "led": "L2" },
    { "name": "All", "led": "ALL" }
  ]
}
```

## Pair

Scan the Homebridge setup code from the Home app → Add Accessory → More Options.

## Behavior

**Position mapping.** HomeKit models blinds on a 0–100 slider. This plugin snaps any write to either end: `target ≥ 50` fires `up`, `target < 50` fires `down`, and the current position updates immediately to match. There's no travel animation because the Somfy remote gives no position feedback — the Pi only knows which LED is lit.

**No MY / STOP.** HomeKit's `WindowCovering` service has no "stop" verb that a user-facing control can trigger, so the plugin only emits `up` / `down`. If you need to stop a blind mid-travel or use the Somfy MY position, use the Preact PWA, a Siri Shortcut that `POST`s `{"command": "stop"}` to `/command`, or the remote itself. `HoldPosition` via Home automations is not wired up.

**Errors.** A failed or timed-out `POST /command` (default timeout 5s, override with `requestTimeoutMs`) logs an error line and throws `SERVICE_COMMUNICATION_FAILURE` back to HomeKit, which surfaces in the Home app as "No Response" for that accessory. The plugin does not retry — HomeKit re-issues the write if the user taps again. Non-numeric `TargetPosition` writes are rejected with `INVALID_VALUE_IN_REQUEST` rather than defaulting to `down`.

## Troubleshooting

- **"No Response" in Home.** Check `ssh pi somfy doctor` (GPIO + service state) and `ssh pi curl -s localhost:5002/led` (backend reachable). Tail `journalctl -u homebridge -f` for the plugin's own error lines.
- **Pairing fails.** Make sure Homebridge and `somfy` are on the same LAN as the iPhone and that multicast (mDNS) isn't blocked by the router. The Homebridge UI at `http://<pi>:8581` is the source of truth for the current setup code.
- **Remote doesn't react.** The plugin always fires a SELECT chain for the configured LED before `up` / `down`. Verify the physical remote cycles through L1–L4 → ALL as expected by watching the LEDs in the PWA while triggering the accessory.
