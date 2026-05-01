# CLI and Configuration Design

This note captures a proposed direction for simplifying the `somfy` command
line interface and introducing a small configuration file. It is intentionally
operator-facing: commands should be easy for humans to remember and regular
enough for AI tools to inspect, generate, and explain.

## Goals

- Make the common control path driver-neutral.
- Keep driver-specific details out of the user-facing command vocabulary.
- Expose driver diagnostics through structured service logs rather than separate driver command groups.
- Move persistent hardware choices out of command flags and systemd `ExecStart` strings.
- Use one configuration resolution model across `serve`, `doctor`, install flows, and one-shot commands.

## Current Friction

The current CLI leaks implementation details into operator workflows. Users
usually want to operate a channel, not think about whether the active driver is
RTS or Telis.

Persistent settings are also split across command flags, environment variables,
systemd `ExecStart`, and state files. This makes non-default hardware setups
harder to inspect and easy to drift.

## Proposed Command Model

The target command surface should be:

```bash
somfy serve
somfy install
somfy upgrade
somfy doctor
somfy restart
somfy uninstall

somfy remote up [channel]
somfy remote down [channel]
somfy remote stop [channel]
somfy remote select <channel>
somfy remote prog <channel>
somfy remote status
somfy remote watch

somfy homekit status
somfy homekit pairings
somfy homekit unpair <identifier>
somfy homekit reset

somfy logs
somfy logs --follow
somfy logs --debug

somfy config path
somfy config show
somfy config validate
```

Keep lifecycle commands at the top level because they are already common,
short, and clear:

```bash
somfy serve
somfy install
somfy upgrade
somfy doctor
somfy restart
somfy uninstall
```

Add a driver-neutral `remote` command group for normal operation:

```bash
somfy remote status
somfy remote watch
somfy remote <command> [channel]
```

Examples:

```bash
somfy remote up
somfy remote stop
somfy remote up L1
somfy remote down ALL
somfy remote prog L1
somfy remote select L3
```

`remote` should also cover commands that are currently driver-specific. For
example, programming a channel should be expressed as:

```bash
somfy remote prog L1
```

not as an RTS-specific command.

Keep `homekit` as its own domain:

```bash
somfy homekit status
somfy homekit pairings
somfy homekit unpair <identifier>
somfy homekit reset
```

Add `logs` as the observability surface:

```bash
somfy logs
somfy logs --follow
somfy logs --debug
```

Frame details and driver diagnostics should live in structured service logs,
rather than in separate operator-facing RTS commands. `somfy logs` should be a
convenience wrapper around the service logs.

Add a `config` domain for inspectability:

```bash
somfy config path
somfy config show
somfy config validate
```

`config show` should print the resolved configuration by default. The raw file
can be inspected directly when needed.

Do not add a public `rts` command group. RTS is a driver implementation, not a
top-level operator domain.

## Argument Rules

Use positional arguments for required domain values:

```bash
somfy remote <command>
somfy remote <command> <channel>
somfy homekit unpair <identifier>
```

Use flags only for command behavior or output format:

```bash
--config <path>
--json
--verbose
--debug
--dry-run
```

Do not expose persistent hardware settings as command flags. Driver selection,
RTS wiring, Telis GPIO mapping, and Telis-assisted programming should come from
the config file or built-in defaults.

The current driver and RTS hardware flags should be removed as part of this
cleanup. That includes `--driver`, `--rts-spi-device`, `--rts-gdo0-gpio`,
`--pigpiod-addr`, `--rts-frame-count`, and Telis Prog wiring flags.

Use the same value names everywhere:

```text
<channel> = L1 | L2 | L3 | L4 | ALL
<command> = up | down | stop | prog | select
<driver> = fake | telis | rts
```

Remote command channel rules:

| Command           | Channel rule                                       |
| ----------------- | -------------------------------------------------- |
| `up`, `down`      | Optional; defaults to the current selected channel |
| `stop`            | Optional; defaults to the current selected channel |
| `select`          | Required                                           |
| `prog`            | Required                                           |
| `status`, `watch` | Not accepted                                       |

When no channel is provided for `up`, `down`, or `stop`, the command
targets the currently selected channel:

```bash
somfy remote up
somfy remote down
somfy remote stop
```

`stop` sends the Somfy middle-button command. While a motor is moving it stops
movement; while idle it may move to the stored favorite position. The CLI uses
only `stop` to avoid two names for the same signal.

`prog` requires a channel because programming the wrong motor is high-friction
to undo. It should not default to the current selected channel.

## Configuration File

