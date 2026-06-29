import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { cleanupAppContext, createAppContext, createNode, submitOp } from "../setup/api.js";
import { requestJson } from "../setup/helpers.js";

describe("grant API", () => {
  let ctx: Awaited<ReturnType<typeof createAppContext>>;

  beforeAll(async () => {
    ctx = await createAppContext();
  });

  afterAll(async () => cleanupAppContext(ctx));

  it("creates scoped device grants, enforces scope, and revokes tokens", async () => {
    const subdir = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "scoped", "folder");
    const grant = await requestJson<{ grant_id: string; device_id: string; token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/grants`, {
      method: "POST",
      cookie: ctx.context.cookie,
      body: { scope_node_id: subdir.nodeId, name: "Scoped Device", can_read: true, can_write: true },
    });
    expect(grant.device_id).toEqual(expect.any(String));

    const inside = await createNode(ctx.app, ctx.context.folderId, grant.token, subdir.nodeId, "inside.txt", "file");
    expect(inside.result).toBe("applied");

    await submitOp(ctx.app, ctx.context.folderId, grant.token, {
      op_type: "create",
      payload: { node_id: crypto.randomUUID(), parent_id: ctx.context.rootNodeId, name: "outside.txt", type: "file" },
    }, 403);

    const revoked = await ctx.app.request(`/api/folders/${ctx.context.folderId}/grants/${grant.grant_id}`, {
      method: "DELETE",
      headers: { cookie: ctx.context.cookie },
    });
    expect(revoked.status).toBe(204);
    const after = await ctx.app.request("/api/grants", { headers: { authorization: `Bearer ${grant.token}` } });
    expect([401, 403]).toContain(after.status);
  });
});
