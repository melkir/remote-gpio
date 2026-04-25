#!/usr/bin/env bash
set -euo pipefail

REPO="melkir/remote-gpio"
TARGET_ARCH="armv7l"

WITH_HOMEKIT=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --with-homekit) WITH_HOMEKIT=1; shift ;;
        -h|--help)
            cat <<EOF
Usage: install.sh [--with-homekit]

  --with-homekit  Also install Homebridge and the homebridge-somfy-remote
                  plugin so the blinds show up in Apple Home.
EOF
            exit 0
            ;;
        *) echo "error: unknown flag: $1" >&2; exit 1 ;;
    esac
done

arch="$(uname -m)"
if [[ "$arch" != "$TARGET_ARCH" ]]; then
    echo "error: somfy only ships for $TARGET_ARCH (Raspberry Pi armv7). Detected: $arch" >&2
    exit 1
fi

if [[ $EUID -ne 0 ]]; then
    echo "error: run with sudo" >&2
    exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Fetching latest stable release metadata..."
api="https://api.github.com/repos/$REPO/releases/latest"
release_json="$(curl -fsSL "$api")"

asset_url="$(echo "$release_json" \
    | grep -E '"browser_download_url":.*/somfy"$' \
    | head -1 | cut -d '"' -f 4)"

sums_url="$(echo "$release_json" \
    | grep -E '"browser_download_url":.*/SHA256SUMS"' \
    | head -1 | cut -d '"' -f 4)"

if [[ -z "${asset_url:-}" ]]; then
    echo "error: could not find somfy asset in latest release" >&2
    exit 1
fi

if [[ -z "${sums_url:-}" ]]; then
    echo "error: release is missing SHA256SUMS; cannot verify integrity" >&2
    exit 1
fi
curl -fsSL "$sums_url" -o "$tmp/SHA256SUMS"

echo "Downloading somfy..."
curl -fsSL "$asset_url" -o "$tmp/somfy"

verify_checksum() {
    local file="$1"
    local line
    line="$(grep -E "[[:space:]]+\*?${file}$" "$tmp/SHA256SUMS" || true)"
    if [[ -z "$line" ]]; then
        echo "error: ${file} has no entry in SHA256SUMS" >&2
        exit 1
    fi
    (cd "$tmp" && printf '%s\n' "$line" | sha256sum -c -)
}

verify_checksum somfy

install -m 0755 "$tmp/somfy" /usr/local/bin/somfy

echo "Running somfy install..."
# Preserve SUDO_USER so the unit runs as the invoking user
/usr/local/bin/somfy install

if [[ "$WITH_HOMEKIT" == "1" ]]; then
    if ! command -v hb-service >/dev/null 2>&1; then
        echo "Installing Homebridge from repo.homebridge.io..."
        curl -fsSL https://repo.homebridge.io/KEY.gpg \
            | gpg --dearmor --yes -o /usr/share/keyrings/homebridge.gpg
        echo "deb [signed-by=/usr/share/keyrings/homebridge.gpg] https://repo.homebridge.io stable main" \
            > /etc/apt/sources.list.d/homebridge.list
        apt-get update
        apt-get install -y homebridge
    else
        echo "Homebridge already installed; skipping apt step."
    fi

    echo "Installing homebridge-somfy-remote plugin..."
    hb-service add homebridge-somfy-remote

    cat <<EOF

HomeKit bootstrap complete. Next:
  1. Open the Homebridge UI at http://$(hostname -I | awk '{print $1}'):8581
  2. On your iPhone: Home → Add Accessory → More Options → scan the
     Homebridge setup code shown in the UI.
EOF
fi
