# @valv/contracts-ipc

This package defines TypeScript types for the local daemon control protocol between `valvd` and its clients. It is produced by `valvd` and consumed by `valv-cli` and the macOS app/File Provider extensions.

Two local transports carry the same protocol: `valv-cli` and other non-sandboxed clients connect over the Unix socket at `~/.local/share/valv/valvd.sock`, while the sandboxed macOS app and File Provider extensions connect over loopback TCP, discovering the bound port through a file in the shared app-group container.

## Sub-Modules

- `control`: daemon status (`DaemonStatus`, `MountStatus`, `AccountStatus`), mount (`MountRequest`/`MountResponse`), unmount (`UnmountRequest`), and sync (`SyncRequest`) request/response types, plus `NodePathResponse`. `valvd` also exposes bodyless `pause`/`resume` routes and `versions`/`restore` routes typed on the Rust side by `valv_sync::protocol::ipc` rather than by this package.
- `fileprovider`: enumeration (`FpEnumerateQuery`/`FpEnumerateResponse`, `FpItem`), change tracking (`FpAnchorResponse`, `FpChangesResponse`, `FpWatchQuery`/`FpWatchResponse`), content download (`FpContentResponse`, `FpChunkDownload`), upload (`FpUploadRequest`/`FpUploadQueued`), delete (`FpDeleteRequest`), move (`FpMoveRequest`/`FpMoveResponse`), and share/invite (`FpShareRequest`/`FpShareResponse`) types for the File Provider extension.

## Typecheck

```bash
pnpm exec tsc --noEmit
```
