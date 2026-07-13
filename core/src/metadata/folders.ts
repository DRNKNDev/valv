import { Hono } from "hono";
import { or } from "drizzle-orm";

import { type CoreAuth } from "../auth/index.js";
import { checkGrant } from "./authz.js";
import {
  type MetadataVariables,
  eq,
  getFolderRoot,
  inTransaction,
  newId,
  requirePrincipal,
  requireUserBackedPrincipal,
  resolveDeviceUserId,
  resolveEffectiveUserId,
  resolveEmailsByUserId,
  toIso,
} from "./common.js";

type FolderRouteStore = {
  createFolderForRoute?: (opts: {
    folderId: string;
    rootNodeId: string;
    grantId: string;
    name: string;
    ownerUserId: string;
  }) => Promise<void>;
  listGrantsForRoute?: (principal: { type: "user"; userId: string } | { type: "device"; deviceId: string }) => Promise<unknown[]>;
  listFolderGrantsForRoute?: (folderId: string) => Promise<unknown[]>;
  getDeviceUserIdForRoute?: (deviceId: string) => Promise<string | undefined>;
  getFolderForRoute?: (folderId: string) => Promise<{ name: string } | undefined>;
};

export type OnFolderCreated = (info: { folderId: string; ownerUserId: string; grantId: string }) => Promise<void>;

