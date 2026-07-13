import { sql } from "drizzle-orm";
import {
  type AnyPgColumn,
  bigint,
  bigserial,
  boolean,
  check,
  index,
  integer,
  jsonb,
  pgTable,
  primaryKey,
  text,
  timestamp,
  uniqueIndex,
  uuid,
} from "drizzle-orm/pg-core";

export const devices = pgTable("devices", {
  deviceId: uuid("device_id").primaryKey().defaultRandom(),
  userId: text("user_id"),
  name: text("name").notNull(),
  tokenHash: text("token_hash").notNull(),
  createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
});

export const user = pgTable("user", {
  id: text("id").primaryKey(),
  name: text("name").notNull(),
  email: text("email").notNull().unique(),
  emailVerified: boolean("emailVerified").notNull(),
  image: text("image"),
  createdAt: timestamp("createdAt", { withTimezone: true }).notNull(),
  updatedAt: timestamp("updatedAt", { withTimezone: true }).notNull(),
});

export const session = pgTable("session", {
  id: text("id").primaryKey(),
  userId: text("userId")
    .notNull()
    .references(() => user.id, { onDelete: "cascade" }),
  token: text("token").notNull().unique(),
  expiresAt: timestamp("expiresAt", { withTimezone: true }).notNull(),
  ipAddress: text("ipAddress"),
  userAgent: text("userAgent"),
  createdAt: timestamp("createdAt", { withTimezone: true }).notNull(),
  updatedAt: timestamp("updatedAt", { withTimezone: true }).notNull(),
});

export const account = pgTable("account", {
  id: text("id").primaryKey(),
  accountId: text("accountId").notNull(),
  providerId: text("providerId").notNull(),
  userId: text("userId")
    .notNull()
    .references(() => user.id, { onDelete: "cascade" }),
  accessToken: text("accessToken"),
  refreshToken: text("refreshToken"),
  idToken: text("idToken"),
  accessTokenExpiresAt: timestamp("accessTokenExpiresAt", { withTimezone: true }),
  refreshTokenExpiresAt: timestamp("refreshTokenExpiresAt", { withTimezone: true }),
  scope: text("scope"),
  password: text("password"),
  createdAt: timestamp("createdAt", { withTimezone: true }).notNull(),
  updatedAt: timestamp("updatedAt", { withTimezone: true }).notNull(),
});

export const verification = pgTable("verification", {
  id: text("id").primaryKey(),
  identifier: text("identifier").notNull(),
  value: text("value").notNull(),
  expiresAt: timestamp("expiresAt", { withTimezone: true }).notNull(),
  createdAt: timestamp("createdAt", { withTimezone: true }),
  updatedAt: timestamp("updatedAt", { withTimezone: true }),
});

export const sharedFolders = pgTable("shared_folders", {
  folderId: uuid("folder_id").primaryKey().defaultRandom(),
  name: text("name").notNull(),
  ownerUserId: text("owner_user_id").notNull(),
  createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
});

export const chunks = pgTable("chunks", {
  chunkHash: text("chunk_hash").primaryKey(),
  sizeBytes: integer("size_bytes").notNull(),
  refcount: integer("refcount").notNull().default(0),
  createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
});

export const nodes = pgTable(
  "nodes",
  {
    nodeId: uuid("node_id").primaryKey().defaultRandom(),
    folderId: uuid("folder_id")
      .notNull()
      .references(() => sharedFolders.folderId),
    parentId: uuid("parent_id").references((): AnyPgColumn => nodes.nodeId),
    name: text("name").notNull(),
    type: text("type", { enum: ["file", "folder"] }).notNull(),
    currentVersionId: uuid("current_version_id").references(
      (): AnyPgColumn => versions.versionId,
    ),
    deletedAt: timestamp("deleted_at", { withTimezone: true }),
    serverSeq: bigint("server_seq", { mode: "number" }).notNull().default(0),
  },
  (table) => [
    uniqueIndex("nodes_live_name_unique")
      .on(table.folderId, table.parentId, table.name)
      .where(sql`${table.deletedAt} IS NULL`),
    index("nodes_folder_parent_idx").on(table.folderId, table.parentId),
  ],
);

