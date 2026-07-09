import { Hono } from "hono";
import { eq, inArray, sql } from "drizzle-orm";

import type { CoreAuth, Principal } from "../auth/index.js";
import { checkGrant } from "./authz.js";
import type { ChunkRef, SubmitOpRequest, SubmitOpResponse } from "@valv/contracts-sync";
import {
  inTransaction,
  newId,
  requirePrincipal,
  supportsForUpdate,
  type MetadataHub,
  type MetadataVariables,
} from "./common.js";

export type CommittedOp = {
  folderId: string;
  serverSeq: number;
  nodeId: string;
  opType: SubmitOpRequest["op_type"];
  previousParentId?: string | null;
  chunkHashesAdded?: string[];
  chunkHashesRemoved?: string[];
};

export function registerOpRoutes(
  router: Hono<{ Variables: MetadataVariables }>,
  auth: CoreAuth,
  hub: MetadataHub,
  onOpCommitted?: (op: CommittedOp) => Promise<void>,
): void {
  router.post("/folders/:id/ops", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const body = (await ctx.req.json()) as SubmitOpRequest;
    try {
      const response = await submitOp(auth, hub, ctx.req.param("id"), principal, body, onOpCommitted);
      return ctx.json(response);
    } catch (error) {
      if (error instanceof Response) {
        return error;
      }
      throw error;
    }
  });
}

export async function submitOp(
  auth: CoreAuth,
  hub: MetadataHub,
  folderId: string,
  principal: Principal,
  op: SubmitOpRequest,
  onOpCommitted?: (op: CommittedOp) => Promise<void>,
): Promise<SubmitOpResponse> {
  if (principal.type !== "device") {
    throw new Response(JSON.stringify({ error: "device_required" }), { status: 403 });
  }

  if (op.op_type === "create") {
    const grant = await checkGrant(auth.db, op.payload.parent_id, principal, "write", auth.schema);
    if (!grant.granted) {
      throw new Response(JSON.stringify({ error: grant.reason }), { status: 403 });
    }
    return createNode(auth, hub, folderId, principal.deviceId, op, onOpCommitted);
  }

  const grant = await checkGrant(auth.db, op.node_id, principal, "write", auth.schema);
  if (!grant.granted) {
    throw new Response(JSON.stringify({ error: grant.reason }), { status: 403 });
  }

  if (op.op_type === "move") {
    const parents = await auth.db
      .select({ nodeId: auth.schema.nodes.nodeId, type: auth.schema.nodes.type })
      .from(auth.schema.nodes)
      .where(eq(auth.schema.nodes.nodeId, op.payload.new_parent_id))
      .limit(1);
    if (!parents[0] || parents[0].type !== "folder") {
      throw new Response(JSON.stringify({ error: "parent_not_found" }), { status: 404 });
    }
  }

  const { response, committedOp } = await inTransaction(auth, async (tx) => {
    if (supportsForUpdate(auth.schema) && typeof tx.execute === "function") {
      await tx.execute(sql`SELECT node_id FROM nodes WHERE node_id = ${op.node_id} FOR UPDATE`);
    }
    const nodes = await tx
      .select({ serverSeq: auth.schema.nodes.serverSeq })
      .from(auth.schema.nodes)
      .where(eq(auth.schema.nodes.nodeId, op.node_id))
      .limit(1);
    const node = nodes[0];
    if (!node) {
      throw new Response(JSON.stringify({ error: "node_not_found" }), { status: 404 });
    }

    if (node.serverSeq !== op.based_on_seq) {
      if (op.op_type !== "new_version") {
        return {
          response: { result: "superseded", current_seq: node.serverSeq } satisfies SubmitOpResponse,
          committedOp: undefined,
        };
      }
      const versionResult = await insertVersionAndOp(auth, tx, folderId, op.node_id, principal.deviceId, op, true);
      const serverSeq = await latestSeqForNode(auth, tx, folderId, op.node_id);
      await tx
        .update(auth.schema.nodes)
        .set({ serverSeq })
        .where(eq(auth.schema.nodes.nodeId, op.node_id));
      return {
        response: {
          result: "conflict_copy",
          server_seq: serverSeq,
          node_id: op.node_id,
          conflict_version_id: versionResult.versionId,
        } satisfies SubmitOpResponse,
        committedOp: {
          folderId,
          serverSeq,
          nodeId: op.node_id,
          opType: op.op_type,
          chunkHashesAdded: versionResult.chunkHashesAdded,
        } satisfies CommittedOp,
      };
    }

    if (op.op_type === "rename" || op.op_type === "move") {
      const nextName = op.op_type === "rename" ? op.payload.new_name : (await currentNode(auth, tx, op.node_id))?.name;
      const nextParentId = op.op_type === "move" ? op.payload.new_parent_id : (await currentNode(auth, tx, op.node_id))?.parentId;
      if (nextName && nextParentId) {
        const collisions = await tx
          .select({ nodeId: auth.schema.nodes.nodeId })
          .from(auth.schema.nodes)
          .where(
            sql`${auth.schema.nodes.folderId} = ${folderId} AND ${auth.schema.nodes.parentId} = ${nextParentId} AND ${auth.schema.nodes.name} = ${nextName} AND ${auth.schema.nodes.nodeId} <> ${op.node_id} AND ${auth.schema.nodes.deletedAt} IS NULL`,
          )
          .limit(1);
        if (collisions[0]) {
          throw new Response(JSON.stringify({ error: "name_collision" }), { status: 409 });
        }
      }
    }

    const previousParentId = op.op_type === "move" ? (await currentNode(auth, tx, op.node_id))?.parentId : undefined;
    await applyMetadataMutation(auth, tx, op.node_id, op);
    const versionResult = await insertVersionAndOp(auth, tx, folderId, op.node_id, principal.deviceId, op, false);
    const serverSeq = await latestSeqForNode(auth, tx, folderId, op.node_id);
    await tx
      .update(auth.schema.nodes)
      .set({ serverSeq })
      .where(eq(auth.schema.nodes.nodeId, op.node_id));
    return {
      response: { result: "applied", server_seq: serverSeq, node_id: op.node_id } satisfies SubmitOpResponse,
      committedOp: {
        folderId,
        serverSeq,
        nodeId: op.node_id,
        opType: op.op_type,
        previousParentId,
        chunkHashesAdded: versionResult.chunkHashesAdded,
        chunkHashesRemoved: versionResult.chunkHashesRemoved,
      } satisfies CommittedOp,
    };
  });
  if (committedOp) {
    await notifyCommitted(hub, committedOp, onOpCommitted);
  }
  return response;
}