export function registerFolderRoutes(
  router: Hono<{ Variables: MetadataVariables }>,
  auth: CoreAuth,
  onFolderCreated?: OnFolderCreated,
): void {
  router.post("/folders", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const ownerUserId = await resolveEffectiveUserId(auth, principal);
    if (!ownerUserId) {
      return ctx.json({ error: "agent_devices_cannot_create_folders" }, 403);
    }

    const body = await ctx.req.json().catch(() => ({}));
    const folderId = newId();
    const rootNodeId = newId();
    const grantId = newId();
    const name = typeof body.name === "string" && body.name.length > 0 ? body.name : "Untitled Folder";

    if (hasFolderRouteStore(auth.db) && auth.db.createFolderForRoute) {
      await auth.db.createFolderForRoute({ folderId, rootNodeId, grantId, name, ownerUserId });
    } else {
      await inTransaction(auth, async (tx) => {
        await tx.insert(auth.schema.sharedFolders).values({
          folderId,
          name,
          ownerUserId,
        });
        await tx.insert(auth.schema.nodes).values({
          nodeId: rootNodeId,
          folderId,
          parentId: null,
          name: "",
          type: "folder",
          serverSeq: 0,
        });
        await tx.insert(auth.schema.folderGrants).values({
          grantId,
          folderId,
          scopeNodeId: rootNodeId,
          userId: ownerUserId,
          deviceId: null,
          role: "owner",
          canRead: true,
          canWrite: true,
          createdByUserId: ownerUserId,
        });
      });
    }

    if (onFolderCreated) {
      try {
        await onFolderCreated({ folderId, ownerUserId, grantId });
      } catch (error) {
        console.error("onFolderCreated hook failed", error);
      }
    }

    return ctx.json({ folder_id: folderId });
  });

  router.get("/grants", async (ctx) => {
    const principal = requirePrincipal(ctx);
    if (hasFolderRouteStore(auth.db) && auth.db.listGrantsForRoute) {
      return ctx.json(await auth.db.listGrantsForRoute(principal));
    }
    const deviceUserId = principal.type === "device" ? await resolveDeviceUserId(auth, principal.deviceId) : undefined;
    const principalCondition =
      principal.type === "user"
        ? eq(auth.schema.folderGrants.userId, principal.userId)
        : deviceUserId
          ? or(eq(auth.schema.folderGrants.deviceId, principal.deviceId), eq(auth.schema.folderGrants.userId, deviceUserId))
          : eq(auth.schema.folderGrants.deviceId, principal.deviceId);
    const rows = await auth.db
      .select({
        grant_id: auth.schema.folderGrants.grantId,
        folder_id: auth.schema.folderGrants.folderId,
        scope_node_id: auth.schema.folderGrants.scopeNodeId,
        role: auth.schema.folderGrants.role,
        can_read: auth.schema.folderGrants.canRead,
        can_write: auth.schema.folderGrants.canWrite,
        user_id: auth.schema.folderGrants.userId,
        device_id: auth.schema.folderGrants.deviceId,
        name: auth.schema.folderGrants.name,
        grantee_email: auth.schema.user.email,
        device_name: auth.schema.devices.name,
        folder_name: auth.schema.sharedFolders.name,
      })
      .from(auth.schema.folderGrants)
      .leftJoin(auth.schema.user, eq(auth.schema.user.id, auth.schema.folderGrants.userId))
      .leftJoin(auth.schema.devices, eq(auth.schema.devices.deviceId, auth.schema.folderGrants.deviceId))
      .leftJoin(auth.schema.sharedFolders, eq(auth.schema.sharedFolders.folderId, auth.schema.folderGrants.folderId))
      .where(principalCondition);
    return ctx.json(rows);
  });

  router.get("/folders/:id/grants", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const folderId = ctx.req.param("id");
    const rootNodeId = await getFolderRoot(auth, folderId);
    if (!rootNodeId) {
      return ctx.json({ error: "folder_not_found" }, 404);
    }

    const ownerCheck = await requireUserBackedPrincipal(auth, ctx, principal, "access_key_cannot_list_grants");
    if (ownerCheck instanceof Response) {
      return ownerCheck;
    }

    const grant = await checkGrant(auth.db, rootNodeId, principal, "write", auth.schema);
    if (!grant.granted) {
      return ctx.json({ error: grant.reason }, 403);
    }

    if (hasFolderRouteStore(auth.db) && auth.db.listFolderGrantsForRoute) {
      return ctx.json(await auth.db.listFolderGrantsForRoute(folderId));
    }

    const rows = await auth.db
      .select({
        grant_id: auth.schema.folderGrants.grantId,
        folder_id: auth.schema.folderGrants.folderId,
        scope_node_id: auth.schema.folderGrants.scopeNodeId,
        role: auth.schema.folderGrants.role,
        can_read: auth.schema.folderGrants.canRead,
        can_write: auth.schema.folderGrants.canWrite,
        user_id: auth.schema.folderGrants.userId,
        device_id: auth.schema.folderGrants.deviceId,
        name: auth.schema.folderGrants.name,
        grantee_email: auth.schema.user.email,
        device_name: auth.schema.devices.name,
        created_by_user_id: auth.schema.folderGrants.createdByUserId,
        created_at: auth.schema.folderGrants.createdAt,
      })
      .from(auth.schema.folderGrants)
      .leftJoin(auth.schema.user, eq(auth.schema.user.id, auth.schema.folderGrants.userId))
      .leftJoin(auth.schema.devices, eq(auth.schema.devices.deviceId, auth.schema.folderGrants.deviceId))
      .where(eq(auth.schema.folderGrants.folderId, folderId));

    const creatorEmails = await resolveEmailsByUserId(
      auth,
      rows.map((row: { created_by_user_id: string | null }) => row.created_by_user_id),
    );
    return ctx.json(
      rows.map((row: { created_by_user_id: string | null; created_at: Date }) => ({
        ...row,
        created_at: toIso(row.created_at),
        created_by_email: row.created_by_user_id ? (creatorEmails.get(row.created_by_user_id) ?? null) : null,
      })),
    );
  });

  router.get("/folders/:id", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const folderId = ctx.req.param("id");

    const rootNodeId = await getFolderRoot(auth, folderId);
    if (!rootNodeId) {
      return ctx.json({ error: "folder_not_found" }, 404);
    }

    const grant = await checkGrant(auth.db, rootNodeId, principal, "read", auth.schema);
    if (!grant.granted) {
      return ctx.json({ error: grant.reason }, 403);
    }

    const folder =
      hasFolderRouteStore(auth.db) && auth.db.getFolderForRoute
        ? await auth.db.getFolderForRoute(folderId)
        : (
            await auth.db
              .select({ name: auth.schema.sharedFolders.name })
              .from(auth.schema.sharedFolders)
              .where(eq(auth.schema.sharedFolders.folderId, folderId))
              .limit(1)
          )[0];
    if (!folder) {
      return ctx.json({ error: "folder_not_found" }, 404);
    }

    return ctx.json({ folder_id: folderId, name: folder.name });
  });
}

function hasFolderRouteStore(db: CoreAuth["db"]): db is CoreAuth["db"] & FolderRouteStore {
  return (
    "createFolderForRoute" in db ||
    "listGrantsForRoute" in db ||
    "listFolderGrantsForRoute" in db ||
    "getFolderForRoute" in db
  );
}
