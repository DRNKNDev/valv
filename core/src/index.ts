export { createAuth, authenticateRequest, createAuthMiddleware, generateDeviceToken, sha256Hex } from "./auth/index.js";
export type { AuthResult, CoreAuth, CoreDb, CoreSchema, Principal } from "./auth/index.js";
export { createDeviceAuthRouter } from "./auth/device.js";
export { createBlobstoreRouter, chunkKey } from "./blobstore/index.js";
export { pgSchema, sqliteSchema } from "./db/schema.js";
export type {
  Chunk,
  Device,
  FolderGrant,
  FolderInvite,
  Node,
  OpLogRow,
  SharedFolder,
  Version,
} from "./db/schema.js";
export { createSendInviteEmail } from "./email/index.js";
export type { SendInviteEmail } from "./email/index.js";
export {
  DEFAULT_CHUNK_GC_INTERVAL_MS,
  DEFAULT_CHUNK_GRACE_PERIOD_MS,
  DEFAULT_OPLOG_RETENTION_MS,
  DEFAULT_OPLOG_TRUNCATION_INTERVAL_MS,
  DEFAULT_TOMBSTONE_PURGE_INTERVAL_MS,
  DEFAULT_TOMBSTONE_RETENTION_MS,
  startGc,
} from "./gc/index.js";
export { checkGrant, MAX_GRANT_WALK_DEPTH } from "./metadata/authz.js";
export { createMetadataRouter } from "./metadata/index.js";
export type { MetadataHub } from "./metadata/index.js";
export { createHub, createRealtimeRouter } from "./realtime/index.js";
export type { Hub, RealtimeSocket } from "./realtime/index.js";

export const corePackageName = "@valv/core";
