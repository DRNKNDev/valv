---
name: valv-cli-usage
description: Operate the valv sync CLI: install, login, mount/unmount, sync, status, pause/resume, versions/restore, share/unshare, daemon control, and update. Use when running valv commands, driving the valvd daemon, sharing folders, or debugging valv sync/mount/login errors.
---

# valv CLI usage

Operate the `valv` sync CLI. Prefer `--help` on any command to confirm flags.

## Orientation

Thirteen commands:

    valv login
    valv mount <path> (--folder <id|name> | --key <token> | --new)
    valv unmount <path> [--yes]
    valv sync [<path>]
    valv pause
    valv resume
    valv status
    valv versions <path>
    valv restore <path> <version-id>
    valv share <path> [--to <email> | --key <name>] [--read-only]
    valv unshare <path> (--to <email> | --key <name> | --id <id>) [--yes]
    valv update
    valv daemon restart | uninstall

There is no `valv daemon install` and no separate `auth`/`grant`/`grants` namespace: those verbs were folded into `login`, `share`, and `unshare`. There are no aliases for the old names.

`valv` talks to two backends:

- **Daemon ops** (`mount`, `unmount`, `sync`, `pause`, `resume`, `versions`, `restore`, and `status` when the daemon is up) go to the local `valvd` daemon over a Unix socket.
- **Login and sharing** (`login`, `share`, `unshare`, and `status`'s folder discovery on an account machine) call the Core HTTP backend directly, `login` and the discovery call with no daemon involved at all.
- `daemon restart` / `uninstall` shell out to the sibling `valvd` binary.

Shared config lives at `~/.config/valv/config.toml`. `--json` is a **global** flag honored by every command, and may precede or follow the subcommand (`valv --json status` and `valv status --json` both work).

## Install

Installs `valv` and `valvd` to `~/.local/bin`:

    curl -fsSL https://valvsync.com/install | bash

- Relocate: `VALV_INSTALL_DIR=/path` before the command.
- Pin a version: `VALV_VERSION=0.2.0 curl -fsSL https://valvsync.com/install | bash`.
- Verify: `valv --version`, and confirm `~/.local/bin` is on `PATH`.
- Already installed: use `valv update` instead of reinstalling.

Installing no longer starts anything. There is no `valv daemon install` step to run afterward: the first daemon-bound command you run starts the daemon itself.

## Login (first run, account machines only)

    valv login

Opens a browser device flow (5 minute timeout) and writes `~/.config/valv/config.toml` at mode `0600` with `backend_url`, `device_id`, `device_token`, and `device_name`. Flags: `--no-open` (print the URL instead of opening a browser), `--web-base-url`, `--backend-url`, `--device-name`. Defaults come from `VALV_WEB_BASE_URL` (else `https://valvsync.com`) and `VALV_BACKEND_URL` (else `https://api.valvsync.com`).

`login` ensures the daemon itself first, same as every other daemon-bound command (see below).

## The daemon starts itself

There is no user-facing `valv daemon install`. Every daemon-bound command (`login`, `mount`, `unmount`, `sync`, `pause`, `resume`, `versions`, `restore`) calls `ensure_daemon()` before doing its own work:

1. If `~/.config/valv/config.toml` is missing, it writes a credential-less one (`backend_url` and `device_name` only, no `device_id`/`device_token`), mode `0600`.
2. If the daemon's Unix socket already serves, it proceeds silently.
3. Otherwise it installs and starts the daemon (`valvd daemon install` under the hood: a systemd user service on Linux, a launchd agent on macOS) and polls the socket until it serves, printing `Started the Valv daemon.` to stderr only when it actually started one.

If the daemon never starts serving within the timeout, the command fails with `daemon_failed_to_start` (exit `1`), naming the daemon's last log output and the platform's log command. `valv status` is the one exception: it diagnoses the daemon's absence rather than repairing it, so it never installs or starts anything, and works whether or not a daemon is running.

    valv daemon restart      # stop, start, and verify the daemon is serving
    valv daemon uninstall    # stop and remove the daemon service for this user

`daemon install` is gone; there is nothing left to run it for; a failing daemon is repaired by running any command, or explicitly with `valv daemon restart`.

## Discovery: status

    valv status

The one command guaranteed to work with the daemon down, and the way you find a folder id to mount. It always prints:

- The signed-in principal (an email for an account machine, the scoped folder list for an access-key machine, or a message naming why neither applies: rejected, pending, or not configured).
- The daemon's connection state (`Connected`, `Disconnected`, or `Paused`) when the daemon is up.
- One row per folder this principal can reach: `folder_id`, `name`, `access`, local `path` (or `not mounted`), and `sync_state`.

On an **account machine**, the reachable-folder list comes from `GET /grants`, fetched by the CLI directly with a short timeout, not through the daemon: that is exactly what lets discovery work when the daemon is unreachable. If the backend can't be reached, `status` still prints the principal and the daemon's state and mounts, plus one line saying the folder list is unavailable, and still exits `0`.

On an **access-key machine**, `status` makes no backend call at all: its reach is its mounts, so the table only ever shows what is already mounted.

There is no separate `valv folders` command; the reachable-folder list is a column of `status`.

With the daemon down, `status` reads the local sync database and `config.toml`: it prints this machine's own mounts (folder id, name, path) and a state (`not_configured`, `not_installed`, or `installed_but_failing` with the last log lines), and still exits `0`. If `config.toml` holds a `device_token`, it also calls `GET /grants` directly (same as the online path) to list reachable-but-unmounted folders; an access-key-only machine (no `device_token`) makes no call and shows mounts only.

## Everyday ops

    valv mount <path> (--folder <id|name> | --key <token> | --new)
    valv unmount <path> [--yes]
    valv sync [<path>]
    valv pause
    valv resume

`mount`'s source is **required**: exactly one of `--folder <id|name>` (attach a folder you can already reach, found via `valv status`), `--key <token>` (redeem an access key), or `--new` (create a folder from `<path>`'s contents). There is no bare `valv mount <path>` that silently creates a folder anymore, and no `--read-only` on `mount`: a mount's permission is a property of the grant, not the local attachment. Both `--folder` and `--key` *attach*; only `--new` creates, and the command's own message says which happened.

