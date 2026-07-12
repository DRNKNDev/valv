---
name: valv-cli-usage
description: Operate the valv sync CLI: install, auth login, mount/unmount, sync, status, pause/resume, versions/restore, grants, daemon control, and update. Use when running valv commands, driving the valvd daemon, sharing folders via grants, or debugging valv sync/mount/auth errors.
---

# valv CLI usage

Operate the `valv` sync CLI. Prefer `--help` on any command to confirm flags.

## Orientation

`valv` talks to two backends:

- **Daemon ops** (`mount`, `unmount`, `status`, `pause`, `resume`, `sync`, `versions`, `restore`) go to the local `valvd` daemon over a Unix socket.
- **Auth and sharing** (`auth login`, `grant`, `grants`) call the Core HTTP backend directly with the device token.
- `daemon install` / `uninstall` shells out to the sibling `valvd` binary.

Shared config lives at `~/.config/valv/config.toml`. The global `--json` flag is honored only by `status`, `versions`, and `grants`, and may precede the subcommand (`valv --json status`).

## Install

Installs `valv` and `valvd` to `~/.local/bin`:

    curl -fsSL https://valvsync.com/install | bash

- Relocate: `VALV_INSTALL_DIR=/path` before the command.
- Pin a version: `VALV_VERSION=0.2.0 curl -fsSL https://valvsync.com/install | bash`.
- Verify: `valv --version`, and confirm `~/.local/bin` is on `PATH`.
- Already installed: use `valv update` instead of reinstalling.

## Auth (first run)

    valv auth login

Opens a browser device flow (5 minute timeout) and writes `~/.config/valv/config.toml` at mode `0600`. Flags: `--no-open` (print URL instead), `--web-base-url`, `--backend-url`, `--device-name`. Defaults come from `VALV_WEB_BASE_URL` (else `https://valvsync.com`) and `VALV_BACKEND_URL` (else `https://api.valvsync.com`).

## Daemon lifecycle

    valv daemon install
    valv daemon uninstall

Manages a launchd agent (`dev.drnkn.valvd`) on macOS or a systemd user service (`valvd`) on Linux. The daemon serves `~/.local/share/valv/valvd.sock`; if the socket is missing, daemon commands fail with `DAEMON_NOT_RUNNING`.

## Everyday ops

    valv mount <path> [--folder <id> | --grant <token>]
    valv unmount --folder <id>
    valv sync [--folder <id>]
    valv status
    valv pause
    valv resume

`--folder` and `--grant` on `mount` are mutually exclusive: omit both to create a new folder, `--folder` to mount an existing one, `--grant` to mount a shared folder from an invite token. `unmount --folder` is required.

## Versions and restore

    valv versions <path>
    valv restore <path> <version_id>

Paths are canonicalized before the call. `restore` reports one result: `applied`, `conflict_copy` (a copy was written to avoid clobbering local edits), or `superseded` (a newer version won).

## Grants (sharing)

    valv grants [folder_path]
    valv grant create <node_path> (--to <email> | --device <name>) [--write | --read-only]
    valv grant revoke <grant_id>

`grant create` requires exactly one of `--to` (email invite, prints an invite URL) or `--device` (prints a one-time device token, grant id, and device id: capture it, it is not shown again). Write access is the default; pass `--read-only` for read-only.

## Update

    valv update
    valv update --check

Self-updates `valv` and the sibling `valvd` from GitHub releases, verifying checksums and signatures, then restarts the daemon. `--check` only reports. `VALV_VERSION` pins the target version. On macOS it refuses to touch app-managed binaries.

## Error to fix

| Error | Fix |
| --- | --- |
| `Daemon is not running` / `DAEMON_NOT_RUNNING` | `valv daemon install`, then retry. |
| `Not signed in. Run: valv auth login` | `valv auth login`. Only `auth login` populates `device_token`. |
| `Missing device_token in config.toml` | `valv auth login`. Means a config template exists (usually written by `valv daemon install`) but no token. `daemon install` does not sign you in. |
| `--folder and --grant are mutually exclusive` | Pass only one, or neither to create a new folder. |
| `path is not inside a mounted folder` | Mount the parent folder first, or pass a path under an existing mount. |
| `path is not present in the local mirror` | Run `valv sync` so the daemon populates the mirror, then retry. |
| Update refuses on macOS (app-managed) | The Valv app owns the binaries; update through the app, not `valv update`. |

## Paths and env vars

Paths (under `$HOME`):

- Config: `~/.config/valv/config.toml`
- Socket: `~/.local/share/valv/valvd.sock`
- SQLite mirror: `~/.local/share/valv/sync.db`
- Update-check cache: `~/.local/share/valv/update-check.json`

Env vars: `HOME` (required for all paths), `RUST_LOG` (tracing filter, default `INFO`), `VALV_WEB_BASE_URL`, `VALV_BACKEND_URL`, `VALV_VERSION` (pin update target), `VALV_NO_UPDATE_CHECK=1` (suppress update notice), `VALV_INSTALL_DIR` (install script only).
