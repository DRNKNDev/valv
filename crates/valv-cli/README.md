# valv-cli

`valv-cli` builds the `valv` command-line tool. It is a thin client for the local `valvd` daemon and the Core backend: daemon commands go over the local Unix socket, while grant/invite commands call the backend directly with the configured device token.

## Build

Run from `oss/crates/`:

```bash
cargo build --bin valv
```

The debug binary is written to `./target/debug/valv`.

## Prerequisites

- A running Core backend. See `../../core/README.md`.
- A registered device token in `~/.config/valv/config.toml`.
- A running `valvd` daemon for local mount, status, pause, resume, and sync commands.

During development, start the daemon in the foreground from `oss/crates/`:

```bash
./target/debug/valvd run
```

The CLI talks to the daemon through `~/.local/share/valv/valvd.sock`.

## Config

Create `~/.config/valv/config.toml` after registering a device with the Core backend:

```toml
backend_url = "http://localhost:4747"
device_token = "replace-with-device-token"
```

The daemon config may include additional fields such as `device_id`, `device_name`, and `[[mounts]]`; see `../README.md` for the full shared config example.

## Commands

```bash
valv status
```

Prints daemon connection state and a tab-separated table of mounted folders, sync state, pending operation count, last sync time, and error.

```bash
valv mount <path>
valv mount <path> --folder <folder-id>
valv mount <path> --grant <grant-token>
```

Mounts a new folder at `<path>`, mounts an existing folder by ID, or mounts a folder using a one-time grant token. `--folder` and `--grant` are mutually exclusive.

```bash
valv pause
valv resume
valv sync
valv sync --folder <folder-id>
```

Pauses all sync work, resumes sync work, or asks the daemon to run a sync pass. `valv sync --folder` limits the request to one folder.

```bash
valv grants
valv grants <folder-path>
```

Lists grants for the first mounted folder, or for the mounted folder containing `<folder-path>`. The output is tab-separated: `grant_id`, `scope`, `grantee`, `role`, `can_read`, `can_write`.

```bash
valv grant create <node-path> --to <email>
```

Creates a user invite for the node at `<node-path>` and prints an invite URL. The path must be inside a mounted folder and present in the local mirror.

```bash
valv grant create <node-path> --device <name>
valv grant create <node-path> --device <name> --read-only
valv grant create <node-path> --device <name> --write
```

Creates a device grant and prints the device token, grant ID, and device ID. Store the device token immediately; it cannot be retrieved again. Grants are writable by default unless `--read-only` is provided.

```bash
valv grant revoke <grant-id>
```

Revokes an existing grant by ID.

```bash
valv daemon install
valv daemon uninstall
```

Delegates service installation and removal to the sibling `valvd` binary when available, or `/usr/local/bin/valvd` otherwise. In development, prefer `./target/debug/valvd run` so logs stay in the foreground.

## Troubleshooting

- `Daemon is not running. Start it with: valv daemon install`: no Unix socket was found at `~/.local/share/valv/valvd.sock`. Start `valvd` first.
- `Config not found`: create `~/.config/valv/config.toml` with `backend_url` and `device_token`.
- `path is not inside a mounted folder`: use a path under a folder that `valvd` has mounted.
- `path is not present in the local mirror`: wait for sync to discover the path, or run `valv sync` and retry.
