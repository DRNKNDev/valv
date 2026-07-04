import { sql } from "drizzle-orm";
import {
  type AnySQLiteColumn,
  check,
  index,
  integer,
  sqliteTable,
  text,
  uniqueIndex,
} from "drizzle-orm/sqlite-core";

const sqliteNowMs = sql`(unixepoch() * 1000)`;

export const user = sqliteTable("user", {
  id: text("id").primaryKey(),
  name: text("name").notNull(),
  email: text("email").notNull().unique(),
  emailVerified: integer("emailVerified", { mode: "boolean" }).notNull(),
  image: text("image"),
  createdAt: integer("createdAt", { mode: "timestamp_ms" }).notNull(),
  updatedAt: integer("updatedAt", { mode: "timestamp_ms" }).notNull(),
});

export const session = sqliteTable("session", {
  id: text("id").primaryKey(),
  userId: text("userId")
    .notNull()
    .references(() => user.id, { onDelete: "cascade" }),
  token: text("token").notNull().unique(),
  expiresAt: integer("expiresAt", { mode: "timestamp_ms" }).notNull(),
  ipAddress: text("ipAddress"),
  userAgent: text("userAgent"),
  createdAt: integer("createdAt", { mode: "timestamp_ms" }).notNull(),
  updatedAt: integer("updatedAt", { mode: "timestamp_ms" }).notNull(),
});

export const account = sqliteTable("account", {
  id: text("id").primaryKey(),
  accountId: text("accountId").notNull(),
  providerId: text("providerId").notNull(),
  userId: text("userId")
    .notNull()
    .references(() => user.id, { onDelete: "cascade" }),
  accessToken: text("accessToken"),
  refreshToken: text("refreshToken"),
  idToken: text("idToken"),
  accessTokenExpiresAt: integer("accessTokenExpiresAt", { mode: "timestamp_ms" }),
  refreshTokenExpiresAt: integer("refreshTokenExpiresAt", { mode: "timestamp_ms" }),
  scope: text("scope"),
  password: text("password"),
  createdAt: integer("createdAt", { mode: "timestamp_ms" }).notNull(),
  updatedAt: integer("updatedAt", { mode: "timestamp_ms" }).notNull(),
});

export const verification = sqliteTable("verification", {
  id: text("id").primaryKey(),
  identifier: text("identifier").notNull(),
  value: text("value").notNull(),
  expiresAt: integer("expiresAt", { mode: "timestamp_ms" }).notNull(),
  createdAt: integer("createdAt", { mode: "timestamp_ms" }),
  updatedAt: integer("updatedAt", { mode: "timestamp_ms" }),
});

export const devices = sqliteTable("devices", {
  deviceId: text("device_id").primaryKey(),
  userId: text("user_id"),
  name: text("name").notNull(),
  tokenHash: text("token_hash").notNull(),
  createdAt: integer("created_at", { mode: "timestamp_ms" }).notNull().default(sqliteNowMs),
});

export const sharedFolders = sqliteTable("shared_folders", {
  folderId: text("folder_id").primaryKey(),
  name: text("name").notNull(),
  ownerUserId: text("owner_user_id").notNull(),
  createdAt: integer("created_at", { mode: "timestamp_ms" }).notNull().default(sqliteNowMs),
});

export const chunks = sqliteTable("chunks", {
  chunkHash: text("chunk_hash").primaryKey(),
  sizeBytes: integer("size_bytes").notNull(),
  refcount: integer("refcount").notNull().default(0),
  createdAt: integer("created_at", { mode: "timestamp_ms" }).notNull().default(sqliteNowMs),
});

export const nodes = sqliteTable(
  "nodes",
  {
    nodeId: text("node_id").primaryKey(),
    folderId: text("folder_id")
      .notNull()
      .references(() => sharedFolders.folderId),
    parentId: text("parent_id").references((): AnySQLiteColumn => nodes.nodeId),
    name: text("name").notNull(),
    type: text("type", { enum: ["file", "folder"] }).notNull(),
    currentVersionId: text("current_version_id").references(
      (): AnySQLiteColumn => versions.versionId,
    ),
    deletedAt: integer("deleted_at", { mode: "timestamp_ms" }),
    serverSeq: integer("server_seq").notNull().default(0),
  },
  (table) => [
    uniqueIndex("nodes_live_name_unique")
      .on(table.folderId, table.parentId, table.name)
      .where(sql`${table.deletedAt} IS NULL`),
    index("nodes_folder_parent_idx").on(table.folderId, table.parentId),
  ],
);

