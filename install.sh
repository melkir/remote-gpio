#!/usr/bin/env bash
set -euo pipefail

REPO="melkir/remote-gpio"
TARGET_ARCH="armv7l"

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help)
            echo "Usage: install.sh"
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

cat <<EOF

HomeKit pairing: the somfy binary advertises itself natively over mDNS.
Find the setup code in the journal:

  sudo journalctl -u somfy | grep "setup code"

In the iOS Home app: Add Accessory → More Options → enter the code.
EOF
