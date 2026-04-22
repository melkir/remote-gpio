# Deploy Design

Pull-based deploy with a manual operator step. The Raspberry Pi runs a self-managing binary (`somfy`) installed at `/usr/local/bin/somfy` and supervised by a system-level systemd unit. CI builds and publishes artifacts; updating the Pi is done explicitly over SSH by running `sudo somfy upgrade` on the box.

## How it works

1. Merge to `main` → GitHub Actions cross-compiles `somfy` for armv7 (frontend embedded inside the binary) and publishes or refreshes a moving prerelease for the `main` branch.
2. Push a tag like `v0.2.0` → GitHub Actions publishes a stable release asset.
3. When you want to update the Pi, SSH in and run `sudo somfy upgrade` (stable) or `sudo somfy upgrade --channel main` (latest branch build from `main`).

That's the whole loop. No rsync deploys, no CI access to the Pi, no always-on web updater.

The old `remote-gpio.sh` workflow is no longer part of the design. The operator commands now live directly in the README.

## Stack

- **Rust + clap** — binary subcommands, with `serve` used explicitly in automation.
- `build.rs` + `vergen` — embeds git SHA at compile time. The binary knows its own version.
- `rust-embed` — bundles `app/dist/` into the binary. Debug builds read from disk for hot-reload; release builds embed.
- **GitHub Actions + `cargo-zigbuild`** — cross-compiles for the single supported Pi target, `armv7-unknown-linux-gnueabihf.2.31`, and uploads the release asset.

## The binary: `somfy`

```text
somfy                                   # serve (default, for local convenience)
somfy serve                             # explicit serve command
sudo somfy install                      # idempotent: write/refresh systemd unit for the invoking user, daemon-reload, enable --now
sudo somfy upgrade                      # pull latest stable release, stop, replace, start
sudo somfy upgrade --channel main       # pull the moving prerelease for the main branch
sudo somfy upgrade --version vX         # pin or roll back to a specific release
somfy upgrade --check                   # report if newer release exists, no-op
somfy doctor                            # check everything, exit non-zero if any red
somfy doctor --json                     # machine-readable
sudo somfy uninstall                    # disable + remove unit
somfy --version                         # embedded git SHA + build date
```

Read-only verbs (`serve`, `doctor`, `upgrade --check`, `--version`) work without sudo. Anything that writes to `/usr/local/bin` or `/etc/systemd/system` requires sudo — which is appropriate for a privileged operation and matches Brew/Bun conventions.

### System systemd unit

The unit lives at `/etc/systemd/system/somfy.service`, runs as the existing operator account in the `gpio` group, starts on boot, and should use `ExecStart=/usr/local/bin/somfy serve`. No `loginctl enable-linger` needed.

`sudo somfy install` should default the service user from `SUDO_USER`, so `ssh pi sudo somfy install` naturally runs the service as the SSH login user. If `install` is run directly as root without `SUDO_USER`, require an explicit `--user <pi-user>`.

The unit file is a template inside the binary, rendered from the binary's own paths on `install`. Re-running `install` reconciles drift after an upgrade or a manual edit. `install` is idempotent: it compares the on-disk unit against what it would generate and only writes if different.

### `upgrade` flow

1. Query GitHub releases API for the latest stable release, the moving `main` branch prerelease, or the requested tag.
2. If already on that version, exit 0.
3. Download the armv7 asset to a temp file and verify checksum.
4. Move the current binary to `/usr/local/bin/somfy.prev`.
5. Stop the service with `systemctl stop somfy`.
6. Replace `/usr/local/bin/somfy` with the new binary.
7. Run `install` (idempotent — refreshes unit if the template changed).
8. Start the service with `systemctl start somfy`.
9. Wait briefly for the service to become active, then run `doctor`.
10. If `systemctl start somfy` fails, the service never becomes active within the timeout, or `doctor` reports a blocking failure, put `somfy.prev` back in place and start the previous version again.

### `doctor` checks

Single JSON contract, the source of truth for "is this thing healthy":

- unit installed at `/etc/systemd/system/somfy.service`?
- unit content matches what this binary would generate? (drift detection)
- service active / failed / inactive?
- unit `ExecStart` matches `/usr/local/bin/somfy serve`?
- configured service user exists and is in `gpio` group? (defaults to the invoking SSH user)
- GPIO chip accessible (open `/dev/gpiochip0`)?
- newer stable release or newer `main` branch build available on GitHub?
- deployed SHA + build date

Exits 0 if all green, non-zero otherwise.

### `serve` startup

On `somfy serve`, run doctor and print a Flutter-style summary to stdout (which becomes `journalctl -u somfy`). Don't auto-install or auto-upgrade — those are explicit operator actions.

```
somfy v0.1.8 (sha abc1234, built 2026-04-15)
Doctor summary (run `somfy doctor -v` for details):
[✓] Binary       (/usr/local/bin/somfy)
[✓] Systemd unit (in sync)
[✓] User & perms (service user in gpio group)
[✓] GPIO         (/dev/gpiochip0 accessible)
[!] Updates      (v0.2.0 available)

! 1 advisory. Serving on 0.0.0.0:5002.
```

Severity:

- `[✓]` healthy.
- `[!]` advisory — log and continue (e.g. update available, drifted unit).
- `[✗]` blocking — abort startup (e.g. GPIO chip inaccessible, unit `ExecStart` mismatch).

The "updates available" check is a network call to GitHub. Use a short timeout (~2s) and fail soft — never block startup on GitHub reachability.

## CI/CD

One workflow at `.github/workflows/release.yml`, triggered on pushes to `main`, tag pushes (`v*`), and optionally `workflow_dispatch`:

