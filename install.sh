#!/usr/bin/env bash
set -euo pipefail

REPO="melkir/server-remote-gpio"
TARGET_ARCH="armv7l"

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
    | grep -E '"browser_download_url":.*/somfy"' \
    | head -1 | cut -d '"' -f 4)"

sums_url="$(echo "$release_json" \
    | grep -E '"browser_download_url":.*/SHA256SUMS"' \
    | head -1 | cut -d '"' -f 4)"

if [[ -z "${asset_url:-}" ]]; then
    echo "error: could not find somfy asset in latest release" >&2
    exit 1
fi

echo "Downloading somfy..."
curl -fsSL "$asset_url" -o "$tmp/somfy"

if [[ -n "${sums_url:-}" ]]; then
    curl -fsSL "$sums_url" -o "$tmp/SHA256SUMS"
    (cd "$tmp" && sha256sum -c SHA256SUMS)
else
    echo "warning: no SHA256SUMS in release; skipping checksum verification" >&2
fi

install -m 0755 "$tmp/somfy" /usr/local/bin/somfy

echo "Running somfy install..."
# Preserve SUDO_USER so the unit runs as the invoking user
/usr/local/bin/somfy install
