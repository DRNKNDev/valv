import { and, eq, or } from "drizzle-orm";

import type { CoreDb, CoreSchema, Principal } from "../auth/index.js";
import { pgSchema } from "../db/schema.js";

export const MAX_GRANT_WALK_DEPTH = 50;

export type GrantRequirement = "read" | "write";

export type AuthzResult =
  | { granted: true; grantId: string; scopeNodeId: string; canWrite: boolean }
  | { granted: false; reason: "no_grant" | "insufficient_permission" };

type AuthzNode = { nodeId: string; folderId: string; parentId: string | null };
type AuthzGrant = { grantId: string; scopeNodeId: string; canRead: boolean; canWrite: boolean };

type AuthzLoaderDb = {
  getNodeForAuthz: (nodeId: string) => Promise<AuthzNode | undefined>;
  getGrantForAuthz: (opts: {
    folderId: string;
    scopeNodeId: string;
    principal: Principal;
  }) => Promise<AuthzGrant | undefined>;
  getDeviceUserIdForAuthz?: (deviceId: string) => Promise<string | undefined>;
};

export async function checkGrant(
  db: CoreDb,
  nodeId: string,
  principal: Principal,
  require: GrantRequirement,
  schema: CoreSchema = pgSchema,
): Promise<AuthzResult> {
  const target = await loadNode(db, nodeId, schema);
  if (!target) {
    return { granted: false, reason: "no_grant" };
  }

  let currentId: string | null = target.nodeId;
  let parentId: string | null = target.parentId;

  for (let depth = 0; currentId && depth < MAX_GRANT_WALK_DEPTH; depth += 1) {
    const grant = await loadGrant(db, target.folderId, currentId, principal, schema);

    if (grant) {
      if (require === "write" && !grant.canWrite) {
        return { granted: false, reason: "insufficient_permission" };
      }
      if (require === "read" && !grant.canRead && !grant.canWrite) {
        return { granted: false, reason: "insufficient_permission" };
      }
      return {
        granted: true,
        grantId: grant.grantId,
        scopeNodeId: grant.scopeNodeId,
        canWrite: grant.canWrite,
      };
    }

    if (!parentId) {
      break;
    }
    const parent = await loadNode(db, parentId, schema);
    currentId = parent?.nodeId ?? null;
    parentId = parent?.parentId ?? null;
  }

  return { granted: false, reason: "no_grant" };
}

async function loadNode(db: CoreDb, nodeId: string, schema: CoreSchema): Promise<AuthzNode | undefined> {
  if (hasAuthzLoaders(db)) {
    return db.getNodeForAuthz(nodeId);
  }

  const rows = await db
    .select({ nodeId: schema.nodes.nodeId, folderId: schema.nodes.folderId, parentId: schema.nodes.parentId })
    .from(schema.nodes)
    .where(eq(schema.nodes.nodeId, nodeId))
    .limit(1);
  return rows[0];
}

async function loadGrant(
  db: CoreDb,
  folderId: string,
  scopeNodeId: string,
  principal: Principal,
  schema: CoreSchema,
): Promise<AuthzGrant | undefined> {
  if (hasAuthzLoaders(db)) {
    return db.getGrantForAuthz({ folderId, scopeNodeId, principal });
  }

  const deviceUserId = principal.type === "device" ? await loadDeviceUserId(db, principal.deviceId, schema) : undefined;
  const principalCondition =
    principal.type === "user"
      ? eq(schema.folderGrants.userId, principal.userId)
      : deviceUserId
        ? or(eq(schema.folderGrants.deviceId, principal.deviceId), eq(schema.folderGrants.userId, deviceUserId))
        : eq(schema.folderGrants.deviceId, principal.deviceId);

  const rows = await db
    .select({
      grantId: schema.folderGrants.grantId,
      scopeNodeId: schema.folderGrants.scopeNodeId,
      canRead: schema.folderGrants.canRead,
      canWrite: schema.folderGrants.canWrite,
    })
    .from(schema.folderGrants)
    .where(
      and(
        eq(schema.folderGrants.scopeNodeId, scopeNodeId),
        eq(schema.folderGrants.folderId, folderId),
        principalCondition,
      ),
    )
    .limit(1);
  return rows[0];
}

async function loadDeviceUserId(db: CoreDb, deviceId: string, schema: CoreSchema): Promise<string | undefined> {
  if (hasAuthzLoaders(db) && db.getDeviceUserIdForAuthz) {
    return db.getDeviceUserIdForAuthz(deviceId);
  }

  const rows = await db
    .select({ userId: schema.devices.userId })
    .from(schema.devices)
    .where(eq(schema.devices.deviceId, deviceId))
    .limit(1);
  return rows[0]?.userId ?? undefined;
}

function hasAuthzLoaders(db: CoreDb): db is CoreDb & AuthzLoaderDb {
  return "getNodeForAuthz" in db && "getGrantForAuthz" in db;
}
