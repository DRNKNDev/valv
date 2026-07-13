# Valv Rust Crates

## What's Here

- `valv-sync`: the sync engine library. Content-defined chunking, S3-compatible chunk storage client, the sync protocol, the local SQLite mirror, filesystem watching, materialization, conflict handling, version/restore support, and update-check support all live here.
- `valvd`: the long-running daemon. It owns the sync engine for every mounted folder and exposes a local control API (status, mount/unmount, pause/resume, sync, versions, restore) and a File Provider API (`fp/*`) over local transports, plus launchd/systemd service registration.
- `valv-cli`: the user- and automation-facing `valv` binary. It talks to `valvd` over the local control API for daemon operations and calls selected Core backend endpoints directly (browser login, sharing) using the configured device token.

## Prerequisites

- Rust stable toolchain (no pinned MSRV; CI builds against current `stable`)
- `cargo`

## Build

Run these commands from `crates/`:

```bash
cargo build --workspace --locked
cargo test --workspace --locked
```

`--locked` uses the committed `Cargo.lock` without re-resolving dependencies.

Released prebuilt binaries currently cover macOS arm64 (`aarch64-apple-darwin`) and Linux x86_64 (`x86_64-unknown-linux-gnu`). Other targets require a source build.

## Config File

Browser-based `valv login` is the preferred setup path for end users: it opens `https://valvsync.com/login`, completes device pairing, and writes `~/.config/valv/config.toml` for you.

For self-hosted or development setups, register a device against your own Core backend (see [`../core/README.md`](../core/README.md)) and write the file manually:

```toml
# Core backend URL.
backend_url = "http://localhost:4747"

# Returned as device_id by POST /auth/device.
device_id = "replace-with-device-id"

# Returned as token by POST /auth/device.
device_token = "replace-with-device-token"

# Optional. Defaults to the hostname when omitted.
device_name = "Dev Mac"

# [[mounts]]
# path = "/Users/you/Valv"
# folder_id = "replace-with-folder-id"
```

Do not commit `config.toml` or its `device_token`; treat it like any other credential.

## Running The Daemon And Using The CLI

Run the daemon in the foreground during development:

```bash
./target/debug/valvd run
```

Local transport depends on the client:

- `valv-cli` and other non-sandboxed clients connect over the Unix socket at `~/.local/share/valv/valvd.sock`.
- The sandboxed macOS app and File Provider extensions cannot use a Unix socket, so they connect over loopback TCP; `valvd` writes the bound port to a file inside the shared macOS app-group container for those clients to discover.

There is no user-facing `valv daemon install`: any daemon-bound `valv` command installs and starts the daemon itself the first time it runs. `valv daemon restart` and `valv daemon uninstall` stop/reinstall or remove the daemon as a service: a launchd LaunchAgent on macOS, or a systemd user service on Linux. Skip them in development and run `valvd run` directly so logs stay in the foreground.

### CLI Quick Reference

See [`valv-cli/README.md`](./valv-cli/README.md) for full CLI usage, setup, command examples, and troubleshooting.

```bash
valv login
valv status
valv mount <path> --new
valv unmount <path>
valv pause
valv resume
valv sync
valv versions <path>
valv restore <path> <version-id>
valv share <path> --to <email>
valv share <path>
valv unshare <path> --to <email>
valv update
```

`--json` is a global flag, honored by every command, for machine-readable output.
