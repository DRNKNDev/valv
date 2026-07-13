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

`--folder` and `--grant` on `mount` are mutually exclusive: omit both to create a new folder, `--folder` to mount an existing one, `--grant` to mount a shared folder using an access key (from `grant create --device`, or "Add Access Key..." in the macOS app). An invite token does not work here: accepting an invite requires signing in as a user in a browser, not a device credential. `unmount --folder` is required.

## Headless machines (access keys)

    valv daemon install
    valv mount <path> --grant <token>

No `valv auth login`, no hand-edited `config.toml`. `daemon install` writes only `backend_url`, `device_name`, and `mounts = []`; it no longer writes empty `device_id`/`device_token` placeholders, and `valvd` no longer requires either to start. It starts credential-less, idles, and begins syncing the moment `mount --grant` hands it a token, no restart needed.

This makes two kinds of machine:

- An **account machine** holds a `device_token` belonging to a signed-in user (written only by `valv auth login`).
- An **access-key machine** holds no account at all, only a folder-scoped, ownerless, revocable credential redeemed by `mount --grant <token>` (minted by `grant create --device`, or "Add Access Key..." in the macOS app). The token is stored on that mount only, never promoted into `device_token`, so a second mount on the same box can't silently inherit the wrong scope.

`mount --grant` is the one command that needs no existing config or credential: the token on the command line is the credential.

What an access-key machine cannot do, enforced server-side with a stable 403 code:

- Mint another access key or send an invite: `access_key_cannot_issue_keys`, `access_key_cannot_invite_people`.
- Revoke anything, including its own grant: `access_key_cannot_revoke`.
- List who else a folder is shared with: `access_key_cannot_list_grants`. It sees only its own access, via `GET /grants`.

The `valv` CLI does not yet turn these into a clean local refusal; `grant create`, `grants`, and `grant revoke` still hard-require a `device_token` (see the error table below), so none of them are runnable from an access-key-only machine at all today. The codes above matter when something calls the backend directly with the access key as the bearer token.

## Versions and restore

    valv versions <path>
    valv restore <path> <version_id>

Paths are canonicalized before the call. `restore` reports one result: `applied`, `conflict_copy` (a copy was written to avoid clobbering local edits), or `superseded` (a newer version won).

## Grants (sharing)

    valv grants [folder_path]
    valv grant create <node_path> (--to <email> | --device <name>) [--write | --read-only]
    valv grant revoke <grant_id>

`grant create` requires exactly one of `--to` (email invite, prints an invite URL) or `--device` (prints a one-time device token, grant id, and device id: capture it, it is not shown again). Write access is the default; pass `--read-only` for read-only.

### Sharing management is incomplete on the CLI

The backend can now show and manage everything shared from a folder: who has access (invited users and access keys, not just the caller's own), pending invites, and key rotation. `valv grants` has not caught up. It still reads the self-scoped `GET /grants` and filters client-side by folder, so it only ever shows the calling principal's own access, never a full "who has this folder" list, and it has no way to see or cancel a pending invite. `grant create` and `grant revoke` are unchanged too. Do not tell a user `valv grants` can show them their collaborators; it cannot yet. Pointing the CLI at the folder-scoped endpoints is a later change, not this one.

The macOS app is ahead of the CLI here: its "Manage Folders & Sharing" window reads the folder-scoped grants list, so its "Shared With" table can show invited collaborators and access keys with working Revoke, plus a Regenerate action per key and pending invites with Cancel. Its device-provisioning button is "Add Access Key..." (renamed from "Add Device...").

## Update

    valv update
    valv update --check

Self-updates `valv` and the sibling `valvd` from GitHub releases, verifying checksums and signatures, then restarts the daemon. `--check` only reports. `VALV_VERSION` pins the target version. On macOS it refuses to touch app-managed binaries.

## Error to fix

`Not signed in` and `Missing device_token` only come from `grant create`, `grants`, and `grant revoke`, the three commands that still call the backend directly with an account's `device_token`. `mount`, `unmount`, `status`, `pause`, `resume`, `sync`, `versions`, `restore`, and `daemon` all go through the daemon socket instead and need no `device_token` at all, so a machine with none (an access-key-only setup, see "Headless machines" above) never hits either error for those commands.

| Error | Fix |
| --- | --- |
| `Daemon is not running` / `DAEMON_NOT_RUNNING` | `valv daemon install`, then retry. |
| `Not signed in. Run: valv auth login` | Only from `grant create`/`grants`/`grant revoke`. `valv auth login`, the only command that populates `device_token`. Not the fix for a headless access-key machine, which has no `device_token` by design and does not run these commands. |
| `Missing device_token in config.toml` | Same three commands only. Means `config.toml` exists (often written by `valv daemon install`) but has no `device_token`, which `daemon install` no longer writes even as a placeholder. `valv auth login` fixes it if you actually want a full account on this machine; do not run it just because a mount is working fine off an access key. |
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
