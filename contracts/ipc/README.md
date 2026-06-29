# @valv/contracts-ipc

This package defines TypeScript types for the local Unix socket control API between the daemon and its clients.

The producer is `valvd`. The consumers are `valv-cli` and the macOS Swift GUI/File Provider extension.

## Sub-Modules

- `control`: daemon status, mount, pause, and resume commands.
- `fileprovider`: enumeration, content, upload, and delete calls for the File Provider extension.

## Typecheck

```bash
pnpm exec tsc --noEmit
```
