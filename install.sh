#!/usr/bin/env bash
set -euo pipefail

REPO="melkir/remote-gpio"
TARGET_ARCH="armv7l"
INSTALL_ARGS=()
GLOBAL_ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help)
            echo "Usage: install.sh [--config PATH] [--user USER]"
            exit 0
            ;;
        --config)
            if [[ $# -lt 2 ]]; then
                echo "error: --config requires a value" >&2
                exit 1
            fi
            GLOBAL_ARGS+=("--config" "$2")
            shift 2
            ;;
        --user)
            if [[ $# -lt 2 ]]; then
                echo "error: --user requires a value" >&2
                exit 1
            fi
            INSTALL_ARGS+=("--user" "$2")
            shift 2
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
/usr/local/bin/somfy "${GLOBAL_ARGS[@]}" install "${INSTALL_ARGS[@]}"

cat <<EOF

HomeKit pairing: the somfy binary advertises itself natively over mDNS.
Show the pairing QR code and setup code:

  somfy homekit status

In the iOS Home app: Add Accessory → scan the QR code.
EOF
