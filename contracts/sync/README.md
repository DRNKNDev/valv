# @valv/contracts-sync

This package defines TypeScript types for the server-authoritative operation log protocol: op submission request/response shapes such as `SubmitOpRequest` and `SubmitOpResponse`, op log entries such as `OpLogEntry` and `DeltaPullResponse`, folder tree snapshot types, and the WebSocket push notification type `WsPushNotification`.

The producer is the Node.js backend in `oss/core`. The consumer is the Rust sync engine in `oss/crates/valv-sync/src/protocol/sync.rs`.

## Typecheck

```bash
pnpm exec tsc --noEmit
```
