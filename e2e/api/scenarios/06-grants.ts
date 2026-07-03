import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { createNode, requestJson, submitOp } from "./helpers.js";
import type { SeededHarness } from "./types.js";

export function grantScenarios(harness: SeededHarness): void {
  describe("grant API", () => {
    let ctx: Awaited<ReturnType<SeededHarness["createApp"]>>;

    beforeAll(async () => {
      ctx = await harness.createApp();
    });

    afterAll(async () => ctx?.cleanup());

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

    it("GET /api/grants includes grantee_email for a user-held grant and device_name for a device-held grant", async () => {
      const deviceGrant = await requestJson<{ grant_id: string; device_id: string; token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/grants`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { scope_node_id: ctx.context.rootNodeId, name: "CI Agent", can_read: true, can_write: true },
      });

      const grants = await requestJson<
        Array<{ grant_id: string; user_id: string | null; device_id: string | null; grantee_email: string | null; device_name: string | null }>
      >(ctx.app, "/api/grants", { cookie: ctx.context.cookie });

      const ownerGrant = grants.find((item) => item.user_id === ctx.context.userId);
      expect(ownerGrant).toMatchObject({
        user_id: ctx.context.userId,
        device_id: null,
        grantee_email: `${ctx.context.userId}@example.com`,
        device_name: null,
      });

      const agentGrant = await ctx.app.request("/api/grants", { headers: { authorization: `Bearer ${deviceGrant.token}` } });
      const agentGrants = (await agentGrant.json()) as typeof grants;
      const agentOwnGrant = agentGrants.find((item) => item.grant_id === deviceGrant.grant_id);
      expect(agentOwnGrant).toMatchObject({
        user_id: null,
        device_id: deviceGrant.device_id,
        grantee_email: null,
        device_name: "CI Agent",
      });
    });

    it("rejects device grant provisioning from a read-only grant holder", async () => {
      const readOnlyGrant = await requestJson<{ grant_id: string; device_id: string; token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/grants`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { scope_node_id: ctx.context.rootNodeId, name: "Read Only Agent", can_read: true, can_write: false },
      });

      const response = await ctx.app.request(`/api/folders/${ctx.context.folderId}/grants`, {
        method: "POST",
        body: JSON.stringify({ scope_node_id: ctx.context.rootNodeId, name: "Should Fail" }),
        headers: { "content-type": "application/json", authorization: `Bearer ${readOnlyGrant.token}` },
      });

      expect(response.status).toBe(403);
    });
  });
}
