#!/usr/bin/env bash
set -euo pipefail

REPO="melkir/remote-gpio"
TARGET_ARCH="armv7l"
INSTALL_ARGS=()
GLOBAL_ARGS=()

print_logo() {
    cat <<'EOF'

   ┌─────────────────────────────┐
   │   somfy · remote-gpio       │
   │   Pi-driven Somfy blinds    │
   └─────────────────────────────┘

EOF
}

usage() {
    cat <<EOF
Usage: install.sh [--config PATH] [--user USER]

  --config PATH    config file path (forwarded to somfy)
  --user USER      service user (defaults to \$SUDO_USER)

After install, switch driver with:  sudo somfy config set-driver <fake|telis|rts>
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help) usage; exit 0 ;;
        --config)
            [[ $# -ge 2 ]] || { echo "error: --config requires a value" >&2; exit 1; }
            GLOBAL_ARGS+=("--config" "$2"); shift 2 ;;
        --user)
            [[ $# -ge 2 ]] || { echo "error: --user requires a value" >&2; exit 1; }
            INSTALL_ARGS+=("--user" "$2"); shift 2 ;;
        *) echo "error: unknown flag: $1" >&2; usage >&2; exit 1 ;;
    esac
done

print_logo

# Batch preflight: collect all problems, report together.
errors=()

arch="$(uname -m)"
if [[ "$arch" != "$TARGET_ARCH" ]]; then
    errors+=("somfy only ships for $TARGET_ARCH (Raspberry Pi armv7). Detected: $arch")
fi

if [[ $EUID -ne 0 ]]; then
    errors+=("must be run as root (use sudo)")
fi

for cmd in curl sha256sum install; do
    command -v "$cmd" >/dev/null 2>&1 || errors+=("required command not found: $cmd")
done

if (( ${#errors[@]} > 0 )); then
    echo "Preflight failed:" >&2
    for e in "${errors[@]}"; do echo "  - $e" >&2; done
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

Switch driver (fake/telis/rts):

  sudo somfy config set-driver rts

HomeKit pairing: the somfy binary advertises itself natively over mDNS.
Show the pairing QR code and setup code:

  somfy homekit status

In the iOS Home app: Add Accessory → scan the QR code.
EOF
