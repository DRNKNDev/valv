import { Hono } from "hono";

import {
  generateDeviceToken,
  sha256Hex,
  type CoreAuth,
} from "../auth/index.js";
import { checkGrant } from "./authz.js";
import { eq, inTransaction, newId, requirePrincipal, type MetadataVariables } from "./common.js";

type GrantRouteStore = {
  createAgentGrantForRoute?: (opts: {
    folderId: string;
    scopeNodeId: string;
    deviceId: string;
    grantId: string;
    rawToken: string;
    tokenHash: string;
    name: string;
    canRead: boolean;
    canWrite: boolean;
  }) => Promise<void>;
  getGrantScopeForRoute?: (grantId: string) => Promise<string | undefined>;
  deleteGrantForRoute?: (grantId: string) => Promise<void>;
};

export function registerGrantRoutes(router: Hono<{ Variables: MetadataVariables }>, auth: CoreAuth): void {
  router.post("/folders/:id/grants", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const folderId = ctx.req.param("id");
    const body = await ctx.req.json().catch(() => ({}));
    const scopeNodeId = body.scope_node_id;
    if (typeof scopeNodeId !== "string") {
      return ctx.json({ error: "invalid_scope_node_id" }, 400);
    }

    const grant = await checkGrant(auth.db, scopeNodeId, principal, "read", auth.schema);
    if (!grant.granted) {
      return ctx.json({ error: grant.reason }, 403);
    }

    const rawToken = generateDeviceToken();
    const deviceId = newId();
    const grantId = newId();
    if (hasGrantRouteStore(auth.db) && auth.db.createAgentGrantForRoute) {
      await auth.db.createAgentGrantForRoute({
        folderId,
        scopeNodeId,
        deviceId,
        grantId,
        rawToken,
        tokenHash: sha256Hex(rawToken),
        name: typeof body.name === "string" ? body.name : "Agent",
        canRead: body.can_read !== false,
        canWrite: body.can_write !== false,
      });
    } else {
      await inTransaction(auth, async (tx) => {
      await tx.insert(auth.schema.devices).values({
        deviceId,
        userId: null,
        name: typeof body.name === "string" ? body.name : "Agent",
        tokenHash: sha256Hex(rawToken),
      });
      await tx.insert(auth.schema.folderGrants).values({
        grantId,
        folderId,
        scopeNodeId,
        userId: null,
        deviceId,
        role: "collaborator",
        canRead: body.can_read !== false,
        canWrite: body.can_write !== false,
      });
      });
    }

    return ctx.json({ grant_id: grantId, device_id: deviceId, token: rawToken });
  });

  router.delete("/folders/:id/grants/:grantId", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const grantId = ctx.req.param("grantId");
    const routeScope = hasGrantRouteStore(auth.db) && auth.db.getGrantScopeForRoute
      ? await auth.db.getGrantScopeForRoute(grantId)
      : undefined;
    const grants = routeScope ? [{ scopeNodeId: routeScope, deviceId: undefined }] : await auth.db
      .select({ scopeNodeId: auth.schema.folderGrants.scopeNodeId, deviceId: auth.schema.folderGrants.deviceId })
      .from(auth.schema.folderGrants)
      .where(eq(auth.schema.folderGrants.grantId, grantId))
      .limit(1);
    const target = grants[0];
    if (!target) {
      return ctx.json({ error: "grant_not_found" }, 404);
    }

    const grant = await checkGrant(auth.db, target.scopeNodeId, principal, "read", auth.schema);
    if (!grant.granted) {
      return ctx.json({ error: grant.reason }, 403);
    }

    if (hasGrantRouteStore(auth.db) && auth.db.deleteGrantForRoute) {
      await auth.db.deleteGrantForRoute(grantId);
    } else {
      await auth.db.delete(auth.schema.folderGrants).where(eq(auth.schema.folderGrants.grantId, grantId));
      if (target.deviceId) {
        await auth.db
          .update(auth.schema.devices)
          .set({ tokenHash: `revoked:${grantId}` })
          .where(eq(auth.schema.devices.deviceId, target.deviceId));
      }
    }
    return ctx.body(null, 204);
  });
}

function hasGrantRouteStore(db: CoreAuth["db"]): db is CoreAuth["db"] & GrantRouteStore {
  return "createAgentGrantForRoute" in db || "getGrantScopeForRoute" in db || "deleteGrantForRoute" in db;
}
