import { randomUUID } from "node:crypto";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { createNode, submitOp } from "./helpers.js";
import type { SeededHarness } from "./types.js";

export function opScenarios(harness: SeededHarness): void {
  describe("op submission API", () => {
    let ctx: Awaited<ReturnType<SeededHarness["createApp"]>>;

    beforeAll(async () => {
      ctx = await harness.createApp();
    });

    afterAll(async () => ctx?.cleanup());

    it("applies all op types, enforces CAS, and rejects collisions", async () => {
      const folder = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "Projects", "folder");
      const file = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "doc.txt", "file");
      expect(file.result).toBe("applied");
      expect(file.server_seq).toBeGreaterThan(0);

      const renamed = await submitOp<{ result: string; server_seq: number }>(ctx.app, ctx.context.folderId, ctx.context.token, {
        op_type: "rename",
        node_id: file.nodeId,
        based_on_seq: file.server_seq,
        payload: { new_name: "renamed.txt" },
      });
      expect(renamed.result).toBe("applied");
      expect((await ctx.row<{ name: string }>("SELECT name FROM nodes WHERE node_id = ?", file.nodeId))?.name).toBe("renamed.txt");

      const stale = await submitOp<{ result: string; current_seq: number }>(ctx.app, ctx.context.folderId, ctx.context.token, {
        op_type: "rename",
        node_id: file.nodeId,
        based_on_seq: file.server_seq,
        payload: { new_name: "stale.txt" },
      });
      expect(stale).toEqual({ result: "superseded", current_seq: renamed.server_seq });

      const moved = await submitOp<{ result: string; server_seq: number }>(ctx.app, ctx.context.folderId, ctx.context.token, {
        op_type: "move",
        node_id: file.nodeId,
        based_on_seq: renamed.server_seq,
        payload: { new_parent_id: folder.nodeId },
      });
      expect(moved.result).toBe("applied");
      expect((await ctx.row<{ parent_id: string }>("SELECT parent_id FROM nodes WHERE node_id = ?", file.nodeId))?.parent_id).toBe(folder.nodeId);

      const versioned = await submitOp<{ result: string; server_seq: number }>(ctx.app, ctx.context.folderId, ctx.context.token, {
        op_type: "new_version",
        node_id: file.nodeId,
        based_on_seq: moved.server_seq,
        payload: { version_id: randomUUID(), content_hash: "hash-1", size_bytes: 5, manifest: [] },
      });
      expect(versioned.result).toBe("applied");

      const deleted = await submitOp<{ result: string }>(ctx.app, ctx.context.folderId, ctx.context.token, {
        op_type: "delete",
        node_id: file.nodeId,
        based_on_seq: versioned.server_seq,
        payload: {},
      });
      expect(deleted.result).toBe("applied");
      expect((await ctx.row<{ deleted_at: number }>("SELECT deleted_at FROM nodes WHERE node_id = ?", file.nodeId))?.deleted_at).toBeTruthy();

      const taken = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "taken.txt", "file");
      const source = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "source.txt", "file");
      await submitOp(ctx.app, ctx.context.folderId, ctx.context.token, {
        op_type: "rename",
        node_id: source.nodeId,
        based_on_seq: source.server_seq,
        payload: { new_name: "taken.txt" },
      }, 409);
      expect(taken.nodeId).toEqual(expect.any(String));

      await submitOp(ctx.app, ctx.context.folderId, ctx.context.token, {
        op_type: "move",
        node_id: source.nodeId,
        based_on_seq: source.server_seq,
        payload: { new_parent_id: randomUUID() },
      }, 404);
    });
  });
}
