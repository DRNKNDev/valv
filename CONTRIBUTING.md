# Contributing To Valv

Thanks for your interest in contributing. This guide covers everything needed to set up, verify, and submit a change from a standalone clone of this repository (`DRNKNDev/valv`). No access to any other repository is required.

## Architecture Orientation

- `contracts/sync`, `contracts/http`, `contracts/ipc`: TypeScript types shared between the backend, CLI, and daemon.
- `core`: Node.js backend (Hono, Drizzle, Better Auth) with SQLite/PostgreSQL and S3-compatible storage.
- `crates/valv-sync`: Rust sync engine (chunking, storage, filesystem watching, local mirror).
- `crates/valvd`: Rust daemon that owns the sync engine and exposes the local control API.
- `crates/valv-cli`: Rust CLI for controlling the daemon.
- `macos/Valv`: the native macOS app Xcode project, including the File Provider and Finder Sync extensions.
- `macos/DaemonKit`: the shared Swift daemon-control library used by the app and both extensions.
- `e2e`: MinIO-backed API integration tests and the numbered daemon smoke suite under `e2e/smoke`.

## Prerequisites

- Node.js 24 and pnpm 10.9.0
- Rust stable (`cargo`)
- For macOS app/extension/DaemonKit work: Xcode 26.2 or newer
- For the API e2e suite and smoke suite: a local MinIO instance and the `mc` (MinIO Client) CLI

## Setup

```bash
pnpm install
```

Run from the repository root. This installs every Node workspace member (`contracts/*`, `core`, `e2e`) from the single root `pnpm-lock.yaml`.

```bash
cargo check --workspace
```

Run from `crates/`.

## Verification Commands

Run the checks relevant to what you changed before opening a pull request.

### TypeScript

```bash
pnpm typecheck            # all contracts, core, and e2e
pnpm typecheck:contracts  # contracts/http, contracts/ipc, contracts/sync only
pnpm typecheck:core       # core only
pnpm typecheck:e2e        # e2e only
```

Run from the repository root.

### Core Unit Tests

```bash
pnpm test:core
```

Run from the repository root.

### API End-To-End Tests

```bash
pnpm test:e2e
```

Run from the repository root. Requires a running MinIO instance reachable at the endpoint/credentials the `e2e` suite expects; see [`core/README.md`](./core/README.md).

### Rust

```bash
cargo check --workspace
cargo test --workspace
```

Run from `crates/`.

### Daemon Smoke Suite

```bash
./e2e/smoke/run-all.sh
```

Run from the repository root. Requires MinIO, `mc`, and debug `valvd`/`valv` binaries built with `cargo build --workspace` from `crates/`. Each numbered script under `e2e/smoke/` exercises one scenario (mount, sync, conflicts, grants, and related daemon behavior).

### Swift (DaemonKit)

```bash
swift test --package-path macos/DaemonKit
```

### macOS App And File Provider

Open `macos/Valv/Valv.xcodeproj` in Xcode 26.2 or newer and build the `Valv`, `ValvFileProvider`, and `ValvFileProviderUI` targets. For a command-line compile-only check with signing disabled:

```bash
xcodebuild -project "macos/Valv/Valv.xcodeproj" -scheme "Valv" -configuration Debug CODE_SIGNING_ALLOWED=NO build
```

## Code And Generated-File Expectations

- Match the existing near-zero inline-comment style; only comment non-obvious *why*, never restate *what* the code does.
- Do not hand-edit generated files (Drizzle migrations, `Cargo.lock`, `pnpm-lock.yaml`) outside the tool that generates them.
- Never commit secrets, tokens, real account data, or `.env` files. Redact logs and screenshots before attaching them to an issue or pull request.
- Keep commit messages terse and focused on why a change was made.

## Spec Impact

Maintainers track behavior-affecting changes internally with [OpenSpec](https://github.com/openspec-dev/openspec), but those spec files live in the private monorepo, not in this repository, so you won't have them to reference. If your change alters documented behavior (an API, a CLI command, a config option, etc.), just describe the change and why in your pull request; maintainers will reconcile it against the relevant internal spec during the private-repository integration step described below.

## How Pull Requests Are Integrated

`DRNKNDev/valv` is a public mirror of a subtree inside a private monorepo that also contains hosted-service code. Maintainers develop primarily in the private monorepo and periodically push a subtree split of that OSS subtree here.

When you open a pull request against `DRNKNDev/valv`:

1. A maintainer reviews it on GitHub like any other pull request.
2. Once approved, the change is pulled into the private monorepo's OSS subtree and merged there.
3. On the next mirror push, your change (attributed to you) appears on `main` here as part of that subtree split, and your original pull request is closed as merged/superseded rather than fast-forwarded in place.

This means there can be a delay between approval and your commit landing on `main`, and the final commit hash may differ from your branch's. This is expected and does not indicate your contribution was dropped.

## Getting Help

For questions while contributing, join the Valv Discord: <https://discord.gg/29a3dVRdE>
