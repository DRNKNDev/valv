import { Hono } from "hono";

import type { CoreAuth, Principal } from "../auth/index.js";
import { eq, getFolderRoot, requirePrincipal, resolveDeviceUserId, type MetadataVariables } from "./common.js";

export type PrincipalScope = { folder_id: string; folder_name: string; scope_label: string; can_write: boolean };
export type PrincipalStatus = { type: "account" | "access_key"; email?: string; scopes: PrincipalScope[] };

type MeRouteStore = {
  resolvePrincipalStatusForRoute?: (principal: Principal) => Promise<PrincipalStatus>;
};

export function registerMeRoutes(router: Hono<{ Variables: MetadataVariables }>, auth: CoreAuth): void {
  router.get("/me", async (ctx) => {
    const principal = requirePrincipal(ctx);

    if (hasMeRouteStore(auth.db) && auth.db.resolvePrincipalStatusForRoute) {
      return ctx.json(await auth.db.resolvePrincipalStatusForRoute(principal));
    }

    if (principal.type === "user") {
      return ctx.json(await accountStatus(auth, principal.userId));
    }

    const deviceUserId = await resolveDeviceUserId(auth, principal.deviceId);
    if (deviceUserId) {
      return ctx.json(await accountStatus(auth, deviceUserId));
    }

    return ctx.json(await accessKeyStatus(auth, principal.deviceId));
  });
}

async function accountStatus(auth: CoreAuth, userId: string): Promise<PrincipalStatus> {
  const rows = await auth.db.select({ email: auth.schema.user.email }).from(auth.schema.user).where(eq(auth.schema.user.id, userId)).limit(1);
  return { type: "account", email: rows[0]?.email, scopes: [] };
}

async function accessKeyStatus(auth: CoreAuth, deviceId: string): Promise<PrincipalStatus> {
  const grantRows = await auth.db
    .select({
      folderId: auth.schema.folderGrants.folderId,
      scopeNodeId: auth.schema.folderGrants.scopeNodeId,
      canWrite: auth.schema.folderGrants.canWrite,
    })
    .from(auth.schema.folderGrants)
    .where(eq(auth.schema.folderGrants.deviceId, deviceId));

  const scopes: PrincipalScope[] = [];
  for (const row of grantRows as Array<{ folderId: string; scopeNodeId: string; canWrite: boolean }>) {
    const folderRows = await auth.db
      .select({ name: auth.schema.sharedFolders.name })
      .from(auth.schema.sharedFolders)
      .where(eq(auth.schema.sharedFolders.folderId, row.folderId))
      .limit(1);
    const folderName = folderRows[0]?.name ?? "";
    const rootNodeId = await getFolderRoot(auth, row.folderId);
    let scopeLabel = folderName;
    if (row.scopeNodeId !== rootNodeId) {
      const nodeRows = await auth.db
        .select({ name: auth.schema.nodes.name })
        .from(auth.schema.nodes)
        .where(eq(auth.schema.nodes.nodeId, row.scopeNodeId))
        .limit(1);
      scopeLabel = nodeRows[0]?.name ?? folderName;
    }
    scopes.push({ folder_id: row.folderId, folder_name: folderName, scope_label: scopeLabel, can_write: row.canWrite });
  }

  return { type: "access_key", scopes };
}

function hasMeRouteStore(db: CoreAuth["db"]): db is CoreAuth["db"] & MeRouteStore {
  return "resolvePrincipalStatusForRoute" in db;
}