`unmount <path>` takes a local path, not `--folder <id>`. It unmounts locally only: it does not delete the shared folder, its grants, or the local files. `--yes` skips the confirmation prompt; a non-interactive session without `--yes` refuses (exit `1`) rather than prompting into a pipe.

`sync [<path>]` is a **barrier**, not a nudge: it repeats a push+pull round trip and blocks until a round leaves nothing pending and reports no errors, only then exiting `0`. Omit `<path>` to settle every mounted folder; pass one to settle just the folder that covers it. A pass that reports an error is not settled: `sync` keeps retrying until a clean pass or the bound (120s), at which point it exits `75` (`sync_timed_out`, retryable) rather than exiting `0` on unpushed changes. This is the CLI's one synchronous primitive; script against it instead of polling `status --json` for `pending_ops == 0`.

A mount can enter an error state while `sync` waits, and the exit code depends on *why*, not just on the fact that an error is present: if the backend is unreachable (`DaemonStatus.backend_connected: false`), the mount's error is a symptom of the outage, not a verdict on the mount, so `sync` does **not** fail fast on it - it keeps waiting and, on reaching the bound, exits `75` (`sync_timed_out`), same as `share`/`unshare` report a backend outage. If the backend is reachable and a mount is still erroring (a forbidden push, a durable materialize failure), that is a genuine fault: `sync` exits `1` (`sync_mount_error`) immediately instead of waiting out the bound.

`pause` / `resume` stop and restart background filesystem watching and sync work for this device.

## Headless machines (access keys)

    valv mount ~/data --key <token>

This is the entire headless flow. No `valv login`, no hand-edited `config.toml`, no separate daemon-install step: `mount --key` ensures the daemon itself (writing a credential-less config if one doesn't exist), redeems the token, and starts syncing.

This makes two kinds of machine:

- An **account machine** holds a `device_token` belonging to a signed-in user, written only by `valv login`.
- An **access-key machine** holds no account at all, only a folder-scoped, ownerless, revocable credential redeemed by `mount --key <token>` (minted by `valv share <path> --key <name>`, or "Add Access Key..." in the macOS app). The token is stored on that mount only, never promoted into `device_token`, so a second mount on the same box can't silently inherit the wrong scope.

`mount --key` is never refused by the access-key restrictions below: it is checked before every other source, and is the one path that supplies a credential to a machine that may hold none at all.

What an access-key machine cannot do, each a stable `77`-exit refusal:

- Create a folder (`mount --new`) or attach one by id or name (`mount --folder <id|name>`): `access_key_cannot_create_folder`, `access_key_cannot_mount_folder`. The CLI checks this itself, from a quick local probe of the daemon's own status, before it ever sends the request.
- Mint another access key or send an invite: `access_key_cannot_issue_keys`, `access_key_cannot_invite_people` (`share --key` / `share --to`). Also checked locally first, the same way.
- Revoke anything, including its own grant: `access_key_cannot_revoke` (`unshare`). Checked locally first too.
- Restore a version on a mount it holds read-only: `access_key_is_read_only`. Checked from the mount's own local permission first (an account machine's own read-only mount is never wrongly blocked); only when that's read-only does it also confirm the machine is an access key before refusing.
- List who else a folder is shared with: `access_key_cannot_list_grants`, from bare `share <path>`. This one is **not** checked locally: it only surfaces after the backend itself refuses `GET /folders/:id/grants`, and the CLI then falls back to showing just this machine's own access via the self-scoped `GET /grants`. That fallback authenticates with `config.toml`'s `device_token` when present, and otherwise with the current mount's own token, so bare `share <path>` works from a true access-key-only machine with no `device_token` at all, the same as every other command.
- Reuse a key name another grant already has: `access_key_name_taken` (exit `1`, not `77`: a naming conflict, not a permissions refusal).

