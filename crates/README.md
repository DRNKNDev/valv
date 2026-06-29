# Valv Rust Crates

## What's Here

- `valv-sync`: sync engine library with the CDC chunker, S3-compatible chunk storage client, sync loop, local SQLite mirror, and filesystem watcher.
- `valvd`: daemon binary that owns the sync engine, exposes the local control API over a Unix socket, and manages launchd/systemd service registration.
- `valv-cli`: thin CLI binary that sends commands to the daemon over the Unix socket.

`valv-sync` contains the sync logic. `valvd` and `valv-cli` are intentionally thin wrappers around it.

## Prerequisites

- Rust stable >= 1.80
- `cargo`

## Build

Run these commands from `oss/crates/`:

```bash
cargo build --workspace
cargo test --workspace
```

## Config File

Create `~/.config/valv/config.toml` after registering a device with the core backend. The `device_id` and `device_token` values come from the device registration steps in `../core/README.md`.

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

## Running The Daemon

Run the daemon in the foreground during development:

```bash
./target/debug/valvd run
```

The daemon creates its Unix socket at `~/.local/share/valv/valvd.sock`. Skip `valv daemon install` in dev; that command registers the daemon as a launchd LaunchAgent on macOS.

## CLI Quick Reference

See [`valv-cli/README.md`](./valv-cli/README.md) for full CLI usage, setup, command examples, and troubleshooting.

```bash
valv status
valv mount <path>
valv pause
valv resume
valv grant create
valv grants
valv grant revoke
```
