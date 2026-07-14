---
name: valv-cli-usage
description: "Operate the valv file-sync CLI: sign in, mount and unmount folders, sync, check status, share folders with people or machines, revoke access, and restore file versions. Use when running valv commands, setting up a headless or agent machine with an access key, driving the valvd daemon, or interpreting valv exit codes and error codes."
---

# valv CLI

Sync folders across machines. Run `valv <command> --help` to confirm flags.

## Contract

Read this first if you are scripting or running non-interactively.

`--json` is global and honored by every command. stdout carries the answer and nothing else; progress and errors go to stderr. Under `--json` an error is exactly one `{"error":{"code","message","hint","scope"}}` object on stderr, so `valv status --json | jq` can never parse an error as data.

Every error carries a stable `code`. Branch on the exit status:

| Exit | Meaning | Do |
| --- | --- | --- |
| `0` | ok | continue |
| `1` | failed | read `code`, fix, retry |
| `2` | usage error | fix the command |
| `75` | temporary | retry with backoff |
| `77` | refused | **do not retry and do not work around it**; this machine may not do this |

`77` means forbidden, never not-found. A mistyped path is `1`.

Commands that destroy access (`unshare`) or a credential (`unmount`) confirm first. `--yes` skips the prompt. With no TTY and no `--yes` they refuse (`confirmation_required`) rather than prompting into a pipe.

## Two kinds of machine

- **Account machine**: signed in with `valv login`. Reaches every folder its user owns.
- **Access-key machine**: holds one folder-scoped, revocable credential, redeemed with `valv mount <path> --key <token>`. No account and no sign-in. This is what a server or an agent gets.

`valv status`'s first line always says which one you are on. Every `access_key_*` refusal below follows from being the second kind.

## Commands

    valv login
    valv status
    valv mount <path> (--folder <id|name> | --key <token> | --new)
    valv unmount <path> [--yes]
    valv sync [<path>]
    valv pause | valv resume
    valv share <path> [--to <email> | --key <name>] [--read-only]
    valv unshare <path> (--to <email> | --key <name> | --id <id>) [--yes]
    valv versions <path>
    valv restore <path> <version-id>
    valv update
    valv daemon restart | uninstall

The daemon starts itself: any command that needs it installs, starts, and verifies it first. `valv status` is the exception, since it diagnoses rather than repairs, and works whether or not the daemon is running.

## Install

    curl -fsSL https://valvsync.com/install | bash

Puts `valv` and `valvd` in `~/.local/bin`. Confirm that is on `PATH`. If already installed, use `valv update`.

## Headless machine (a server or an agent)

One command. No sign-in, no config file to edit:

    valv mount ~/data --key <token>

The key comes from the folder's owner (`valv share <path> --key <name>`, or "Add Access Key..." in the macOS app) and is shown once.

Such a machine can `status`, `sync`, `pause`, `resume`, `versions`, `unmount`, and `restore` when its key is read/write. It **cannot** create folders, attach folders by id, issue keys, invite people, revoke anything, or list a folder's other collaborators. Each refusal exits `77` with its own code.

`valv login` still works here, and turns it into an account machine.

## Find a folder, then mount it

    valv status                            # every folder this machine can reach
    valv mount ~/Design --folder Design    # attach one

`mount` requires a source. `--folder <id|name>` attaches a folder you can already reach, `--key <token>` redeems an access key, `--new` creates a folder from the directory's contents. There is no default.

`valv status` also works when the daemon is down, and reports whether it is not installed, failing, or not configured.

## Sync is a barrier

    valv sync [<path>]

Blocks until the folder is settled, then exits `0`. Script against this instead of polling `status --json` for `pending_ops == 0`.

Exits `75` if it does not settle within its bound, which includes the backend being unreachable: retry. Exits `1` only when a mount is genuinely faulted while the backend is reachable.

## Share and revoke

    valv share <path>                                # who can reach this folder
    valv share <path> --to <email> [--read-only]     # invite a person
    valv share <path> --key <name> [--read-only]     # issue a key for a machine
    valv unshare <path> (--to <email> | --key <name> | --id <id>) [--yes]

Bare `share <path>` lists people, access keys, and pending invites, each with an id: `g_` for a grant, `i_` for a pending invite.

`unshare` takes the folder's path plus a handle. If a handle matches more than one grant, it refuses and lists the matches (`ambiguous_grant_handle`); pass `--id` to choose. Under `--json` a destructive command requires `--id` rather than a handle (`handle_requires_pinned_id`), so read with `share --json` before you write.

## Versions and restore

    valv versions <path>
    valv restore <path> <version-id>

`restore` reports `applied`, `conflict_copy` (a copy was written rather than clobbering local edits), or `superseded` (a newer version won).

## Error codes

| Code | Fix |
| --- | --- |
| `mount_source_required` | Pass one of `--folder`, `--key`, or `--new`. |
| `not_configured` / `no_credential` | `valv login`, or `valv mount <path> --key <token>` if you were given a key. |
| `daemon_failed_to_start` | The daemon installed but never served. Read the log command named in the error, then retry. |
| `daemon_not_running` (75) | Retry, or `valv daemon restart`. |
| `sync_timed_out` (75) | Not settled in time, or the backend is unreachable. Retry. |
| `sync_mount_error` (1) | A mount is faulted while the backend is reachable. Run `valv status` for its error. |
| `backend_unreachable` (75) | Network or backend outage. Retry with backoff. |
| `path_not_mounted` | The path is not inside a mounted folder. |
| `path_not_in_mirror` | Run `valv sync`, then retry. |
| `folder_not_found` | No reachable folder matches. Check `valv status`. |
| `grant_not_found` | No grant or invite matches. Check `valv share <path>`. |
| `ambiguous_grant_handle` | Pass `--id <id>` from the listed matches. |
| `handle_requires_pinned_id` (2) | Under `--json`, `unshare` needs `--id`. |
| `share_read_only_requires_target` (2) | `--read-only` needs `--to` or `--key`. |
| `confirmation_required` | Pass `--yes` when there is no TTY. |
| `access_key_name_taken` (1) | That key name is already used on this folder. Pick another. |
| `access_key_cannot_create_folder` (77) | Needs an account. Ask the folder's owner. |
| `access_key_cannot_mount_folder` (77) | Needs an account. Ask the owner for an access key, then `mount --key`. |
| `access_key_cannot_issue_keys` (77) | Only an account can issue keys. |
| `access_key_cannot_invite_people` (77) | Only an account can invite people. |
| `access_key_cannot_revoke` (77) | Only an account can revoke access. |
| `access_key_cannot_list_grants` (77) | An access key sees only its own access. |
| `access_key_is_read_only` (77) | This key is read-only. Ask the owner for a read/write key. |

## Paths and environment

- Config: `~/.config/valv/config.toml`
- Socket: `~/.local/share/valv/valvd.sock`
- Local mirror: `~/.local/share/valv/sync.db`

`VALV_BACKEND_URL`, `VALV_WEB_BASE_URL`, `VALV_VERSION` (pins the update target), `VALV_NO_UPDATE_CHECK=1`, `VALV_INSTALL_DIR` (install script only), `RUST_LOG`.