export const versions = pgTable(
  "versions",
  {
    versionId: uuid("version_id").primaryKey().defaultRandom(),
    nodeId: uuid("node_id")
      .notNull()
      .references(() => nodes.nodeId, { onDelete: "cascade" }),
    manifest: jsonb("manifest").notNull(),
    contentHash: text("content_hash").notNull(),
    sizeBytes: bigint("size_bytes", { mode: "number" }).notNull(),
    authorDeviceId: uuid("author_device_id")
      .notNull()
      .references(() => devices.deviceId),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
    isConflictCopy: boolean("is_conflict_copy").notNull().default(false),
  },
  (table) => [index("versions_node_created_idx").on(table.nodeId, table.createdAt)],
);

export const versionChunks = pgTable(
  "version_chunks",
  {
    versionId: uuid("version_id")
      .notNull()
      .references(() => versions.versionId, { onDelete: "cascade" }),
    nodeId: uuid("node_id")
      .notNull()
      .references(() => nodes.nodeId),
    chunkHash: text("chunk_hash")
      .notNull()
      .references(() => chunks.chunkHash),
  },
  (table) => [
    primaryKey({ columns: [table.versionId, table.chunkHash] }),
    index("version_chunks_chunk_hash_idx").on(table.chunkHash),
  ],
);

export const folderGrants = pgTable(
  "folder_grants",
  {
    grantId: uuid("grant_id").primaryKey().defaultRandom(),
    folderId: uuid("folder_id")
      .notNull()
      .references(() => sharedFolders.folderId),
    scopeNodeId: uuid("scope_node_id")
      .notNull()
      .references(() => nodes.nodeId),
    userId: text("user_id"),
    deviceId: uuid("device_id").references(() => devices.deviceId),
    name: text("name"),
    role: text("role", { enum: ["owner", "collaborator"] })
      .notNull()
      .default("collaborator"),
    canRead: boolean("can_read").notNull().default(true),
    canWrite: boolean("can_write").notNull().default(true),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
    createdByUserId: text("created_by_user_id"),
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
    uniqueIndex("folder_grants_folder_name_unique")
      .on(table.folderId, table.name)
      .where(sql`${table.deviceId} IS NOT NULL`),
  ],
);

export const folderInvites = pgTable(
  "folder_invites",
  {
    inviteId: uuid("invite_id").notNull().defaultRandom(),
    inviteToken: text("invite_token").primaryKey(),
    folderId: uuid("folder_id")
      .notNull()
      .references(() => sharedFolders.folderId),
    scopeNodeId: uuid("scope_node_id")
      .notNull()
      .references(() => nodes.nodeId),
    invitedEmail: text("invited_email").notNull(),
    invitedByUserId: text("invited_by_user_id").notNull(),
    canWrite: boolean("can_write").notNull().default(true),
    status: text("status", { enum: ["pending", "accepted", "revoked", "expired"] })
      .notNull()
      .default("pending"),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
    expiresAt: timestamp("expires_at", { withTimezone: true }).notNull(),
  },
  (table) => [uniqueIndex("folder_invites_invite_id_unique").on(table.inviteId)],
);

export const opLog = pgTable(
  "op_log",
  {
    serverSeq: bigserial("server_seq", { mode: "number" }).primaryKey(),
    folderId: uuid("folder_id")
      .notNull()
      .references(() => sharedFolders.folderId),
    nodeId: uuid("node_id").notNull(),
    opType: text("op_type", {
      enum: ["create", "rename", "move", "delete", "new_version"],
    }).notNull(),
    opPayload: jsonb("op_payload").notNull(),
    basedOnSeq: bigint("based_on_seq", { mode: "number" }),
    actorDeviceId: uuid("actor_device_id")
      .notNull()
      .references(() => devices.deviceId),
    appliedAt: timestamp("applied_at", { withTimezone: true }).notNull().defaultNow(),
  },
  (table) => [index("op_log_folder_seq_idx").on(table.folderId, table.serverSeq)],
);

export const pgSchema = {
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
  versionChunks,
  opLog,
};
