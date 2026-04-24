# homebridge-somfy-remote

Homebridge plugin that exposes a Raspberry Pi-attached Somfy Telis 4 remote (driven by [`somfy`](../README.md)) as HomeKit `WindowCovering` accessories so Siri, the iOS Home app, and HomePod can control the blinds.

Talks to the existing `somfy serve` HTTP API — no changes to the Rust side. Each accessory maps a HomeKit position to `POST /command`:

- Target ≥ 50 → `{"command": "up", "led": "<LED>"}`
- Target < 50 → `{"command": "down", "led": "<LED>"}`

Current position snaps to the target immediately (no progress simulation, since the hardware gives no position feedback).

## Install

The simplest path is to re-run the bootstrap script with the `--with-homekit` flag — it handles adding the Homebridge apt repo, installing Homebridge, and installing this plugin:

```bash
curl -fsSL https://raw.githubusercontent.com/melkir/remote-gpio/main/install.sh | sudo bash -s -- --with-homekit
```

Safe to run on a box that already has `somfy` installed; `somfy install` is idempotent.

If you'd rather install manually:

```bash
sudo apt install -y homebridge            # needs the homebridge apt repo configured
sudo npm install -g https://github.com/melkir/remote-gpio/releases/latest/download/homebridge-somfy-remote.tgz
sudo hb-service restart
```

The tarball is produced by CI on every tagged release (`.github/workflows/release.yml`) and the URL always redirects to the latest stable. For local development, `mise run homebridge-pack` writes the same tarball to `target/homebridge/` so you can install that path directly.

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
    { "name": "Kitchen",     "led": "L2" },
    { "name": "All",         "led": "ALL" }
  ]
}
```

## Pair

Scan the Homebridge setup code from the Home app → Add Accessory → More Options.
