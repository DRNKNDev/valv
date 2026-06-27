import { Hono } from "hono";

import { type CoreAuth } from "../auth/index.js";
import { type MetadataVariables, eq, inTransaction, newId, requirePrincipal } from "./common.js";

type FolderRouteStore = {
  createFolderForRoute?: (opts: {
    folderId: string;
    rootNodeId: string;
    grantId: string;
    name: string;
    ownerUserId: string;
  }) => Promise<void>;
  listGrantsForRoute?: (principal: { type: "user"; userId: string } | { type: "device"; deviceId: string }) => Promise<unknown[]>;
};

export function registerFolderRoutes(router: Hono<{ Variables: MetadataVariables }>, auth: CoreAuth): void {
  router.post("/folders", async (ctx) => {
    const principal = requirePrincipal(ctx);
    if (principal.type !== "user") {
      return ctx.json({ error: "user_required" }, 403);
    }

    const body = await ctx.req.json().catch(() => ({}));
    const folderId = newId();
    const rootNodeId = newId();
    const grantId = newId();
    const name = typeof body.name === "string" && body.name.length > 0 ? body.name : "Untitled Folder";

    if (hasFolderRouteStore(auth.db) && auth.db.createFolderForRoute) {
      await auth.db.createFolderForRoute({ folderId, rootNodeId, grantId, name, ownerUserId: principal.userId });
    } else {
      await inTransaction(auth, async (tx) => {
      await tx.insert(auth.schema.sharedFolders).values({
        folderId,
        name,
        ownerUserId: principal.userId,
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
        userId: principal.userId,
        deviceId: null,
        role: "owner",
        canRead: true,
        canWrite: true,
      });
      });
    }

    return ctx.json({ folder_id: folderId });
  });

  router.get("/grants", async (ctx) => {
    const principal = requirePrincipal(ctx);
    if (hasFolderRouteStore(auth.db) && auth.db.listGrantsForRoute) {
      return ctx.json(await auth.db.listGrantsForRoute(principal));
    }
    const rows = await auth.db
      .select({
        grant_id: auth.schema.folderGrants.grantId,
        folder_id: auth.schema.folderGrants.folderId,
        scope_node_id: auth.schema.folderGrants.scopeNodeId,
        role: auth.schema.folderGrants.role,
        can_read: auth.schema.folderGrants.canRead,
        can_write: auth.schema.folderGrants.canWrite,
      })
      .from(auth.schema.folderGrants)
      .where(
        principal.type === "user"
          ? eq(auth.schema.folderGrants.userId, principal.userId)
          : eq(auth.schema.folderGrants.deviceId, principal.deviceId),
      );
    return ctx.json(rows);
  });
}

function hasFolderRouteStore(db: CoreAuth["db"]): db is CoreAuth["db"] & FolderRouteStore {
  return "createFolderForRoute" in db || "listGrantsForRoute" in db;
}
