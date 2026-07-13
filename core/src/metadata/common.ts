import { randomUUID } from "node:crypto";

import { and, desc, eq, gt, inArray, isNull, sql } from "drizzle-orm";
import type { Context } from "hono";

import type { AuthVariables, CoreAuth, CoreSchema, Principal } from "../auth/index.js";
import { sqliteSchema } from "../db/schema.js";
import { checkGrant } from "./authz.js";

export type MetadataVariables = AuthVariables;

export type MetadataHub = {
  notify: (folderId: string, serverSeq: number) => void;
};

export type MetadataDeps = {
  auth: CoreAuth;
  hub: MetadataHub;
};

export function newId(): string {
  return randomUUID();
}

export async function inTransaction<T>(
  auth: CoreAuth,
  fn: (tx: CoreAuth["db"]) => Promise<T>,
  opts: { atomicOnSqlite?: boolean } = {},
): Promise<T> {
  if (typeof auth.db.transaction === "function" && supportsForUpdate(auth.schema)) {
    return auth.db.transaction(fn);
  }
  if (opts.atomicOnSqlite && auth.schema === sqliteSchema && typeof auth.db.run === "function") {
    return inRawSqliteTransaction(auth, fn);
  }
  return fn(auth.db);
}

// better-sqlite3's db.transaction() requires a sync callback and throws on a promise, hence the hand-rolled BEGIN/COMMIT/ROLLBACK.
async function inRawSqliteTransaction<T>(auth: CoreAuth, fn: (tx: CoreAuth["db"]) => Promise<T>): Promise<T> {
  await auth.db.run(sql`BEGIN IMMEDIATE`);
  let result: T;
  try {
    result = await fn(auth.db);
  } catch (error) {
    await auth.db.run(sql`ROLLBACK`);
    throw error;
  }
  await auth.db.run(sql`COMMIT`);
  return result;
}

export function supportsForUpdate(schema: CoreSchema): boolean {
  return schema !== sqliteSchema;
}

export function requirePrincipal(ctx: Context<{ Variables: MetadataVariables }>): Principal {
  const principal = ctx.var.principal;
  if (!principal) {
    throw new Error("missing authenticated principal");
  }
  return principal;
}

export async function getFolderRoot(auth: CoreAuth, folderId: string, db = auth.db): Promise<string | undefined> {
  if (hasFolderRootLoader(db)) {
    return db.getFolderRootForAuthz(folderId);
  }

  const rows = await db
    .select({ nodeId: auth.schema.nodes.nodeId })
    .from(auth.schema.nodes)
    .where(and(eq(auth.schema.nodes.folderId, folderId), isNull(auth.schema.nodes.parentId)))
    .limit(1);
  return rows[0]?.nodeId;
}

export async function resolveEffectiveUserId(auth: CoreAuth, principal: Principal): Promise<string | undefined> {
  if (principal.type === "user") {
    return principal.userId;
  }
  return resolveDeviceUserId(auth, principal.deviceId);
}

export async function resolveDeviceUserId(auth: CoreAuth, deviceId: string): Promise<string | undefined> {
  if (hasDeviceUserRouteLoader(auth.db)) {
    return auth.db.getDeviceUserIdForRoute(deviceId);
  }
  const rows = await auth.db
    .select({ userId: auth.schema.devices.userId })
    .from(auth.schema.devices)
    .where(eq(auth.schema.devices.deviceId, deviceId))
    .limit(1);
  return rows[0]?.userId ?? undefined;
}

function hasFolderRootLoader(db: CoreAuth["db"]): db is CoreAuth["db"] & {
  getFolderRootForAuthz: (folderId: string) => Promise<string | undefined>;
} {
  return "getFolderRootForAuthz" in db;
}

function hasDeviceUserRouteLoader(db: CoreAuth["db"]): db is CoreAuth["db"] & {
  getDeviceUserIdForRoute: (deviceId: string) => Promise<string | undefined>;
} {
  return "getDeviceUserIdForRoute" in db;
}

export async function requireUserBackedPrincipal(
  auth: CoreAuth,
  ctx: Context<{ Variables: MetadataVariables }>,
  principal: Principal,
  errorCode: string,
): Promise<string | Response> {
  const userId = await resolveEffectiveUserId(auth, principal);
  if (!userId) {
    return ctx.json({ error: errorCode }, 403);
  }
  return userId;
}

export async function resolveEmailsByUserId(
  auth: CoreAuth,
  userIds: Iterable<string | null | undefined>,
): Promise<Map<string, string>> {
  const ids = [...new Set([...userIds].filter((id): id is string => Boolean(id)))];
  if (ids.length === 0) {
    return new Map();
  }
  const rows = await auth.db
    .select({ id: auth.schema.user.id, email: auth.schema.user.email })
    .from(auth.schema.user)
    .where(inArray(auth.schema.user.id, ids));
  return new Map(rows.map((row: { id: string; email: string }) => [row.id, row.email]));
}

export async function assertGrant(
  auth: CoreAuth,
  nodeId: string,
  principal: Principal,
  require: "read" | "write",
): Promise<Response | undefined> {
  const grant = await checkGrant(auth.db, nodeId, principal, require, auth.schema);
  if (!grant.granted) {
    return Response.json({ error: grant.reason }, { status: 403 });
  }
  return undefined;
}

export function toIso(value: Date | number | string | null): string | null {
  if (value === null) {
    return null;
  }
  if (value instanceof Date) {
    return value.toISOString();
  }
  if (typeof value === "number") {
    return new Date(value).toISOString();
  }
  return new Date(value).toISOString();
}

export { and, desc, eq, gt, inArray };