async function createNode(
  auth: CoreAuth,
  hub: MetadataHub,
  folderId: string,
  actorDeviceId: string,
  op: Extract<SubmitOpRequest, { op_type: "create" }>,
  onOpCommitted?: (op: CommittedOp) => Promise<void>,
): Promise<SubmitOpResponse> {
  const nodeId = op.payload.node_id;
  let result: { response: SubmitOpResponse; committedOp: CommittedOp };
  try {
    result = await inTransaction(auth, async (tx) => {
      await tx.insert(auth.schema.nodes).values({
        nodeId,
        folderId,
        parentId: op.payload.parent_id,
        name: op.payload.name,
        type: op.payload.type,
        serverSeq: 0,
      });
      await tx.insert(auth.schema.opLog).values({
        folderId,
        nodeId,
        opType: op.op_type,
        opPayload: op.payload,
        basedOnSeq: null,
        actorDeviceId,
      });
      const serverSeq = await latestSeqForNode(auth, tx, folderId, nodeId);
      await tx.update(auth.schema.nodes).set({ serverSeq }).where(eq(auth.schema.nodes.nodeId, nodeId));
      return {
        response: { result: "applied", server_seq: serverSeq, node_id: nodeId } satisfies SubmitOpResponse,
        committedOp: { folderId, serverSeq, nodeId, opType: op.op_type } satisfies CommittedOp,
      };
    });
  } catch (error) {
    const winners = await auth.db
      .select({ nodeId: auth.schema.nodes.nodeId, serverSeq: auth.schema.nodes.serverSeq })
      .from(auth.schema.nodes)
      .where(
        sql`${auth.schema.nodes.folderId} = ${folderId} AND ${auth.schema.nodes.parentId} = ${op.payload.parent_id} AND ${auth.schema.nodes.name} = ${op.payload.name} AND ${auth.schema.nodes.deletedAt} IS NULL`,
      )
      .limit(1);
    if (winners[0]) {
      return { result: "superseded", current_seq: winners[0].serverSeq };
    }
    throw error;
  }
  await notifyCommitted(hub, result.committedOp, onOpCommitted);
  return result.response;
}

async function notifyCommitted(
  hub: MetadataHub,
  op: CommittedOp,
  onOpCommitted?: (op: CommittedOp) => Promise<void>,
): Promise<void> {
  try {
    hub.notify(op.folderId, op.serverSeq);
    await onOpCommitted?.(op);
  } catch (error) {
    console.error("metadata notification failed", error);
  }
}

async function applyMetadataMutation(
  auth: CoreAuth,
  tx: CoreAuth["db"],
  nodeId: string,
  op: Exclude<SubmitOpRequest, { op_type: "create" }>,
): Promise<void> {
  if (op.op_type === "rename") {
    await tx.update(auth.schema.nodes).set({ name: op.payload.new_name }).where(eq(auth.schema.nodes.nodeId, nodeId));
  }
  if (op.op_type === "move") {
    await tx.update(auth.schema.nodes).set({ parentId: op.payload.new_parent_id }).where(eq(auth.schema.nodes.nodeId, nodeId));
  }
  if (op.op_type === "delete") {
    await tx.update(auth.schema.nodes).set({ deletedAt: new Date() }).where(eq(auth.schema.nodes.nodeId, nodeId));
  }
}

