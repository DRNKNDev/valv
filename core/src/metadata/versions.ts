import { Hono } from "hono";

import type { CoreAuth } from "../auth/index.js";
import { checkGrant } from "./authz.js";
import { desc, eq, requirePrincipal, toIso, type MetadataHub, type MetadataVariables } from "./common.js";
import { submitOp } from "./ops.js";

export function registerVersionRoutes(
  router: Hono<{ Variables: MetadataVariables }>,
  auth: CoreAuth,
  hub: MetadataHub,
): void {
  const listVersions = async (ctx: any) => {
    const principal = requirePrincipal(ctx);
    const nodeId = ctx.req.param("nodeId");
    const grant = await checkGrant(auth.db, nodeId, principal, "read", auth.schema);
    if (!grant.granted) {
      return ctx.json({ error: grant.reason }, 403);
    }

    const rows = await auth.db
      .select()
      .from(auth.schema.versions)
      .where(eq(auth.schema.versions.nodeId, nodeId))
      .orderBy(desc(auth.schema.versions.createdAt));

    return ctx.json(
      rows.map((version: any) => ({
        version_id: version.versionId,
        content_hash: version.contentHash,
        size_bytes: version.sizeBytes,
        manifest: version.manifest,
        author_device_id: version.authorDeviceId,
        created_at: toIso(version.createdAt),
        is_conflict_copy: version.isConflictCopy,
      })),
    );
  };

  router.get("/folders/:id/nodes/:nodeId/versions", listVersions);
  router.get("/folders/:id/versions/:nodeId", listVersions);

  router.post("/folders/:id/nodes/:nodeId/versions/:versionId/restore", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const folderId = ctx.req.param("id");
    const nodeId = ctx.req.param("nodeId");
    const versionId = ctx.req.param("versionId");
    const grant = await checkGrant(auth.db, nodeId, principal, "write", auth.schema);
    if (!grant.granted) {
      return ctx.json({ error: grant.reason }, 403);
    }

    const versions = await auth.db
      .select()
      .from(auth.schema.versions)
      .where(eq(auth.schema.versions.versionId, versionId))
      .limit(1);
    const version = versions[0];
    if (!version || version.nodeId !== nodeId) {
      return ctx.json({ error: "version_not_found" }, 404);
    }

    const nodes = await auth.db
      .select({ serverSeq: auth.schema.nodes.serverSeq })
      .from(auth.schema.nodes)
      .where(eq(auth.schema.nodes.nodeId, nodeId))
      .limit(1);
    const node = nodes[0];
    if (!node) {
      return ctx.json({ error: "node_not_found" }, 404);
    }

    const response = await submitOp(auth, hub, folderId, principal, {
      op_type: "new_version",
      node_id: nodeId,
      based_on_seq: node.serverSeq,
      payload: {
        version_id: crypto.randomUUID(),
        content_hash: version.contentHash,
        size_bytes: version.sizeBytes,
        manifest: version.manifest as any,
      },
    });
    return ctx.json(response);
  });
}
