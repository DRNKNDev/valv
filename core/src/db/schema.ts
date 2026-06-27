import type { InferSelectModel } from "drizzle-orm";

import {
  chunks,
  devices,
  folderGrants,
  folderInvites,
  nodes,
  opLog,
  sharedFolders,
  versions,
} from "./schema.pg.js";

export { pgSchema } from "./schema.pg.js";
export { sqliteSchema } from "./schema.sqlite.js";

export type Device = InferSelectModel<typeof devices>;
export type SharedFolder = InferSelectModel<typeof sharedFolders>;
export type FolderGrant = InferSelectModel<typeof folderGrants>;
export type FolderInvite = InferSelectModel<typeof folderInvites>;
export type Node = InferSelectModel<typeof nodes>;
export type Version = InferSelectModel<typeof versions>;
export type Chunk = InferSelectModel<typeof chunks>;
export type OpLogRow = InferSelectModel<typeof opLog>;
