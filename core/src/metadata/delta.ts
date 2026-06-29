import { Hono } from "hono";
import { sql } from "drizzle-orm";

import type { CoreAuth, Principal } from "../auth/index.js";
import { checkGrant } from "./authz.js";
import { getFolderRoot, requirePrincipal, toIso, type MetadataVariables } from "./common.js";

type DeltaOpRow = {
  server_seq: number;
  node_id: string;
  op_type: string;
  op_payload: Record<string, unknown> | string;
  actor_device_id: string;
  applied_at: Date | number | string | null;
};

type TreeNodeRow = {
  node_id: string;
  parent_id: string | null;
  name: string;
  type: "file" | "folder";
  current_version_id: string | null;
  server_seq: number;
  deleted_at: Date | number | string | null;
};

type DeltaStoreDb = {
  getDeltaOpsForScope: (opts: { folderId: string; scopeNodeId: string; since: number; limit: number }) => Promise<DeltaOpRow[]>;
  getTreeNodesForScope: (opts: { folderId: string; scopeNodeId: string }) => Promise<TreeNodeRow[]>;
  getFolderHeadSeqForDelta: (folderId: string) => Promise<number>;
};

export function registerDeltaRoutes(router: Hono<{ Variables: MetadataVariables }>, auth: CoreAuth): void {
  router.get("/folders/:id/ops", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const folderId = ctx.req.param("id");
    const since = Number(ctx.req.query("since") ?? 0);
    return ctx.json(await pullDelta(auth, folderId, principal, since));
  });

  router.get("/folders/:id/tree", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const folderId = ctx.req.param("id");
    return ctx.json(await pullTree(auth, folderId, principal));
  });
}

export async function pullDelta(auth: CoreAuth, folderId: string, principal: Principal, since: number) {
  const rootNodeId = await getFolderRoot(auth, folderId);
  if (!rootNodeId) {
    throw new Response(JSON.stringify({ error: "folder_not_found" }), { status: 404 });
  }
  const grant = await checkGrant(auth.db, rootNodeId, principal, "read", auth.schema);
  if (!grant.granted) {
    throw new Response(JSON.stringify({ error: grant.reason }), { status: 403 });
  }

  const rows = hasDeltaStore(auth.db)
    ? await auth.db.getDeltaOpsForScope({ folderId, scopeNodeId: grant.scopeNodeId, since, limit: 1000 })
    : await executeRows(auth, sql`
        WITH RECURSIVE subtree(node_id) AS (
          SELECT node_id FROM nodes WHERE node_id = ${grant.scopeNodeId}
          UNION ALL
          SELECT n.node_id FROM nodes n INNER JOIN subtree s ON n.parent_id = s.node_id
          WHERE n.deleted_at IS NULL
        )
        SELECT server_seq, node_id, op_type, op_payload, actor_device_id, applied_at
        FROM op_log
        WHERE folder_id = ${folderId}
          AND server_seq > ${since}
          AND node_id IN (SELECT node_id FROM subtree)
        ORDER BY server_seq ASC
        LIMIT 1000
      `);
  const head = await folderHeadSeq(auth, folderId);
  const ops = rows.map((row) => ({
    server_seq: Number(row.server_seq),
    node_id: String(row.node_id),
    op_type: row.op_type,
    op_payload: parseOpPayload(row.op_payload),
    actor_device_id: String(row.actor_device_id),
    applied_at: toIso(row.applied_at),
  }));

  return {
    ops,
    up_to_seq: ops.length > 0 ? ops[ops.length - 1].server_seq : head,
  };
}

export async function pullTree(auth: CoreAuth, folderId: string, principal: Principal) {
  const rootNodeId = await getFolderRoot(auth, folderId);
  if (!rootNodeId) {
    throw new Response(JSON.stringify({ error: "folder_not_found" }), { status: 404 });
  }
  const grant = await checkGrant(auth.db, rootNodeId, principal, "read", auth.schema);
  if (!grant.granted) {
    throw new Response(JSON.stringify({ error: grant.reason }), { status: 403 });
  }

  const rows = hasDeltaStore(auth.db)
    ? await auth.db.getTreeNodesForScope({ folderId, scopeNodeId: grant.scopeNodeId })
    : await executeRows(auth, sql`
        WITH RECURSIVE subtree(node_id) AS (
          SELECT node_id FROM nodes WHERE node_id = ${grant.scopeNodeId}
          UNION ALL
          SELECT n.node_id FROM nodes n INNER JOIN subtree s ON n.parent_id = s.node_id
        )
        SELECT node_id, parent_id, name, type, current_version_id, server_seq, deleted_at
        FROM nodes
        WHERE folder_id = ${folderId}
          AND node_id IN (SELECT node_id FROM subtree)
        ORDER BY server_seq ASC
      `);

  return {
    nodes: rows.map((row) => ({
      node_id: String(row.node_id),
      parent_id: row.node_id === grant.scopeNodeId ? null : (row.parent_id as string | null),
      name: String(row.name),
      type: row.type,
      current_version_id: row.current_version_id as string | null,
      server_seq: Number(row.server_seq),
      deleted_at: toIso(row.deleted_at),
    })),
    up_to_seq: await folderHeadSeq(auth, folderId),
  };
}

async function folderHeadSeq(auth: CoreAuth, folderId: string): Promise<number> {
  if (hasDeltaStore(auth.db)) {
    return auth.db.getFolderHeadSeqForDelta(folderId);
  }

  const rows = await executeRows(auth, sql`
    SELECT COALESCE(MAX(server_seq), 0) AS up_to_seq FROM op_log WHERE folder_id = ${folderId}
  `);
  return Number(rows[0]?.up_to_seq ?? 0);
}

function hasDeltaStore(db: CoreAuth["db"]): db is CoreAuth["db"] & DeltaStoreDb {
  return "getDeltaOpsForScope" in db && "getTreeNodesForScope" in db && "getFolderHeadSeqForDelta" in db;
}

function parseOpPayload(payload: Record<string, unknown> | string): Record<string, unknown> {
  if (typeof payload !== "string") {
    return payload;
  }
  const parsed = JSON.parse(payload) as unknown;
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("op_payload must decode to a JSON object");
  }
  return parsed as Record<string, unknown>;
}

async function executeRows(auth: CoreAuth, query: unknown): Promise<any[]> {
  if (typeof auth.db.all === "function") {
    return auth.db.all(query);
  }
  if (typeof auth.db.execute !== "function") {
    return [];
  }
  const result = await auth.db.execute(query);
  if (Array.isArray(result)) {
    return result;
  }
  if (Array.isArray(result.rows)) {
    return result.rows;
  }
  return [];
}
