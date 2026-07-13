# valv-cli

`valv-cli` builds the `valv` command-line tool. Daemon operations (mount, unmount, status, pause/resume, sync, versions, restore) go through the local `valvd` control API; `login` and sharing commands (`share`, `unshare`) call the Core backend directly using the configured device token.

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
- A registered device: either `valv login` (browser flow) or a manually written `~/.config/valv/config.toml`.
- A running `valvd` daemon for `mount`, `unmount`, `status`, `pause`, `resume`, `sync`, `versions`, and `restore`. Any daemon-bound command starts it automatically; there is no separate install step.

During development, start the daemon in the foreground from `crates/`:

```bash
./target/debug/valvd run
```

The CLI talks to the daemon over the Unix socket at `~/.local/share/valv/valvd.sock`.

## Configuration

Preferred: browser-based login writes the config file for you.

```bash
valv login
```

By default this opens `https://valvsync.com/login` and stores the resulting `backend_url` and `device_token` against the hosted Core backend at `https://api.valvsync.com`. Pass `--web-base-url`, `--backend-url`, or `--device-name` to point at a self-hosted deployment or set a custom device name, and `--no-open` to print the login URL instead of opening a browser.

There is also a credential-less path for machines that only ever hold a folder-scoped access key: `valv mount <path> --key <token>` writes a bare `config.toml` (no `device_token`) itself and redeems the token directly, with no `login` step at all.

Manual, for self-hosting and development: create `~/.config/valv/config.toml` after registering a device with the Core backend:

```toml
backend_url = "http://localhost:4747"
device_token = "replace-with-device-token"
```

The daemon's config may include additional fields such as `device_id`, `device_name`, and `[[mounts]]`; see [`../README.md`](../README.md) for the full shared config example.

## Commands

```bash
valv login
valv login --web-base-url <url> --backend-url <url> --device-name <name>
valv login --no-open
```

Signs in this device through the browser and writes `config.toml`, or prints the login URL with `--no-open`. Ensures the daemon itself first, same as every other daemon-bound command.

```bash
valv status
valv status --json
```

The one command guaranteed to work with the daemon down. Prints the signed-in principal, daemon connectivity/pause state, and an aligned table of every folder this principal can reach: `folder_id`, `name`, `access`, local `path` (or `not mounted`), and `sync_state`. `--json` returns a JSON object instead.

```bash
valv mount <path> --folder <folder-id-or-name>
valv mount <path> --key <token>
valv mount <path> --new
```

Attaches a folder you can already reach by id or name, redeems an access key and attaches the folder it grants, or creates a new folder from `<path>`'s contents. Exactly one of `--folder`, `--key`, `--new` is required.

```bash
valv unmount <path>
valv unmount <path> --yes
```

Unmounts locally only: does not delete the shared folder, its grants, or the locally materialized files. `--yes` skips the confirmation prompt.

```bash
valv pause
valv resume
valv sync
valv sync <path>
```

Pauses all sync work, resumes sync work, or asks the daemon to run sync passes until nothing is pending. `sync` is a barrier: it blocks and retries until a clean pass, or a bounded timeout (exit `75`). Omit `<path>` to settle every mounted folder, or pass one to settle just the folder that covers it.

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
valv share <path>
```

Lists everyone and everything that can reach the folder covering `<path>`: people, access keys, and pending invites, each with an id, scope, and permission.

```bash
valv share <path> --to <email> [--read-only]
valv share <path> --key <name> [--read-only]
```

Invites a person by email, or mints a named access key for a machine, to the folder covering `<path>`. Read/write is the default; `--read-only` restricts it. The access key's one-time token is printed once and cannot be retrieved again.

```bash
valv unshare <path> --to <email> [--yes]
valv unshare <path> --key <name> [--yes]
valv unshare <path> --id <id> [--yes]
```

Revokes a person's, machine's, or pending invite's access to the folder covering `<path>`. `--id` (printed by `valv share <path>`) is required under `--json`, since a handle like `--to`/`--key` is a query, not a pinned reference.

```bash
valv daemon restart
valv daemon uninstall
```

Delegates to the sibling `valvd` binary (or `/usr/local/bin/valvd` if none is found next to `valv`): `restart` stops, reinstalls, and verifies the daemon service is serving; `uninstall` stops and removes it. There is no `valv daemon install`: any daemon-bound command installs and starts the daemon itself the first time it runs. In development, prefer `./target/debug/valvd run` so logs stay in the foreground.

```bash
valv update
```

Downloads, checksum-verifies, and installs the latest released `valv` (and `valvd`, if a `valvd` sibling binary is present) next to the current binaries, restarting the daemon if it was updated. There is no `--check`: it always applies an available update. On macOS, if `valv` runs from the Valv app's managed install location, or the registered daemon LaunchAgent points there, `update` prints a notice and leaves app-managed binaries untouched instead.

`--json` is a **global** flag honored by every command, and may precede or follow the subcommand (`valv --json status` or `valv status --json`).

## Troubleshooting

- `daemon_not_running` (exit `75`): rare, since `ensure_daemon()` normally installs and starts the daemon before any command runs. Retry, or run `valv daemon restart`.
- `not_configured`: no `~/.config/valv/config.toml` was found. Run `valv login`, or `valv mount <path> --key <token>` if you have an access key.
- `no_credential`: `config.toml` exists but has no usable `device_token` (and, for bare `share <path>`, the current mount has no token either). Run `valv login`.
- `mount_source_required`: pass exactly one of `--folder <id|name>`, `--key <token>`, or `--new` to `valv mount`.
- `path_not_mounted`: use a path under a folder that `valvd` has mounted.
- `path_not_in_mirror`: run `valv sync` so the daemon populates the mirror, then retry.
- `backend_unreachable` (exit `75`): the backend couldn't be reached (connection refused, DNS failure, timeout). Retry with backoff.
- `valvd is managed by the Valv app — update the app instead`: on a machine with the Valv macOS app installed, `valv update` will not replace the app-managed daemon; update through the app.

See [`../../skills/valv-cli-usage/SKILL.md`](../../skills/valv-cli-usage/SKILL.md) for the full error-code reference.
