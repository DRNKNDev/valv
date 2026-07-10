# @valv/contracts-sync

This package defines TypeScript types for the server-authoritative operation log protocol: op submission request/response shapes (`SubmitOpRequest`, `SubmitOpResponse`, including `applied`, `conflict_copy`, `superseded`, and `conflict` results), op log entries and delta pull (`OpLogEntry`, `DeltaPullResponse`), folder tree snapshots (`NodeSnapshot`, `FolderTreeResponse`), and the WebSocket push notification type `WsPushNotification`. It also exports the protocol version contract: `PROTOCOL_VERSION` and the `X-Valv-Protocol` header name (`PROTOCOL_HEADER`) used to reject clients below `core`'s configured `VALV_MIN_PROTOCOL`.

The producer is the Node.js backend in `core`. The consumer is the Rust sync engine in `crates/valv-sync/src/protocol/sync.rs`.

## Typecheck

```bash
pnpm exec tsc --noEmit
```
