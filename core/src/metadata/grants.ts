import { Hono } from "hono";

import {
  generateDeviceToken,
  sha256Hex,
  type CoreAuth,
} from "../auth/index.js";
import { checkGrant } from "./authz.js";
import {
  eq,
  getFolderRoot,
  inTransaction,
  newId,
  requirePrincipal,
  requireUserBackedPrincipal,
  type MetadataVariables,
} from "./common.js";

const FOLDER_GRANT_NAME_UNIQUE_CONSTRAINT = "folder_grants_folder_name_unique";

function isDuplicateGrantNameError(error: unknown): boolean {
  if (!(error instanceof Error)) {
    return false;
  }
  const withCode = error as Error & { code?: string; constraint?: string };
  if (withCode.code === "23505") {
    return withCode.constraint === undefined || withCode.constraint === FOLDER_GRANT_NAME_UNIQUE_CONSTRAINT;
  }
  if (withCode.code === "SQLITE_CONSTRAINT_UNIQUE" || withCode.code === "SQLITE_CONSTRAINT") {
    return error.message.includes("folder_grants");
  }
  return error.message.includes(FOLDER_GRANT_NAME_UNIQUE_CONSTRAINT);
}

type RegeneratedGrant = { folderId: string; scopeNodeId: string; name: string | null; canRead: boolean; canWrite: boolean };
type OnBeforeCommit = (info: { folderId: string; scopeNodeId: string; deviceId: string; grantId: string }) => Promise<void>;

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
    createdByUserId: string | undefined;
  }) => Promise<void>;
  getGrantForRoute?: (grantId: string) => Promise<{ scopeNodeId: string; deviceId: string | null } | undefined>;
  getGrantScopeForRoute?: (grantId: string) => Promise<string | undefined>;
  deleteGrantForRoute?: (grantId: string) => Promise<void>;
  regenerateGrantForRoute?: (opts: {
    oldGrantId: string;
    newDeviceId: string;
    newGrantId: string;
    tokenHash: string;
    createdByUserId: string | undefined;
    onBeforeCommit?: OnBeforeCommit;
  }) => Promise<RegeneratedGrant | undefined>;
};

export type OnGrantCreated = (info: { folderId: string; grantId: string; deviceId: string }) => Promise<void>;

export type GrantRouteOptions = {
  onGrantCreated?: OnGrantCreated;
  onGrantDeviceCreated?: (info: { folderId: string; scopeNodeId: string; deviceId: string; grantId: string }) => Promise<void>;
  checkPlan?: (folderId: string) => Promise<{ allowed: boolean; status?: string } | null>;
};

