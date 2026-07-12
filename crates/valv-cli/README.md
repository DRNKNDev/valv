# valv-cli

`valv-cli` builds the `valv` command-line tool. Daemon operations (mount, status, pause/resume, sync, versions, restore) go through the local `valvd` control API; auth and grant commands call the Core backend directly using the configured device token.

## Install

Hosted installer (downloads and verifies the latest GitHub release for your platform):

```bash
curl -fsSL https://valvsync.com/install | bash
```

This runs [`../../scripts/install.sh`](../../scripts/install.sh), which detects your OS/architecture, downloads `valv-<version>-<target>.tar.gz` and `SHA256SUMS` from the matching GitHub release, verifies the checksum, and installs `valv` and `valvd` to `~/.local/bin` (override with `VALV_INSTALL_DIR`). Set `VALV_VERSION` to pin a specific release instead of the latest.

Manual install: download `valv-<version>-<target>.tar.gz` from the [releases page](https://github.com/DRNKNDev/valv/releases), verify it against the release's `SHA256SUMS`/`SHA256SUMS.minisig`, and extract `valv`/`valvd` onto your `PATH`.

Prebuilt releases currently cover macOS arm64 (`aarch64-apple-darwin`) and Linux x86_64 (`x86_64-unknown-linux-gnu`). Other platforms require a source build.

## Build From Source

Run from `crates/`:

```bash
cargo build --bin valv --bin valvd --locked
```

The debug binaries are written to `./target/debug/valv` and `./target/debug/valvd`.

## Prerequisites

- A running Core backend (hosted at `https://api.valvsync.com` by default, or your own self-hosted instance; see [`../../core/README.md`](../../core/README.md)).
- A registered device: either `valv auth login` (browser flow) or a manually written `~/.config/valv/config.toml`.
- A running `valvd` daemon for `mount`, `unmount`, `status`, `pause`, `resume`, `sync`, `versions`, and `restore`.

During development, start the daemon in the foreground from `crates/`:

```bash
./target/debug/valvd run
```

The CLI talks to the daemon over the Unix socket at `~/.local/share/valv/valvd.sock`.

## Configuration

Preferred: browser-based login writes the config file for you.

```bash
valv auth login
```

By default this opens `https://valvsync.com/login` and stores the resulting `backend_url` and `device_token` against the hosted Core backend at `https://api.valvsync.com`. Pass `--web-base-url`, `--backend-url`, or `--device-name` to point at a self-hosted deployment or set a custom device name, and `--no-open` to print the login URL instead of opening a browser.

Manual, for self-hosting and development: create `~/.config/valv/config.toml` after registering a device with the Core backend:

```toml
backend_url = "http://localhost:4747"
device_token = "replace-with-device-token"
```

The daemon's config may include additional fields such as `device_id`, `device_name`, and `[[mounts]]`; see [`../README.md`](../README.md) for the full shared config example.

## Commands

```bash
valv auth login
valv auth login --web-base-url <url> --backend-url <url> --device-name <name>
valv auth login --no-open
```

Signs in this device through the browser and writes `config.toml`, or prints the login URL with `--no-open`.

```bash
valv status
valv status --json
```

Prints daemon connectivity, pause state, and an aligned human-readable table of mounted folders (sync state, pending operation count, last sync time, error), or a JSON `DaemonStatus` object with `--json`.

```bash
valv mount <path>
valv mount <path> --folder <folder-id>
valv mount <path> --grant <grant-token>
```

Mounts a new folder at `<path>`, mounts an existing folder by ID, or mounts a folder using a one-time grant token. `--folder` and `--grant` are mutually exclusive.

```bash
valv unmount --folder <folder-id>
```

Unmounts locally only: does not delete the shared folder, its grants, or the locally materialized files.

```bash
valv pause
valv resume
valv sync
valv sync --folder <folder-id>
```

Pauses all sync work, resumes sync work, or asks the daemon to run a sync pass. `--folder` limits the sync request to one folder.

```bash
valv versions <path>
valv versions <path> --json
```

Lists stored versions for a local file inside a mounted folder as an aligned table, or as JSON with `--json`.

```bash
valv restore <path> <version-id>
```

Restores the file at `<path>` to the version id shown by `valv versions <path>`.

```bash
valv grants
valv grants <folder-path>
valv grants --json
```

Lists grants for the first mounted folder, or for the mounted folder containing `<folder-path>`. The human table shows `grant_id`, `scope`, `grantee`, `role`, `can_read`, `can_write`; `--json` returns the same data as JSON.

```bash
valv grant create <node-path> --to <email>
```

Creates a user invite for the node at `<node-path>` and prints an invite URL. The path must be inside a mounted folder and present in the local mirror.

```bash
valv grant create <node-path> --device <name>
valv grant create <node-path> --device <name> --read-only
valv grant create <node-path> --device <name> --write
```

Creates a device grant and prints the device token, grant ID, and device ID. Store the device token immediately; it cannot be retrieved again. Grants are writable by default unless `--read-only` is passed; `--write` is accepted for symmetry but is already the default.

```bash
valv grant revoke <grant-id>
```

Revokes an existing grant by ID.

```bash
valv daemon install
valv daemon uninstall
```

Delegates service installation and removal to the sibling `valvd` binary (or `/usr/local/bin/valvd` if none is found next to `valv`): a launchd LaunchAgent on macOS, a systemd user service on Linux. In development, prefer `./target/debug/valvd run` so logs stay in the foreground.

```bash
valv update
valv update --check
```

Downloads, checksum-verifies, and installs the latest released `valv` (and `valvd`, if a `valvd` sibling binary is present) next to the current binaries, restarting the daemon if it was updated. `--check` reports whether a newer version is available without installing anything. On macOS, if `valv` runs from the Valv app's managed install location, or the registered daemon LaunchAgent points there, `update` prints a notice and leaves app-managed binaries untouched instead.

Add the global `--json` flag before the subcommand (`valv --json status`) on `status`, `versions`, and `grants` to get machine-readable output instead of the aligned human table; other commands do not change their output based on `--json`.

## Troubleshooting

- `Daemon is not running. Start it with: valv daemon install`: no Unix socket was found at `~/.local/share/valv/valvd.sock`. Start `valvd`, or install it as a service with `valv daemon install`.
- `Not signed in. Run: valv auth login`: no `~/.config/valv/config.toml` was found. Run `valv auth login`, or create the file with `backend_url` and `device_token` set.
- `Missing backend_url in config.toml` / `Missing device_token in config.toml`: `config.toml` exists but is missing one of the required fields. This is what you get if `valv daemon install` wrote the config template (which leaves `device_token` empty) and you have not signed in yet. Run `valv auth login`.
- `--folder and --grant are mutually exclusive`: pass only one of `--folder` or `--grant` to `valv mount`.
- `path is not inside a mounted folder`: use a path under a folder that `valvd` has mounted.
- `path is not present in the local mirror`: wait for sync to discover the path, or run `valv sync` and retry.
- `valvd is managed by the Valv app — update the app instead`: on a machine with the Valv macOS app installed, `valv update` will not replace the app-managed daemon; update through the app.
