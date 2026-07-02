import { Hono } from "hono";
import { eq, inArray, sql } from "drizzle-orm";

import type { CoreAuth, Principal } from "../auth/index.js";
import { pgSchema } from "../db/schema.js";
import { checkGrant } from "./authz.js";
import {
  inTransaction,
  newId,
  requirePrincipal,
  type MetadataHub,
  type MetadataVariables,
} from "./common.js";

type ChunkRef = { chunk_hash: string; offset: number; length: number };
type SubmitOpRequest =
  | { op_type: "create"; payload: { node_id: string; parent_id: string; name: string; type: "file" | "folder" } }
  | { op_type: "rename"; node_id: string; based_on_seq: number; payload: { new_name: string } }
  | { op_type: "move"; node_id: string; based_on_seq: number; payload: { new_parent_id: string } }
  | { op_type: "delete"; node_id: string; based_on_seq: number; payload: Record<string, never> }
  | {
      op_type: "new_version";
      node_id: string;
      based_on_seq: number;
      payload: { version_id: string; content_hash: string; size_bytes: number; manifest: ChunkRef[] };
    };
type SubmitOpResponse =
  | { result: "applied"; server_seq: number; node_id: string }
  | { result: "conflict_copy"; server_seq: number; node_id: string; conflict_version_id: string }
  | { result: "superseded"; current_seq: number };

type OpNode = {
  nodeId: string;
  folderId: string;
  parentId: string | null;
  name: string;
  type: "file" | "folder";
  serverSeq: number;
  currentVersionId?: string | null;
  deletedAt?: Date | null;
};

type OpStoreDb = {
  getNodeForOp: (nodeId: string) => Promise<OpNode | undefined>;
  findLiveChildForOp: (opts: { folderId: string; parentId: string; name: string }) => Promise<OpNode | undefined>;
  insertNodeForOp: (node: OpNode) => Promise<void>;
  updateNodeForOp: (nodeId: string, patch: Partial<OpNode>) => Promise<void>;
  insertVersionForOp: (version: {
    versionId: string;
    nodeId: string;
    manifest: ChunkRef[];
    contentHash: string;
    sizeBytes: number;
    authorDeviceId: string;
    isConflictCopy: boolean;
  }) => Promise<void>;
  incrementChunksForOp: (chunkHashes: string[]) => Promise<void>;
  insertOpForOp: (op: {
    folderId: string;
    nodeId: string;
    opType: string;
    opPayload: Record<string, unknown>;
    basedOnSeq: number | null;
    actorDeviceId: string;
  }) => Promise<number>;
};