async function currentNode(
  auth: CoreAuth,
  tx: CoreAuth["db"],
  nodeId: string,
): Promise<{ name: string; parentId: string | null } | undefined> {
  const rows = await tx
    .select({ name: auth.schema.nodes.name, parentId: auth.schema.nodes.parentId })
    .from(auth.schema.nodes)
    .where(eq(auth.schema.nodes.nodeId, nodeId))
    .limit(1);
  return rows[0];
}

async function insertVersionAndOp(
  auth: CoreAuth,
  tx: CoreAuth["db"],
  folderId: string,
  nodeId: string,
  actorDeviceId: string,
  op: Exclude<SubmitOpRequest, { op_type: "create" }>,
  isConflictCopy: boolean,
): Promise<{ versionId: string; chunkHashesAdded?: string[]; chunkHashesRemoved?: string[] }> {
  let versionId = "";
  let chunkHashesAdded: string[] | undefined;
  let chunkHashesRemoved: string[] | undefined;
  if (op.op_type === "new_version") {
    versionId = isConflictCopy ? newId() : op.payload.version_id;
    let previousManifest: ChunkRef[] = [];
    if (!isConflictCopy) {
      previousManifest = await currentVersionManifest(auth, tx, nodeId);
    }
    await tx.insert(auth.schema.versions).values({
      versionId,
      nodeId,
      manifest: op.payload.manifest,
      contentHash: op.payload.content_hash,
      sizeBytes: op.payload.size_bytes,
      authorDeviceId: actorDeviceId,
      isConflictCopy,
    });
    const chunkHashes = [...new Set<string>(op.payload.manifest.map((chunk: ChunkRef) => chunk.chunk_hash))];
    chunkHashesAdded = chunkHashes;
    if (chunkHashes.length > 0) {
      await tx.insert(auth.schema.versionChunks).values(
        chunkHashes.map((chunkHash) => ({
          versionId,
          nodeId,
          chunkHash,
        })),
      );
      await tx
        .update(auth.schema.chunks)
        .set({ refcount: sql`${auth.schema.chunks.refcount} + 1` })
        .where(inArray(auth.schema.chunks.chunkHash, chunkHashes));
    }
    if (!isConflictCopy) {
      const previousChunkHashes = [...new Set(previousManifest.map((chunk) => chunk.chunk_hash))];
      chunkHashesRemoved = previousChunkHashes;
      if (previousChunkHashes.length > 0) {
        await tx
          .update(auth.schema.chunks)
          .set({ refcount: sql`CASE WHEN ${auth.schema.chunks.refcount} > 0 THEN ${auth.schema.chunks.refcount} - 1 ELSE 0 END` })
          .where(inArray(auth.schema.chunks.chunkHash, previousChunkHashes));
      }
      await tx
        .update(auth.schema.nodes)
        .set({ currentVersionId: versionId })
        .where(eq(auth.schema.nodes.nodeId, nodeId));
    }
  }

  await tx.insert(auth.schema.opLog).values({
    folderId,
    nodeId,
    opType: op.op_type,
    opPayload: isConflictCopy ? { ...op.payload, version_id: versionId, is_conflict_copy: true } : op.payload,
    basedOnSeq: op.based_on_seq,
    actorDeviceId,
  });
  return { versionId, chunkHashesAdded, chunkHashesRemoved };
}

async function currentVersionManifest(
  auth: CoreAuth,
  tx: CoreAuth["db"],
  nodeId: string,
): Promise<ChunkRef[]> {
  const rows = await tx
    .select({ manifest: auth.schema.versions.manifest })
    .from(auth.schema.nodes)
    .innerJoin(auth.schema.versions, eq(auth.schema.nodes.currentVersionId, auth.schema.versions.versionId))
    .where(eq(auth.schema.nodes.nodeId, nodeId))
    .limit(1);
  const manifest = rows[0]?.manifest;
  return Array.isArray(manifest) ? manifest as ChunkRef[] : [];
}

async function latestSeqForNode(
  auth: CoreAuth,
  tx: CoreAuth["db"],
  folderId: string,
  nodeId: string,
): Promise<number> {
  const rows = await tx
    .select({ serverSeq: auth.schema.opLog.serverSeq })
    .from(auth.schema.opLog)
    .where(sql`${auth.schema.opLog.folderId} = ${folderId} AND ${auth.schema.opLog.nodeId} = ${nodeId}`)
    .orderBy(sql`${auth.schema.opLog.serverSeq} DESC`)
    .limit(1);
  return rows[0]?.serverSeq ?? 0;
}
