import { randomUUID } from "node:crypto";

import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { startGc } from "@valv/core";

import { createNode, submitOp } from "./helpers.js";
import type { SeededHarness } from "./types.js";

export function gcScenarios(harness: SeededHarness): void {
  describe("GC API integration", () => {
    let ctx: Awaited<ReturnType<SeededHarness["createApp"]>>;
    let stopGc: (() => void) | undefined;

    beforeAll(async () => {
      ctx = await harness.createApp();
    });

    afterEach(() => {
      stopGc?.();
      stopGc = undefined;
      vi.useRealTimers();
    });

    afterAll(async () => ctx?.cleanup());

    it("purges tombstones after 0ms retention", async () => {
      const file = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "dead.txt", "file");
      await submitOp(ctx.app, ctx.context.folderId, ctx.context.token, {
        op_type: "delete",
        node_id: file.nodeId,
        based_on_seq: file.server_seq,
        payload: {},
      });
      // Scoped to setInterval/clearInterval/Date only - startGc only ever uses setInterval, and
      // the Postgres Pool driver (@neondatabase/serverless) schedules its own connection-idle/
      // timeout bookkeeping via real setTimeout calls. Faking setTimeout globally here would trap
      // those in fake-timer land, so pool.end() in this file's afterAll hangs waiting on a timeout
      // callback that never fires once vi.useRealTimers() restores the real (but now stale) clock.
      vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
      vi.setSystemTime(Date.now() + 1_000);

      stopGc = startGc(ctx.db as Parameters<typeof startGc>[0], ctx.s3 as Parameters<typeof startGc>[1], ctx.bucket, undefined, {
        chunkGcIntervalMs: 60_000,
        tombstonePurgeIntervalMs: 10,
        tombstoneRetentionMs: 0,
        opLogTruncationIntervalMs: 60_000,
      });
      await vi.advanceTimersByTimeAsync(100);
      stopGc();
      stopGc = undefined;
      vi.useRealTimers();
      await waitForMissingNode(ctx, file.nodeId);
    });

    it("decrements old chunk refcounts when a version is superseded", async () => {
      const file = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "versions.txt", "file");
      const oldHash = randomUUID();
      const newHash = randomUUID();
      await ctx.exec("INSERT INTO chunks (chunk_hash, size_bytes, refcount, created_at) VALUES (?, ?, ?, ?)", oldHash, 1, 0, Date.now());
      await ctx.exec("INSERT INTO chunks (chunk_hash, size_bytes, refcount, created_at) VALUES (?, ?, ?, ?)", newHash, 1, 0, Date.now());

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

      expect((await ctx.row<{ refcount: number }>("SELECT refcount FROM chunks WHERE chunk_hash = ?", oldHash))?.refcount).toBe(0);
      expect((await ctx.row<{ refcount: number }>("SELECT refcount FROM chunks WHERE chunk_hash = ?", newHash))?.refcount).toBe(1);
    });
  });
}

async function waitForMissingNode(ctx: Awaited<ReturnType<SeededHarness["createApp"]>>, nodeId: string): Promise<void> {
  const deadline = Date.now() + 2_000;
  while (Date.now() < deadline) {
    if ((await ctx.row("SELECT node_id FROM nodes WHERE node_id = ?", nodeId)) === undefined) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  expect(await ctx.row("SELECT node_id FROM nodes WHERE node_id = ?", nodeId)).toBeUndefined();
}