1. Checkout.
2. Install Bun, build the frontend (`bun --cwd=app run build`).
3. Install Rust + `cargo-zigbuild` + zig.
4. `cargo zigbuild --release --target armv7-unknown-linux-gnueabihf.2.31`.
5. On `main`, publish or refresh a single prerelease named `main` and upload `target/.../release/somfy` there.
6. On tags, upload the same binary as a stable release asset, plus a `SHA256SUMS` file.

Cache `~/.cargo` and `target/` to keep cold builds tolerable.

That's the full pipeline. CI never touches the Pi. Deployment stays a manual pull on the device over SSH.

## Testing

Keep the testing story modest: validate that the project builds cleanly, that the deployable artifact is reproducible in CI, and that hardware changes can be exercised on the Pi before tagging.

### CI checks

On pull requests and on `main`, run the cheap checks:

1. `cargo fmt --check`
2. `cargo clippy --all-targets`
3. `cargo test`
4. `cargo check --no-default-features --features hw`
5. `bun --cwd=app install --frozen-lockfile`
6. `bun --cwd=app run build`
7. `bun --cwd=app run lint` (oxlint) + `bun --cwd=app run format:check` (oxfmt)

This catches most regressions without pretending GitHub-hosted runners are a Raspberry Pi with a real systemd-managed service.

### Hardware verification

For GPIO-facing changes, verify on the Pi before cutting a stable tag:

1. Merge to `main`.
2. Let CI refresh the `main` branch prerelease.
3. Run `ssh pi sudo somfy upgrade --channel main`.
4. Exercise the hardware through the app / browser / CLI.
5. If it looks good, cut a stable tag.

### Iterating on the CI workflow itself

Three feedback loops, shortest first:

1. `**act**` (nektos/act) — runs the workflow locally in Docker. `act push` or `act -j build` reproduces most of GitHub Actions on your laptop in seconds. Catches YAML errors, step failures, shell bugs without pushing. The armv7 cross-compile is slow under `act` but validates the pipeline shape.
2. `**workflow_dispatch` trigger** — add `on: workflow_dispatch:` alongside the main/tag triggers, then `gh workflow run release.yml --ref my-branch`. Iterate on a branch without cutting real tags.
3. `**gh run watch`** — streams logs from the most recent run instead of refreshing the Actions tab.

Practical flow: `act` to iron out syntax and step logic, then `workflow_dispatch` on a branch once for a real armv7 build, then merge to `main`, then tag when the Pi has been verified.

## First-time bootstrap

One line on a fresh Pi:

```
curl -fsSL https://raw.githubusercontent.com/<owner>/server-remote-gpio/main/install.sh | sudo bash
```

The hosted `install.sh` is intentionally narrow: it supports the single target this project ships for (`armv7-unknown-linux-gnueabihf.2.31`), downloads the latest stable release asset to `/usr/local/bin/somfy`, `chmod +x`, and runs `sudo somfy install` (which writes the unit for the invoking SSH user by default, daemon-reloads, enables, and starts). If the machine is not that target, fail fast with a clear error.

The script itself is fetched from `main` as a bootstrap convenience, but the binary it installs should come from the latest stable release by default.

Same escape hatch as Brew/Bun/rustup if you'd rather inspect first:

```
curl -fsSL https://raw.githubusercontent.com/<owner>/server-remote-gpio/main/install.sh -o install.sh
less install.sh
sudo bash install.sh
```

## Iteration

Day-to-day work happens locally — `bun run dev` for the frontend, `cargo run` natively for the backend. The app stays reachable through Cloudflare / SSH as usual, but deploys are intentionally separate from the web interface. The Pi normally runs stable tagged releases, and can opt into the moving `main` branch build when you want to test a change on hardware before cutting a tag.

Recommended flow:

1. Merge to `main`. CI builds and refreshes the moving `main` branch prerelease.
2. Test it on hardware with `ssh pi sudo somfy upgrade --channel main`.
3. When satisfied, cut a tag. CI publishes a stable release.
4. Update the Pi to the stable release with `ssh pi sudo somfy upgrade`.

If you do not need the prerelease step for a given change, skip straight from merge to tag.

For convenience on macOS, define `pi` in `~/.ssh/config` and use `ssh pi ...` rather than typing the IP each time.

## SSH alias on macOS

Use an SSH config entry so `ssh pi` works from Terminal:

```sshconfig
Host pi
    HostName <pi-host>
    User <pi-user>
```

Put that in `~/.ssh/config`, then connect with:

```bash
ssh pi
ssh pi sudo somfy upgrade
ssh pi sudo somfy upgrade --channel main
```

This is standard OpenSSH, so it works on macOS without any extra tooling.

## Why this design

- **No CI deploy machine**: CI builds artifacts, but never needs SSH access to the Pi.
- **Single artifact**: frontend embedded; binary and frontend versions can never disagree.
- **Binary owns its lifecycle**: unit, install, upgrade, version, diagnostic. Deployment logic lives in `somfy`, not in a shell script on the Mac.
- **System service, not user service**: starts on boot, no linger dance, and defaults to the invoking SSH user instead of adding more service-user setup.
- **Simple restart-based upgrades with rollback**: a few seconds of downtime is acceptable, so the upgrade path stays boring and easy to reason about.
- **CI is the release build path**: no "works on my machine" cross-compile divergence for deployable artifacts.
- **Manual operator approval for deploys**: no privileged web endpoint, no CI tunnel into the Pi, no accidental background updates.
- **Trivial pinning and rollback**: `sudo somfy upgrade --version v0.1.4`.