export const versions = sqliteTable(
  "versions",
  {
    versionId: text("version_id").primaryKey(),
    nodeId: text("node_id")
      .notNull()
      .references(() => nodes.nodeId, { onDelete: "cascade" }),
    manifest: text("manifest", { mode: "json" }).notNull(),
    contentHash: text("content_hash").notNull(),
    sizeBytes: integer("size_bytes").notNull(),
    authorDeviceId: text("author_device_id")
      .notNull()
      .references(() => devices.deviceId),
    createdAt: integer("created_at", { mode: "timestamp_ms" }).notNull().default(sqliteNowMs),
    isConflictCopy: integer("is_conflict_copy", { mode: "boolean" }).notNull().default(false),
  },
  (table) => [index("versions_node_created_idx").on(table.nodeId, table.createdAt)],
);

export const folderGrants = sqliteTable(
  "folder_grants",
  {
    grantId: text("grant_id").primaryKey(),
    folderId: text("folder_id")
      .notNull()
      .references(() => sharedFolders.folderId),
    scopeNodeId: text("scope_node_id")
      .notNull()
      .references(() => nodes.nodeId),
    userId: text("user_id"),
    deviceId: text("device_id").references(() => devices.deviceId),
    role: text("role", { enum: ["owner", "collaborator"] })
      .notNull()
      .default("collaborator"),
    canRead: integer("can_read", { mode: "boolean" }).notNull().default(true),
    canWrite: integer("can_write", { mode: "boolean" }).notNull().default(true),
    createdAt: integer("created_at", { mode: "timestamp_ms" }).notNull().default(sqliteNowMs),
  },
  (table) => [
    check(
      "folder_grants_principal_xor",
      sql`(${table.userId} IS NULL) <> (${table.deviceId} IS NULL)`,
    ),
    index("folder_grants_scope_principal_idx").on(
      table.scopeNodeId,
      table.folderId,
      table.userId,
      table.deviceId,
    ),
  ],
);

export const folderInvites = sqliteTable("folder_invites", {
  inviteToken: text("invite_token").primaryKey(),
  folderId: text("folder_id")
    .notNull()
    .references(() => sharedFolders.folderId),
  scopeNodeId: text("scope_node_id")
    .notNull()
    .references(() => nodes.nodeId),
  invitedEmail: text("invited_email").notNull(),
  invitedByUserId: text("invited_by_user_id").notNull(),
  canWrite: integer("can_write", { mode: "boolean" }).notNull().default(true),
  status: text("status", { enum: ["pending", "accepted", "revoked", "expired"] })
    .notNull()
    .default("pending"),
  createdAt: integer("created_at", { mode: "timestamp_ms" }).notNull().default(sqliteNowMs),
  expiresAt: integer("expires_at", { mode: "timestamp_ms" }).notNull(),
});

export const opLog = sqliteTable(
  "op_log",
  {
    serverSeq: integer("server_seq").primaryKey({ autoIncrement: true }),
    folderId: text("folder_id")
      .notNull()
      .references(() => sharedFolders.folderId),
    nodeId: text("node_id").notNull(),
    opType: text("op_type", {
      enum: ["create", "rename", "move", "delete", "new_version"],
    }).notNull(),
    opPayload: text("op_payload", { mode: "json" }).notNull(),
    basedOnSeq: integer("based_on_seq"),
    actorDeviceId: text("actor_device_id")
      .notNull()
      .references(() => devices.deviceId),
    appliedAt: integer("applied_at", { mode: "timestamp_ms" }).notNull().default(sqliteNowMs),
  },
  (table) => [index("op_log_folder_seq_idx").on(table.folderId, table.serverSeq)],
);

export const sqliteSchema = {
  user,
  session,
  account,
  verification,
  devices,
  sharedFolders,
  folderGrants,
  folderInvites,
  nodes,
  versions,
  chunks,
  opLog,
};