export function registerGrantRoutes(
  router: Hono<{ Variables: MetadataVariables }>,
  auth: CoreAuth,
  opts: GrantRouteOptions = {},
): void {
  router.post("/folders/:id/grants", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const folderId = ctx.req.param("id");
    const body = await ctx.req.json().catch(() => ({}));
    if (body.scope_node_id !== undefined && typeof body.scope_node_id !== "string") {
      return ctx.json({ error: "invalid_scope_node_id" }, 400);
    }
    const scopeNodeId = typeof body.scope_node_id === "string" ? body.scope_node_id : await getFolderRoot(auth, folderId);
    if (!scopeNodeId) {
      return ctx.json({ error: "folder_not_found" }, 404);
    }

    const ownerCheck = await requireUserBackedPrincipal(auth, ctx, principal, "access_key_cannot_issue_keys");
    if (ownerCheck instanceof Response) {
      return ownerCheck;
    }
    const createdByUserId = ownerCheck;

    const grant = await checkGrant(auth.db, scopeNodeId, principal, "write", auth.schema);
    if (!grant.granted) {
      return ctx.json({ error: grant.reason }, 403);
    }

    const plan = opts.checkPlan ? await opts.checkPlan(folderId) : null;
    if (plan?.allowed === false) {
      const responseBody: { error: "subscription_inactive"; status?: string } = { error: "subscription_inactive" };
      if (plan.status !== undefined) {
        responseBody.status = plan.status;
      }
      return ctx.json(responseBody, 402);
    }

    const rawToken = generateDeviceToken();
    const deviceId = newId();
    const grantId = newId();
    const name = typeof body.name === "string" ? body.name : "Agent";
    try {
      if (hasGrantRouteStore(auth.db) && auth.db.createAgentGrantForRoute) {
        await auth.db.createAgentGrantForRoute({
          folderId,
          scopeNodeId,
          deviceId,
          grantId,
          rawToken,
          tokenHash: sha256Hex(rawToken),
          name,
          canRead: body.can_read !== false,
          canWrite: body.can_write !== false,
          createdByUserId,
        });
      } else {
        await inTransaction(auth, async (tx) => {
        await tx.insert(auth.schema.devices).values({
          deviceId,
          userId: null,
          name,
          tokenHash: sha256Hex(rawToken),
        });
        await tx.insert(auth.schema.folderGrants).values({
          grantId,
          folderId,
          scopeNodeId,
          userId: null,
          deviceId,
          name,
          role: "collaborator",
          canRead: body.can_read !== false,
          canWrite: body.can_write !== false,
          createdByUserId,
        });
        });
      }
    } catch (error) {
      if (isDuplicateGrantNameError(error)) {
        return ctx.json({ error: "access_key_name_taken" }, 409);
      }
      throw error;
    }

    if (opts.onGrantCreated) {
      try {
        await opts.onGrantCreated({ folderId, grantId, deviceId });
      } catch (error) {
        console.error("onGrantCreated hook failed", error);
      }
    }

    if (opts.onGrantDeviceCreated) {
      try {
        await opts.onGrantDeviceCreated({ folderId, scopeNodeId, deviceId, grantId });
      } catch (error) {
        console.error("onGrantDeviceCreated hook failed", error);
      }
    }

    return ctx.json({ grant_id: grantId, device_id: deviceId, token: rawToken });
  });

  router.delete("/folders/:id/grants/:grantId", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const grantId = ctx.req.param("grantId");

    const ownerCheck = await requireUserBackedPrincipal(auth, ctx, principal, "access_key_cannot_revoke");
    if (ownerCheck instanceof Response) {
      return ownerCheck;
    }

    const routeGrantStore = hasGrantRouteStore(auth.db) ? auth.db : undefined;
    const hasGrantLoader = routeGrantStore?.getGrantForRoute !== undefined;
    const routeGrant = hasGrantLoader ? await routeGrantStore.getGrantForRoute?.(grantId) : undefined;
    if (!hasGrantLoader && routeGrantStore?.getGrantScopeForRoute) {
      return ctx.json({ error: "incomplete_grant_route_store" }, 500);
    }
    const grants = hasGrantLoader
      ? routeGrant
        ? [routeGrant]
        : []
      : await auth.db
          .select({ scopeNodeId: auth.schema.folderGrants.scopeNodeId, deviceId: auth.schema.folderGrants.deviceId })
          .from(auth.schema.folderGrants)
          .where(eq(auth.schema.folderGrants.grantId, grantId))
          .limit(1);
    const target = grants[0];
    if (!target) {
      return ctx.json({ error: "grant_not_found" }, 404);
    }

    const grant = await checkGrant(auth.db, target.scopeNodeId, principal, "write", auth.schema);
    if (!grant.granted) {
      return ctx.json({ error: grant.reason }, 403);
    }

    if (hasGrantRouteStore(auth.db) && auth.db.deleteGrantForRoute) {
      await auth.db.deleteGrantForRoute(grantId);
    } else {
      await auth.db.delete(auth.schema.folderGrants).where(eq(auth.schema.folderGrants.grantId, grantId));
    }
    if (target.deviceId) {
      await auth.db
        .update(auth.schema.devices)
        .set({ tokenHash: `revoked:${grantId}` })
        .where(eq(auth.schema.devices.deviceId, target.deviceId));
    }
    return ctx.body(null, 204);
  });

  router.post("/folders/:id/grants/:grantId/regenerate", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const grantId = ctx.req.param("grantId");

    const ownerCheck = await requireUserBackedPrincipal(auth, ctx, principal, "access_key_cannot_issue_keys");
    if (ownerCheck instanceof Response) {
      return ownerCheck;
    }
    const createdByUserId = ownerCheck;

    const routeGrantStore = hasGrantRouteStore(auth.db) ? auth.db : undefined;
    const routeGrant = routeGrantStore?.getGrantForRoute ? await routeGrantStore.getGrantForRoute(grantId) : undefined;
    const grants = routeGrantStore?.getGrantForRoute
      ? routeGrant
        ? [routeGrant]
        : []
      : await auth.db
          .select({ scopeNodeId: auth.schema.folderGrants.scopeNodeId, deviceId: auth.schema.folderGrants.deviceId })
          .from(auth.schema.folderGrants)
          .where(eq(auth.schema.folderGrants.grantId, grantId))
          .limit(1);
    const target = grants[0];
    if (!target) {
      return ctx.json({ error: "grant_not_found" }, 404);
    }

    const grant = await checkGrant(auth.db, target.scopeNodeId, principal, "write", auth.schema);
    if (!grant.granted) {
      return ctx.json({ error: grant.reason }, 403);
    }
    if (!target.deviceId) {
      return ctx.json({ error: "grant_has_no_token" }, 400);
    }

    const rawToken = generateDeviceToken();
    const newDeviceId = newId();
    const newGrantId = newId();
    const onBeforeCommit: OnBeforeCommit | undefined = opts.onGrantDeviceCreated;

    let replacement: RegeneratedGrant | undefined;
    if (hasGrantRouteStore(auth.db) && auth.db.regenerateGrantForRoute) {
      replacement = await auth.db.regenerateGrantForRoute({
        oldGrantId: grantId,
        newDeviceId,
        newGrantId,
        tokenHash: sha256Hex(rawToken),
        createdByUserId,
        onBeforeCommit,
      });
    } else {
      replacement = await inTransaction(auth, async (tx) => {
        const rows = await tx
          .select({
            folderId: auth.schema.folderGrants.folderId,
            scopeNodeId: auth.schema.folderGrants.scopeNodeId,
            deviceId: auth.schema.folderGrants.deviceId,
            name: auth.schema.folderGrants.name,
            canRead: auth.schema.folderGrants.canRead,
            canWrite: auth.schema.folderGrants.canWrite,
          })
          .from(auth.schema.folderGrants)
          .where(eq(auth.schema.folderGrants.grantId, grantId))
          .limit(1);
        const old = rows[0];
        if (!old || !old.deviceId) {
          return undefined;
        }
        const deviceRows = await tx
          .select({ name: auth.schema.devices.name })
          .from(auth.schema.devices)
          .where(eq(auth.schema.devices.deviceId, old.deviceId))
          .limit(1);
        const deviceName = deviceRows[0]?.name ?? "Agent";

        await tx.delete(auth.schema.folderGrants).where(eq(auth.schema.folderGrants.grantId, grantId));
        await tx
          .update(auth.schema.devices)
          .set({ tokenHash: `revoked:${grantId}` })
          .where(eq(auth.schema.devices.deviceId, old.deviceId));
        await tx.insert(auth.schema.devices).values({
          deviceId: newDeviceId,
          userId: null,
          name: deviceName,
          tokenHash: sha256Hex(rawToken),
        });
        await tx.insert(auth.schema.folderGrants).values({
          grantId: newGrantId,
          folderId: old.folderId,
          scopeNodeId: old.scopeNodeId,
          userId: null,
          deviceId: newDeviceId,
          name: old.name,
          role: "collaborator",
          canRead: old.canRead,
          canWrite: old.canWrite,
          createdByUserId,
        });

        if (onBeforeCommit) {
          await onBeforeCommit({ folderId: old.folderId, scopeNodeId: old.scopeNodeId, deviceId: newDeviceId, grantId: newGrantId });
        }

        return { folderId: old.folderId, scopeNodeId: old.scopeNodeId, name: old.name, canRead: old.canRead, canWrite: old.canWrite };
      }, { atomicOnSqlite: true });
    }

    if (!replacement) {
      return ctx.json({ error: "grant_has_no_token" }, 400);
    }

    return ctx.json({ grant_id: newGrantId, device_id: newDeviceId, token: rawToken });
  });
}

function hasGrantRouteStore(db: CoreAuth["db"]): db is CoreAuth["db"] & GrantRouteStore {
  return (
    "createAgentGrantForRoute" in db ||
    "getGrantForRoute" in db ||
    "getGrantScopeForRoute" in db ||
    "deleteGrantForRoute" in db ||
    "regenerateGrantForRoute" in db
  );
}
