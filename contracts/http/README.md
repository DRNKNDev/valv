# @valv/contracts-http

This package defines TypeScript types for the Git LFS-modeled chunk batch API: `BatchRequest`, `BatchResponse`, `BatchResponseObject`, and `BatchAction`.

The producer is the Node.js backend blobstore implementation in `core/src/blobstore/`. The consumer is the Rust storage client in `crates/valv-sync/src/storage/`.

## Typecheck

```bash
pnpm exec tsc --noEmit
```