`login`, `mount --key`, `unmount`, `status`, `pause`, `resume`, `sync`, `versions`, `restore`, `share <path>`/`share --to`/`share --key`, and `unshare` all work from a true access-key-only machine with no `device_token` at all (an access-key machine can still run `login` to become an account machine, same as any other).

## Versions and restore

    valv versions <path>
    valv restore <path> <version-id>

Paths are canonicalized before the call. `restore` reports one result: `applied`, `conflict_copy` (a copy was written to avoid clobbering local edits), or `superseded` (a newer version won).

## Sharing: share / unshare

    valv share <path>
    valv share <path> --to <email> [--read-only]
    valv share <path> --key <name> [--read-only]
    valv unshare <path> (--to <email> | --key <name> | --id <id>) [--yes]

One vocabulary: you **share** a folder with a **person** (`--to`, an email invite) or a **machine** (`--key`, an issued access key). Read/write is the default; `--read-only` on either restricts it. Bare `share <path>` lists everyone and everything that can reach the folder: people, keys, and pending invites, each with an id, scope, and permission, ending with a hint naming `--to` and `--key`.

`unshare` takes the folder's **path** plus a **handle**, not a bare id: `valv unshare ~/Design --to bob@example.com`. The path supplies the folder id that `DELETE /folders/:id/grants/:grantId` needs; a bare grant id does not carry it. `<path>` is a folder-locator, not a scope-locator: it considers every grant anywhere in that folder, including one scoped narrower or wider than where you ran the command.

Every selector (`--to`, `--key`) is a human handle; where it matches more than one grant or invite, `unshare` refuses, lists the matches with their ids, names `--id`, and exits `1` (`ambiguous_grant_handle`) rather than guessing. Ids are prefixed by kind so `--id` can route to the right endpoint: `g_` for a grant, `i_` for a pending invite.

`unshare` fixes a real bug: it used to resolve the target by searching the caller's own `GET /grants`, which cannot contain a grant issued to someone else (a collaborator carries their own `user_id`; a machine's grant carries `user_id = NULL`), so it returned "grant not found" for every grant but the caller's own. It now reads the folder's own grant and invite lists instead, so revoking a collaborator's or a machine's access works.

Confirmation echoes the handle, the scope, the permission, and **how long ago it was created**, then the consequence; `--yes` skips it. No TTY and no `--yes` refuses (exit `1`) rather than prompting into a pipe. The age matters: `folder-access`'s Regenerate action reuses a key's *name* across grant rows, so a stale terminal running `unshare --key build-01` could otherwise revoke the fresh key Regenerate just minted under that name instead of the old one.

Success never prints a bare id: `Revoked bob@example.com's access to Design (Entire Folder, read/write).`

Under `--json`, a handle on a destructive call is a usage error (`handle_requires_pinned_id`, exit `2`): `--id` is required instead. An id is a pinned reference; a handle is a query, and a non-interactive caller must not run a destructive command whose target is resolved by a query it cannot observe. An agent already holds the id from `share --json`'s output, so it reads before it writes.

## Update

    valv update

Self-updates `valv` and the sibling `valvd` from GitHub releases, verifying checksums and signatures, then restarts the daemon. There is no `--check`: it always applies an available update. `VALV_VERSION` pins the target version. On macOS it refuses to touch app-managed binaries.

## Output contract

- `--json` is global and honored by every command.
- stdout carries the answer and nothing else; stderr carries progress (spinners, suppressed when stdout is not a TTY or `--json` is set), `Started the Valv daemon.`, update notices, and human-readable error text.
- Every error carries a stable `code`, not just refusals, and under `--json` is exactly one `{"error":{"code","message","hint","scope"}}` object on stderr, never on stdout: `valv status --json | jq` can never parse an error as data.
- Exit codes: `0` ok, `1` failed, `2` usage, `75` retryable (the daemon still starting, `sync` still settling, a 5xx from the backend), `77` refused. `77` means *forbidden*, never *not found*: a typo'd path or a missing grant is `1`, not `77`.

