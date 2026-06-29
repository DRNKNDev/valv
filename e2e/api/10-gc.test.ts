import { randomUUID } from "node:crypto";

import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { startGc } from "@valv/core";

import { cleanupAppContext, createAppContext, createNode, row, submitOp } from "../setup/api.js";

describe("GC API integration", () => {
  let ctx: Awaited<ReturnType<typeof createAppContext>>;
  let stopGc: (() => void) | undefined;

  beforeAll(async () => {
    ctx = await createAppContext();
  });

  afterEach(() => {
    stopGc?.();
    stopGc = undefined;
    vi.useRealTimers();
  });

  afterAll(async () => cleanupAppContext(ctx));

  it("purges tombstones after 0ms retention", async () => {
    vi.useFakeTimers();
    const file = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "dead.txt", "file");
    await submitOp(ctx.app, ctx.context.folderId, ctx.context.token, {
      op_type: "delete",
      node_id: file.nodeId,
      based_on_seq: file.server_seq,
      payload: {},
    });

    stopGc = startGc(ctx.db, ctx.s3, ctx.bucket, {
      chunkGcIntervalMs: 60_000,
      tombstonePurgeIntervalMs: 10,
      tombstoneRetentionMs: 0,
      opLogTruncationIntervalMs: 60_000,
    });
    await vi.advanceTimersByTimeAsync(20);
    expect(row(ctx.sqlite, "SELECT node_id FROM nodes WHERE node_id = ?", file.nodeId)).toBeUndefined();
  });

  it("decrements old chunk refcounts when a version is superseded", async () => {
    const file = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "versions.txt", "file");
    const oldHash = randomUUID();
    const newHash = randomUUID();
    ctx.sqlite.prepare("INSERT INTO chunks (chunk_hash, size_bytes, refcount, created_at) VALUES (?, ?, ?, ?)").run(oldHash, 1, 0, Date.now());
    ctx.sqlite.prepare("INSERT INTO chunks (chunk_hash, size_bytes, refcount, created_at) VALUES (?, ?, ?, ?)").run(newHash, 1, 0, Date.now());

    const first = await submitOp<{ server_seq: number }>(ctx.app, ctx.context.folderId, ctx.context.token, {
      op_type: "new_version",
      node_id: file.nodeId,
      based_on_seq: file.server_seq,
      payload: { version_id: randomUUID(), content_hash: oldHash, size_bytes: 1, manifest: [{ chunk_hash: oldHash, offset: 0, length: 1 }] },
    });
    await submitOp(ctx.app, ctx.context.folderId, ctx.context.token, {
      op_type: "new_version",
      node_id: file.nodeId,
      based_on_seq: first.server_seq,
      payload: { version_id: randomUUID(), content_hash: newHash, size_bytes: 1, manifest: [{ chunk_hash: newHash, offset: 0, length: 1 }] },
    });

    expect(row<{ refcount: number }>(ctx.sqlite, "SELECT refcount FROM chunks WHERE chunk_hash = ?", oldHash)?.refcount).toBe(0);
    expect(row<{ refcount: number }>(ctx.sqlite, "SELECT refcount FROM chunks WHERE chunk_hash = ?", newHash)?.refcount).toBe(1);
  });
});