export function registerOpRoutes(
  router: Hono<{ Variables: MetadataVariables }>,
  auth: CoreAuth,
  hub: MetadataHub,
  onOpCommitted?: (folderId: string, serverSeq: number) => Promise<void>,
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
  onOpCommitted?: (folderId: string, serverSeq: number) => Promise<void>,
): Promise<SubmitOpResponse> {
  if (principal.type !== "device") {
    throw new Response(JSON.stringify({ error: "device_required" }), { status: 403 });
  }

  if (hasOpStore(auth.db)) {
    return submitOpWithStore(auth.db, hub, folderId, principal, op, onOpCommitted);
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

  return inTransaction(auth, async (tx) => {
    if (auth.schema === pgSchema && typeof tx.execute === "function") {
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
        return { result: "superseded", current_seq: node.serverSeq } satisfies SubmitOpResponse;
      }
      const conflictVersionId = await insertVersionAndOp(auth, tx, folderId, op.node_id, principal.deviceId, op, true);
      const serverSeq = await latestSeqForNode(auth, tx, folderId, op.node_id);
      await notifyCommitted(hub, folderId, serverSeq, onOpCommitted);
      return {
        result: "conflict_copy",
        server_seq: serverSeq,
        node_id: op.node_id,
        conflict_version_id: conflictVersionId,
      } satisfies SubmitOpResponse;
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

    await applyMetadataMutation(auth, tx, op.node_id, op);
    await insertVersionAndOp(auth, tx, folderId, op.node_id, principal.deviceId, op, false);
    const serverSeq = await latestSeqForNode(auth, tx, folderId, op.node_id);
    await tx
      .update(auth.schema.nodes)
      .set({ serverSeq })
      .where(eq(auth.schema.nodes.nodeId, op.node_id));
    await notifyCommitted(hub, folderId, serverSeq, onOpCommitted);
    return { result: "applied", server_seq: serverSeq, node_id: op.node_id } satisfies SubmitOpResponse;
  });
}

async function submitOpWithStore(
  db: CoreAuth["db"] & OpStoreDb,
  hub: MetadataHub,
  folderId: string,
  principal: Extract<Principal, { type: "device" }>,
  op: SubmitOpRequest,
  onOpCommitted?: (folderId: string, serverSeq: number) => Promise<void>,
): Promise<SubmitOpResponse> {
  if (op.op_type === "create") {
    const grant = await checkGrant(db, op.payload.parent_id, principal, "write");
    if (!grant.granted) {
      throw new Response(JSON.stringify({ error: grant.reason }), { status: 403 });
    }
    const existing = await db.findLiveChildForOp({ folderId, parentId: op.payload.parent_id, name: op.payload.name });
    if (existing) {
      return { result: "superseded", current_seq: existing.serverSeq };
    }
    const nodeId = newId();
    await db.insertNodeForOp({
      nodeId,
      folderId,
      parentId: op.payload.parent_id,
      name: op.payload.name,
      type: op.payload.type,
      serverSeq: 0,
      currentVersionId: null,
      deletedAt: null,
    });
    const serverSeq = await db.insertOpForOp({
      folderId,
      nodeId,
      opType: op.op_type,
      opPayload: op.payload,
      basedOnSeq: null,
      actorDeviceId: principal.deviceId,
    });
    await db.updateNodeForOp(nodeId, { serverSeq });
    await notifyCommitted(hub, folderId, serverSeq, onOpCommitted);
    return { result: "applied", server_seq: serverSeq, node_id: nodeId };
  }

  const grant = await checkGrant(db, op.node_id, principal, "write");
  if (!grant.granted) {
    throw new Response(JSON.stringify({ error: grant.reason }), { status: 403 });
  }
  const node = await db.getNodeForOp(op.node_id);
  if (!node) {
    throw new Response(JSON.stringify({ error: "node_not_found" }), { status: 404 });
  }

  if (node.serverSeq !== op.based_on_seq) {
    if (op.op_type !== "new_version") {
      return { result: "superseded", current_seq: node.serverSeq };
    }
    const conflictVersionId = newId();
    await db.insertVersionForOp({
      versionId: conflictVersionId,
      nodeId: op.node_id,
      manifest: op.payload.manifest,
      contentHash: op.payload.content_hash,
      sizeBytes: op.payload.size_bytes,
      authorDeviceId: principal.deviceId,
      isConflictCopy: true,
    });
    await db.incrementChunksForOp(op.payload.manifest.map((chunk) => chunk.chunk_hash));
    const serverSeq = await db.insertOpForOp({
      folderId,
      nodeId: op.node_id,
      opType: op.op_type,
      opPayload: { ...op.payload, version_id: conflictVersionId, is_conflict_copy: true },
      basedOnSeq: op.based_on_seq,
      actorDeviceId: principal.deviceId,
    });
    await notifyCommitted(hub, folderId, serverSeq, onOpCommitted);
    return { result: "conflict_copy", server_seq: serverSeq, node_id: op.node_id, conflict_version_id: conflictVersionId };
  }

  const nodePatch: Partial<OpNode> = {};
  if (op.op_type === "rename") {
    nodePatch.name = op.payload.new_name;
  }
  if (op.op_type === "move") {
    const parent = await db.getNodeForOp(op.payload.new_parent_id);
    if (!parent || parent.type !== "folder") {
      throw new Response(JSON.stringify({ error: "parent_not_found" }), { status: 404 });
    }
    const existing = await db.findLiveChildForOp({ folderId, parentId: op.payload.new_parent_id, name: node.name });
    if (existing && existing.nodeId !== op.node_id) {
      throw new Response(JSON.stringify({ error: "name_collision" }), { status: 409 });
    }
    nodePatch.parentId = op.payload.new_parent_id;
  }
  if (op.op_type === "rename") {
    const existing = await db.findLiveChildForOp({ folderId, parentId: node.parentId ?? "", name: op.payload.new_name });
    if (existing && existing.nodeId !== op.node_id) {
      throw new Response(JSON.stringify({ error: "name_collision" }), { status: 409 });
    }
  }
  if (op.op_type === "delete") {
    nodePatch.deletedAt = new Date();
  }
  if (op.op_type === "new_version") {
    await db.insertVersionForOp({
      versionId: op.payload.version_id,
      nodeId: op.node_id,
      manifest: op.payload.manifest,
      contentHash: op.payload.content_hash,
      sizeBytes: op.payload.size_bytes,
      authorDeviceId: principal.deviceId,
      isConflictCopy: false,
    });
    await db.incrementChunksForOp(op.payload.manifest.map((chunk) => chunk.chunk_hash));
    nodePatch.currentVersionId = op.payload.version_id;
  }
  await db.updateNodeForOp(op.node_id, nodePatch);
  const serverSeq = await db.insertOpForOp({
    folderId,
    nodeId: op.node_id,
    opType: op.op_type,
    opPayload: op.payload,
    basedOnSeq: op.based_on_seq,
    actorDeviceId: principal.deviceId,
  });
  await db.updateNodeForOp(op.node_id, { serverSeq });
  await notifyCommitted(hub, folderId, serverSeq, onOpCommitted);
  return { result: "applied", server_seq: serverSeq, node_id: op.node_id };
}

function hasOpStore(db: CoreAuth["db"]): db is CoreAuth["db"] & OpStoreDb {
  return "getNodeForOp" in db && "insertOpForOp" in db;
}

async function createNode(
  auth: CoreAuth,
  hub: MetadataHub,
  folderId: string,
  actorDeviceId: string,
  op: Extract<SubmitOpRequest, { op_type: "create" }>,
  onOpCommitted?: (folderId: string, serverSeq: number) => Promise<void>,
): Promise<SubmitOpResponse> {
  const nodeId = op.payload.node_id;
  try {
    return await inTransaction(auth, async (tx) => {
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
      await notifyCommitted(hub, folderId, serverSeq, onOpCommitted);
      return { result: "applied", server_seq: serverSeq, node_id: nodeId } satisfies SubmitOpResponse;
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
}

async function notifyCommitted(
  hub: MetadataHub,
  folderId: string,
  serverSeq: number,
  onOpCommitted?: (folderId: string, serverSeq: number) => Promise<void>,
): Promise<void> {
  hub.notify(folderId, serverSeq);
  await onOpCommitted?.(folderId, serverSeq);
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
): Promise<string> {
  let versionId = "";
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
    const chunkHashes = op.payload.manifest.map((chunk: ChunkRef) => chunk.chunk_hash);
    if (chunkHashes.length > 0) {
      await tx
        .update(auth.schema.chunks)
        .set({ refcount: sql`${auth.schema.chunks.refcount} + 1` })
        .where(inArray(auth.schema.chunks.chunkHash, chunkHashes));
    }
    if (!isConflictCopy) {
      const previousChunkHashes = previousManifest.map((chunk) => chunk.chunk_hash);
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
  return versionId;
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
