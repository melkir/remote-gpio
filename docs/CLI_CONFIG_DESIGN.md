# CLI and Configuration Design

This note captures a proposed direction for simplifying the `somfy` command
line interface and introducing a small configuration file. It is intentionally
operator-facing: commands should be easy for humans to remember and regular
enough for AI tools to inspect, generate, and explain.

## Goals

- Make the common control path backend-neutral.
- Keep backend-specific details out of the user-facing command vocabulary.
- Expose backend diagnostics through structured service logs rather than separate backend command groups.
- Move persistent hardware choices out of command flags and systemd `ExecStart` strings.
- Use one configuration resolution model across `serve`, `doctor`, install flows, and one-shot commands.

## Current Friction

The current CLI leaks implementation details into operator workflows. Users
usually want to operate a channel, not think about whether the active backend is
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

Add a backend-neutral `remote` command group for normal operation:

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

`remote` should also cover commands that are currently backend-specific. For
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

Frame details and backend diagnostics should live in structured service logs,
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

Do not add a public `rts` command group. RTS is a backend implementation, not a
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

Do not expose persistent hardware settings as command flags. Backend selection,
RTS wiring, Telis GPIO mapping, and Telis-assisted programming should come from
the config file or built-in defaults.

Use the same value names everywhere:

```text
<channel> = L1 | L2 | L3 | L4 | ALL
<command> = up | down | stop | prog | select
<backend> = fake | telis | rts
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
choices:

```toml
backend = "rts"

[rts]
spi_device = "/dev/spidev0.0"
gdo0_gpio = 18
pigpiod_addr = "127.0.0.1:8888"
frame_count = 4

[telis.gpio]
up = 26
stop = 19
down = 13
select = 6
led1 = 21
led2 = 20
led3 = 16
led4 = 12
# prog = 5
```

The Telis GPIO defaults should match the wiring in
[HARDWARE.md](HARDWARE.md#connection-table). `prog` is optional: when it is
uncommented or otherwise configured, `somfy remote prog <channel>` can use the
wired Telis Prog button as part of the assisted RTS programming flow. If `prog`
is omitted, programming uses backend-native behavior.

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
the backend and hardware options.

## Telis-Assisted RTS Prog

Telis-assisted Prog should be config-driven by default. If `telis.gpio.prog` is
configured, this command:

```bash
somfy remote prog L1
```

should automatically run the assisted sequence:

1. Select the requested channel on the wired Telis backend if needed.
2. Press the configured Telis Prog GPIO.
3. Wait the configured delay.
4. Transmit the RTS Prog frame.

If no Telis Prog GPIO is configured, the same command should use backend-native
Prog behavior where supported.

Resolution rules:

```text
configured gpio  => Telis-assisted Prog by default
no gpio          => backend-native Prog
```

## Debug Logging

Debugging should not require a separate RTS command vocabulary. Instead, the
service and one-shot commands should emit structured backend events when debug
logging is enabled.

For the RTS backend, useful debug fields include:

- channel
- command
- rolling code
- remote id
- encoded frame bytes
- GPIO
- frame count
- pulse count
- total waveform duration

For the Telis backend, useful debug fields include:

- selected channel before and after command execution
- GPIO line being pulsed
- pulse duration
- observed LED input changes

Debug output should be available in the normal service logs. For systemd
installs, this means `journalctl -u somfy` should be enough to inspect the
backend behavior. A convenience command can wrap that:

```bash
somfy logs --follow
somfy logs --debug
```

`--debug` should follow logs with backend debug fields included. Persistent
debug mode can be added later if there is a real operator need, but it should
not be part of the first design because it introduces config mutation and
service restart behavior.

## Implementation Plan

1. Add config loading and `somfy config path/show/validate`.
2. Teach `serve`, `doctor`, `install`, and backend command execution to use the resolved config.
3. Add `somfy remote <command> [channel]` as the canonical control surface.
4. Move frame and GPIO diagnostics into structured debug logs.
5. Keep RTS-specific diagnostics in structured logs, not in a public command group.
6. Update README examples to use `remote` for normal operation and service logs for diagnostics.

This gives both humans and tools a smaller, more regular command vocabulary.
