#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -f "$SCRIPT_DIR/.env" ]; then
    set -a
    . "$SCRIPT_DIR/.env"
    set +a
fi

: "${RASP_IP:?RASP_IP not set (define in .env)}"
: "${RASP_USER:?RASP_USER not set (define in .env)}"
: "${REMOTE_DIR:?REMOTE_DIR not set (define in .env)}"
RASP_URL="$RASP_USER@$RASP_IP"
LOCAL_DIR="$(pwd)"

build() {
    # Build the frontend
    bun run --cwd app build

    # Cross-compile for Raspberry Pi using zig (targets glibc 2.31)
    cargo zigbuild --release --no-default-features --features hw --target armv7-unknown-linux-gnueabihf.2.31
}

deploy() {
    echo "Deploying to Raspberry Pi..."
    ssh $RASP_URL "rm -rf $REMOTE_DIR/dist/assets"

    rsync -az --progress \
        $LOCAL_DIR/target/armv7-unknown-linux-gnueabihf/release/remote-gpio \
        $LOCAL_DIR/app/dist \
        $RASP_URL:$REMOTE_DIR/

    ssh $RASP_URL "systemctl --user restart remote-gpio"
}

start() {
    # Check if the directory and files exist on the Raspberry Pi
    if ! ssh $RASP_URL "[ -d $REMOTE_DIR ] && [ -f $REMOTE_DIR/remote-gpio ]"; then
        echo "Remote directory or files not found. Building and deploying..."
        build
        deploy
    fi

    ssh -t $RASP_URL "cd $REMOTE_DIR && RUST_LOG=info ./remote-gpio"
    echo "Press 'r' to restart, or 'q' to quit."
    while true; do
        read -n 1 -s key
        if [ "$key" = "r" ]; then
            echo "Restarting..."
            build
            deploy
            ssh -t $RASP_URL "cd $REMOTE_DIR && RUST_LOG=info ./remote-gpio"
            echo "Application restarted. Press 'r' to restart again, or 'q' to quit."
        elif [ "$key" = "q" ]; then
            echo "Quitting..."
            exit 0
        fi
    done
}

delete() {
    ssh $RASP_URL "rm -rf $REMOTE_DIR"
    echo "Remote directory cleaned."
}

setup() {
    echo "Setting up systemd user service on Raspberry Pi..."
    ssh $RASP_URL "mkdir -p ~/.config/systemd/user"
    ssh $RASP_URL "cat > ~/.config/systemd/user/remote-gpio.service << 'EOF'
[Unit]
Description=Remote GPIO
After=network.target

[Service]
Type=simple
WorkingDirectory=$REMOTE_DIR
Environment=RUST_LOG=info
ExecStart=$REMOTE_DIR/remote-gpio
Restart=on-failure

[Install]
WantedBy=default.target
EOF"
    ssh $RASP_URL "sudo loginctl enable-linger $RASP_USER"
    ssh $RASP_URL "systemctl --user daemon-reload"
    ssh $RASP_URL "systemctl --user enable --now remote-gpio.service"
    echo "Service installed. Use: systemctl --user {start|stop|status|restart} remote-gpio"
}

case "$1" in
    build)
        build
        deploy
        ;;
    start)
        start
        ;;
    setup)
        setup
        ;;
    delete)
        delete
        ;;
    *)
        echo "Usage: $0 {build|start|setup|delete}"
        exit 1
        ;;
esac
