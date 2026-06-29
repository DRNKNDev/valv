# Valv

Valv is a self-hostable file sync project in the Dropbox / Nextcloud class, built around an open core backend, a Rust sync engine and daemon, and native desktop integration. The public code is licensed under AGPL-3.0.

## Components

- `contracts/sync`: TypeScript types for operation submission, delta pull, folder snapshots, and WebSocket push notifications.
- `contracts/http`: TypeScript types for the Git LFS-modeled chunk batch API used for blob upload and download.
- `contracts/ipc`: TypeScript types for the local Unix socket control API used by the daemon, CLI, and macOS client.
- `core`: Node.js backend using Hono, Drizzle, Better Auth, and S3-compatible bucket storage.
- `crates/valv-sync`: Rust sync engine with chunking, storage, filesystem watching, and local mirror logic.
- `crates/valvd`: Rust daemon that owns the sync engine and exposes the local control API.
- `crates/valv-cli`: Rust CLI for controlling the daemon from a terminal.
- `macos/spike`: macOS File Provider spike used to validate native sync integration behavior.

## Getting Started

Use [`core/README.md`](./core/README.md) to start the Node.js backend and register a device. Use [`crates/README.md`](./crates/README.md) to build and run the Rust daemon and CLI against that backend.

## License

See [`LICENSE.md`](./LICENSE.md) for the AGPL-3.0 license.
