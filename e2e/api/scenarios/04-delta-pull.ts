import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { createNode, requestJson } from "./helpers.js";
import type { SeededHarness } from "./types.js";

export function deltaPullScenarios(harness: SeededHarness): void {
  describe("delta pull API", () => {
    let ctx: Awaited<ReturnType<SeededHarness["createApp"]>>;

    beforeAll(async () => {
      ctx = await harness.createApp();
    });

    afterAll(async () => ctx?.cleanup());

    it("filters by since and scope for ops and tree responses", async () => {
      const subdirA = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "subdir-a", "folder");
      const subdirB = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "subdir-b", "folder");
      const aFile = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, subdirA.nodeId, "a.txt", "file");
      const bFile = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, subdirB.nodeId, "b.txt", "file");

      const all = await requestJson<{ ops: Array<{ server_seq: number; node_id: string }> }>(ctx.app, `/api/folders/${ctx.context.folderId}/ops?since=0`, {
        bearerToken: ctx.context.token,
      });
      expect(all.ops.map((op) => op.node_id)).toEqual(expect.arrayContaining([subdirA.nodeId, subdirB.nodeId, aFile.nodeId, bFile.nodeId]));

      const since = await requestJson<{ ops: Array<{ server_seq: number; node_id: string }> }>(ctx.app, `/api/folders/${ctx.context.folderId}/ops?since=${subdirB.server_seq}`, {
        bearerToken: ctx.context.token,
      });
      expect(since.ops.every((op) => op.server_seq > subdirB.server_seq)).toBe(true);

      const scoped = await requestJson<{ token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/grants`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { scope_node_id: subdirA.nodeId, name: "Scoped", can_read: true, can_write: true },
      });
      const scopedOps = await requestJson<{ ops: Array<{ node_id: string }> }>(ctx.app, `/api/folders/${ctx.context.folderId}/ops?since=0`, {
        bearerToken: scoped.token,
      });
      expect(scopedOps.ops.map((op) => op.node_id)).toContain(aFile.nodeId);
      expect(scopedOps.ops.map((op) => op.node_id)).not.toContain(bFile.nodeId);

      const fullTree = await requestJson<{ nodes: Array<{ node_id: string }> }>(ctx.app, `/api/folders/${ctx.context.folderId}/tree`, {
        bearerToken: ctx.context.token,
      });
      expect(fullTree.nodes.map((node) => node.node_id)).toEqual(expect.arrayContaining([ctx.context.rootNodeId, aFile.nodeId, bFile.nodeId]));

      const scopedTree = await requestJson<{ nodes: Array<{ node_id: string; parent_id: string | null }> }>(ctx.app, `/api/folders/${ctx.context.folderId}/tree`, {
        bearerToken: scoped.token,
      });
      expect(scopedTree.nodes.map((node) => node.node_id)).toContain(aFile.nodeId);
      expect(scopedTree.nodes.map((node) => node.node_id)).not.toContain(bFile.nodeId);
      expect(scopedTree.nodes.find((node) => node.node_id === subdirA.nodeId)?.parent_id).toBeNull();
    });
  });
}
