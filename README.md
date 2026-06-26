# Valv

Valv is an open-source file sync project in the Dropbox / Nextcloud class.

It is currently **scaffolded but still early and mostly stubbed**. The packages and workspaces exist, compile, and define the intended architecture, but they do not implement real sync behavior yet.

## What Is Here

- `contracts/sync`: TypeScript sync contract package
- `contracts/http`: TypeScript HTTP contract package
- `contracts/ipc`: TypeScript IPC contract package
- `core`: TypeScript Core package stub
- `crates/valv-sync`: Rust sync engine crate stub
- `crates/valvd`: Rust daemon crate stub
- `crates/valv-cli`: Rust CLI crate stub
- `macos/app`: placeholder for the native macOS app
- `macos/file-provider`: placeholder for the File Provider extension

## Current Status

Implemented today:
- package boundaries and workspace layout
- compilable TypeScript stubs for contracts and Core
- compilable Rust workspace stubs
- CI for typechecking and `cargo check`

Not implemented yet:
- real Core API logic
- real sync protocol logic
- real daemon / CLI behavior
- macOS app and File Provider implementation

## Development

There is no root Node workspace manifest here. Run Node commands inside each package.

TypeScript checks:

```bash
cd contracts/sync
pnpm install
pnpm exec tsc --noEmit

cd ../http
pnpm install
pnpm exec tsc --noEmit

cd ../ipc
pnpm install
pnpm exec tsc --noEmit

cd ../../core
pnpm install
pnpm exec tsc --noEmit
```

Rust check:

```bash
cd crates
cargo check --workspace
```

## CI

CI lives in `.github/workflows/ci.yml` and currently runs:
- `tsc --noEmit` for each contracts package
- `tsc --noEmit` for `core`
- `cargo check --workspace` for `crates`

## Direction

Valv is being designed around:
- a self-hostable Core for sync, metadata, and blob coordination
- a Rust-based sync engine and daemon
- a native macOS client stack on top of that daemon

## License

See [`LICENSE.md`](./LICENSE.md).
