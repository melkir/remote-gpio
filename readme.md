## RemoteGPIO

### Development

You need to have [cross](https://github.com/rust-embedded/cross) installed.

```bash
cargo install cross --git https://github.com/cross-rs/cross
```

As well as [podman](https://podman.io/).

```bash
brew install podman
```

Start the podman machine.

```bash
podman machine init
podman machine start
```

Then build the project.

```bash
cross build
```

Te clean the project.

```bash
podman machine stop
podman machine rm -f podman-machine-default
```

Copy the binary and the .env file to the Raspberry Pi.

```bash
scp target/armv7-unknown-linux-gnueabihf/release/remote-gpio pi@192.168.1.18:/home/pi/Documents/remote-gpio
scp .env pi@192.168.1.18:/home/pi/Documents/remote-gpio
scp -r assets/ pi@192.168.1.18:/home/pi/Documents/remote-gpio/
```

Run the binary on the raspberry pi.

```bash
ssh -t pi@192.168.1.18 "cd /home/pi/Documents/remote-gpio ; bash --login"
./remote-gpio
```

Troubleshooting

Check if the GPIO pins are used by another process by running the following command.

```bash
lsof | grep gpio
```

If there is a message of disk quota exceeded, you can increase the number of available keys (by default it is set to 200).

```bash
podman machine ssh sudo sysctl -w kernel.keys.maxkeys=20000
```
