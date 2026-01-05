#!/bin/bash

RASPBERRY_PI_IP="192.168.1.18"
REMOTE_DIR="/home/pi/Documents/remote-gpio"
LOCAL_DIR="$(pwd)"

build() {
    # Build the frontend
    cd app && bun run build
    cd ..

    # Cross-compile for Raspberry Pi using zig (targets glibc 2.31)
    cargo zigbuild --release --target armv7-unknown-linux-gnueabihf.2.31
}

deploy() {
    echo "Deploying to Raspberry Pi..."
    ssh pi@$RASPBERRY_PI_IP "rm -rf $REMOTE_DIR/dist/assets"

    rsync -az --progress \
        $LOCAL_DIR/target/armv7-unknown-linux-gnueabihf/release/remote-gpio \
        $LOCAL_DIR/app/dist \
        pi@$RASPBERRY_PI_IP:$REMOTE_DIR/
}

start() {
    # Check if the directory and files exist on the Raspberry Pi
    if ! ssh pi@$RASPBERRY_PI_IP "[ -d $REMOTE_DIR ] && [ -f $REMOTE_DIR/remote-gpio ]"; then
        echo "Remote directory or files not found. Building and deploying..."
        build
        deploy
    fi

    ssh -t pi@$RASPBERRY_PI_IP "cd $REMOTE_DIR && RUST_LOG=info ./remote-gpio"
    echo "Press 'r' to restart, or 'q' to quit."
    while true; do
        read -n 1 -s key
        if [ "$key" = "r" ]; then
            echo "Restarting..."
            build
            deploy
            ssh -t pi@$RASPBERRY_PI_IP "cd $REMOTE_DIR && RUST_LOG=info ./remote-gpio"
            echo "Application restarted. Press 'r' to restart again, or 'q' to quit."
        elif [ "$key" = "q" ]; then
            echo "Quitting..."
            exit 0
        fi
    done
}

delete() {
    ssh pi@$RASPBERRY_PI_IP "rm -rf $REMOTE_DIR"
    echo "Remote directory cleaned."
}

case "$1" in
    build)
        build
        deploy
        ;;
    start)
        start
        ;;
    delete)
        delete
        ;;
    *)
        echo "Usage: $0 {build|start|delete}"
        exit 1
        ;;
esac