## Error to fix

| Error code | Fix |
| --- | --- |
| `mount_source_required` | Pass exactly one of `--folder <id|name>`, `--key <token>`, or `--new` to `mount`. |
| `daemon_not_running` (exit `75`) | Rare: `ensure_daemon()` normally repairs this before any command runs. Retry, or run `valv daemon restart`. |
| `daemon_failed_to_start` (exit `1`) | The daemon installed but never began serving. Inspect the log command the error names (`journalctl --user -u valvd -n 50` on Linux, `tail -n 50 ~/Library/Logs/Valv/valvd.log` on macOS), fix the underlying issue, then retry. |
| `not_configured` | Run `valv login`, or `valv mount <path> --key <token>` if you have an access key. |
| `no_credential` | `config.toml` has no `device_token` and, for bare `share <path>`, the current mount has no token either. `unshare`/`share --to`/`share --key` normally refuse earlier with an `access_key_*` code on an access-key machine instead (see above), so seeing `no_credential` from them means the daemon's status probe couldn't classify the machine at all. Run `valv login` if you want a full account here; do not run it just because a `mount --key` setup is working fine without one. |
| `sync_timed_out` (exit `75`) | `valv sync` didn't settle inside its bound. This also covers a mount erroring while the backend is unreachable: that is treated as connectivity, not a mount fault. Retry; if it keeps happening with the backend reachable, run `valv status` to see which mount has pending ops piling up. |
| `sync_mount_error` (exit `1`) | A mount entered a persistent error state **while the backend was reachable** (not a transient per-pass error, and not a backend outage - both of those make `sync` retry instead). Run `valv status` for the mount's own error detail. |
| `share_read_only_requires_target` (exit `2`) | `--read-only` needs a target: pass `--to <email>` or `--key <name>`. Bare `share <path>` with no flags is unaffected; it lists instead. |
| `path_not_mounted` | Mount the parent folder first, or pass a path under an existing mount. |
| `path_not_in_mirror` | Run `valv sync` so the daemon populates the mirror, then retry. |
| `folder_not_found` | The `--folder <id|name>` you passed doesn't match anything reachable; check `valv status`. |
| `grant_not_found` | The `--to`/`--key`/`--id` you passed to `unshare` doesn't match a grant or invite on this folder; check `valv share <path>`. |
| `ambiguous_grant_handle` | More than one grant or invite matched; pass `--id <id>` from the listed matches. |
| `handle_requires_pinned_id` | `--json` plus a destructive command (`unshare`) needs `--id`, not a handle. |
| `access_key_cannot_create_folder` (exit `77`) | `mount --new` needs an account; ask the folder owner to create it, or attach a folder they shared with `mount <path> --key <token>`. |
| `access_key_cannot_mount_folder` (exit `77`) | `mount --folder <id|name>` needs an account; ask the folder owner for an access key, then `mount <path> --key <token>`. |
| `access_key_cannot_issue_keys` / `access_key_cannot_invite_people` / `access_key_cannot_revoke` / `access_key_cannot_list_grants` (exit `77`) | This principal is an access key; these actions need an account. Run them from the account that owns the folder. |
| `access_key_is_read_only` (exit `77`) | `restore` needs a read/write key for this folder; ask the folder owner for one, or ask them to restore it. |
| `access_key_name_taken` (exit `1`) | Pick a different `--key <name>`; another grant already uses it. |
| `backend_error` | The backend returned a failure the CLI doesn't have a specific code for. Exit is `75` if the backend's own HTTP status was 5xx (retryable), else `1`. The message carries the backend's own error string. |
| Update refuses on macOS (app-managed) | The Valv app owns the binaries; update through the app, not `valv update`. |

## Paths and env vars

Paths (under `$HOME`):

- Config: `~/.config/valv/config.toml`
- Socket: `~/.local/share/valv/valvd.sock`
- SQLite mirror: `~/.local/share/valv/sync.db`
- Update-check cache: `~/.local/share/valv/update-check.json`

Env vars: `HOME` (required for all paths), `RUST_LOG` (tracing filter, default `INFO`), `VALV_WEB_BASE_URL`, `VALV_BACKEND_URL`, `VALV_VERSION` (pin update target), `VALV_NO_UPDATE_CHECK=1` (suppress the update notice), `VALV_INSTALL_DIR` (install script only).
