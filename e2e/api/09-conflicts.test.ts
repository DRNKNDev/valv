import { randomUUID } from "node:crypto";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { cleanupAppContext, createAppContext, createNode, submitOp } from "../setup/api.js";
import { requestJson } from "../setup/helpers.js";

describe("conflict handling API", () => {
  let ctx: Awaited<ReturnType<typeof createAppContext>>;

  beforeAll(async () => {
    ctx = await createAppContext();
  });

  afterAll(async () => cleanupAppContext(ctx));

  it("creates conflict copies for concurrent new_version and supersedes stale metadata", async () => {
    const file = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "conflict.txt", "file");
    const basedOn = file.server_seq;

    await submitOp(ctx.app, ctx.context.folderId, ctx.context.token, {
      op_type: "new_version",
      node_id: file.nodeId,
      based_on_seq: basedOn,
      payload: { version_id: randomUUID(), content_hash: "winner", size_bytes: 1, manifest: [] },
    });
    const conflict = await submitOp<{ result: string; conflict_version_id: string }>(ctx.app, ctx.context.folderId, ctx.context.token, {
      op_type: "new_version",
      node_id: file.nodeId,
      based_on_seq: basedOn,
      payload: { version_id: randomUUID(), content_hash: "conflict", size_bytes: 1, manifest: [] },
    });
    expect(conflict.result).toBe("conflict_copy");
    expect(conflict.conflict_version_id).toEqual(expect.any(String));

    const versions = await requestJson<Array<{ version_id: string; is_conflict_copy: boolean }>>(ctx.app, `/api/folders/${ctx.context.folderId}/versions/${file.nodeId}`, {
      bearerToken: ctx.context.token,
    });
    expect(versions).toEqual(expect.arrayContaining([expect.objectContaining({ version_id: conflict.conflict_version_id, is_conflict_copy: true })]));

    const staleRename = await submitOp<{ result: string; current_seq: number }>(ctx.app, ctx.context.folderId, ctx.context.token, {
      op_type: "rename",
      node_id: file.nodeId,
      based_on_seq: basedOn,
      payload: { new_name: "stale-conflict.txt" },
    });
    expect(staleRename.result).toBe("superseded");
  });
});