Introduce a small TOML configuration file for persistent hardware and service
choices. Keep the canonical example in [HARDWARE.md](HARDWARE.md#configuration)
so operator docs do not drift.

The Telis GPIO defaults should match the wiring in
[HARDWARE.md](HARDWARE.md#connection-table). `prog` is optional: when it is
uncommented or otherwise configured, `somfy remote prog <channel>` can use the
wired Telis Prog button as part of the assisted RTS programming flow. If `prog`
is omitted, programming uses driver-native behavior.

Recommended locations:

- system install: `/etc/somfy/config.toml`
- explicit override: `somfy --config /path/to/config.toml ...`

Avoid adding internal constants to config until there is a real operator need.
Ports, HomeKit metadata, Telis press durations, RTS Prog delay timing, and
protocol internals should stay code defaults unless users need to change them.

## Configuration Precedence

Resolve persistent settings with one simple rule:

```text
config file > built-in defaults
```

`--config <path>` only chooses which config file to read; it does not override
individual settings. There should be no environment-variable layer for hardware
configuration. This keeps installs, diagnostics, and AI-readable command output
simple.

Config resolution should produce one resolved config object used by:

- `serve`
- `doctor`
- `install`
- `remote`
- logging/debug setup
- `config show`

`doctor` should validate the resolved config rather than re-parsing systemd
`ExecStart` as the source of truth. The systemd unit should point at the binary
and config path, while the config file carries persistent hardware choices.
`install` should install the service from the resolved config; config chooses
the driver and hardware options.

## Flag and Env Cleanup

Remove persistent hardware configuration from the command line and environment.
These settings should come from config or built-in defaults:

- `somfy serve --driver`
- `SOMFY_BACKEND`
- `--rts-spi-device`
- `--rts-gdo0-gpio`
- `--pigpiod-addr`
- `--rts-frame-count`
- `SOMFY_RTS_SPI_DEVICE`
- `SOMFY_RTS_GDO0_GPIO`
- `SOMFY_PIGPIOD_ADDR`
- `SOMFY_RTS_FRAME_COUNT`
- `somfy install --driver`
- `install.sh --driver`
- `somfy rts prog --with-telis`
- `--telis-gpio`
- `--telis-press-ms`
- `--telis-delay-ms`

Keep flags that control one command's behavior or output rather than persistent
hardware state:

- `--config <path>` to choose the config file
- `--json`
- `--verbose`
- `--version`
- `--help`
- `homekit status --uri-only`
- `upgrade --channel`
- `upgrade --version`
- `upgrade --check`
- `logs --follow`
- `logs --debug`

Keep install-time service-user discovery:

- `somfy install --user`
- `SUDO_USER`

Keep `STATE_DIRECTORY` because it is the systemd-owned state directory contract.
Avoid adding a second Somfy-specific environment override for production config.
If a developer-only state override remains useful, keep it out of operator docs
and tests should prefer temporary config/state directories where practical.

## Telis-Assisted RTS Prog

Telis-assisted Prog should be config-driven by default. If `telis.gpio.prog` is
configured, this command:

```bash
somfy remote prog L1
```

should automatically run the assisted sequence:

1. Select the requested channel on the wired Telis driver if needed.
2. Press the configured Telis Prog GPIO.
3. Wait the configured delay.
4. Transmit the RTS Prog frame.

If no Telis Prog GPIO is configured, the same command should use driver-native
Prog behavior where supported.

Resolution rules:

```text
configured gpio  => Telis-assisted Prog by default
no gpio          => driver-native Prog
```

## Debug Logging

Debugging should not require a separate RTS command vocabulary. Instead, the
service and one-shot commands should emit structured driver events when debug
logging is enabled.

For the RTS driver, useful debug fields include:

- channel
- command
- rolling code
- remote id
- encoded frame bytes
- GPIO
- frame count
- pulse count
- total waveform duration

For the Telis driver, useful debug fields include:

- selected channel before and after command execution
- GPIO line being pulsed
- pulse duration
- observed LED input changes

Debug output should be available in the normal service logs. For systemd
installs, this means `journalctl -u somfy` should be enough to inspect the
driver behavior. A convenience command can wrap that:

```bash
somfy logs --follow
somfy logs --debug
```

`--debug` should follow logs with driver debug fields included. Persistent
debug mode can be added later if there is a real operator need, but it should
not be part of the first design because it introduces config mutation and
service restart behavior.

## Implementation Plan

1. Add config loading and `somfy config path/show/validate`.
2. Teach `serve`, `doctor`, `install`, and driver command execution to use the resolved config.
3. Add `somfy remote <command> [channel]` as the canonical control surface.
4. Move frame and GPIO diagnostics into structured debug logs.
5. Keep RTS-specific diagnostics in structured logs, not in a public command group.
6. Update README examples to use `remote` for normal operation and service logs for diagnostics.

This gives both humans and tools a smaller, more regular command vocabulary.
