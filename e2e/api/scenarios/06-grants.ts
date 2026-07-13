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

    it("rejects device grant provisioning from a read-only USER principal with insufficient_permission", async () => {
      const readOnlyEmail = `readonly-collaborator-${crypto.randomUUID()}@example.com`;
      const invite = await requestJson<{ invite_token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/invites`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { invited_email: readOnlyEmail, can_write: false },
      });
      const signup = await ctx.app.request("/api/auth/sign-up/email", {
        method: "POST",
        body: JSON.stringify({ name: "Read Only Collaborator", email: readOnlyEmail, password: "password1234" }),
        headers: { "content-type": "application/json" },
      });
      const collaboratorCookie = signup.headers.get("set-cookie")?.split(";")[0];
      await requestJson(ctx.app, `/api/invites/${invite.invite_token}/accept`, { method: "POST", cookie: collaboratorCookie });

      const response = await ctx.app.request(`/api/folders/${ctx.context.folderId}/grants`, {
        method: "POST",
        body: JSON.stringify({ scope_node_id: ctx.context.rootNodeId, name: "Should Fail" }),
        headers: { "content-type": "application/json", cookie: collaboratorCookie ?? "" },
      });
      const body = await response.json();

      expect(response.status).toBe(403);
      expect(body).toEqual({ error: "insufficient_permission" });
    });

    it("rejects a write-capable access key provisioning a second key with access_key_cannot_issue_keys, creating no rows - the exploit path", async () => {
      const writeCapableKey = await requestJson<{ grant_id: string; device_id: string; token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/grants`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { scope_node_id: ctx.context.rootNodeId, name: "Write Capable Agent", can_read: true, can_write: true },
      });
      const beforeCount = await ctx.row<{ count: number }>("SELECT COUNT(*) AS count FROM folder_grants WHERE folder_id = ?", ctx.context.folderId);

      const response = await ctx.app.request(`/api/folders/${ctx.context.folderId}/grants`, {
        method: "POST",
        body: JSON.stringify({ scope_node_id: ctx.context.rootNodeId, name: "Sub Agent" }),
        headers: { "content-type": "application/json", authorization: `Bearer ${writeCapableKey.token}` },
      });
      const body = await response.json();
      const afterCount = await ctx.row<{ count: number }>("SELECT COUNT(*) AS count FROM folder_grants WHERE folder_id = ?", ctx.context.folderId);

      expect(response.status).toBe(403);
      expect(body).toEqual({ error: "access_key_cannot_issue_keys" });
      expect(afterCount?.count).toBe(beforeCount?.count);
    });

    it("rejects a write-capable access key revoking a sibling grant with access_key_cannot_revoke, leaving it and its token_hash untouched - the exploit path", async () => {
      const writeCapableKey = await requestJson<{ grant_id: string; device_id: string; token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/grants`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { scope_node_id: ctx.context.rootNodeId, name: "Write Capable Agent Two", can_read: true, can_write: true },
      });
      const siblingGrant = await requestJson<{ grant_id: string; device_id: string; token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/grants`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { scope_node_id: ctx.context.rootNodeId, name: "Sibling Key", can_read: true, can_write: true },
      });
      const beforeTokenHash = await ctx.row<{ token_hash: string }>("SELECT token_hash FROM devices WHERE device_id = ?", siblingGrant.device_id);

      const response = await ctx.app.request(`/api/folders/${ctx.context.folderId}/grants/${siblingGrant.grant_id}`, {
        method: "DELETE",
        headers: { authorization: `Bearer ${writeCapableKey.token}` },
      });
      const body = await response.json();

      const afterGrant = await ctx.row("SELECT grant_id FROM folder_grants WHERE grant_id = ?", siblingGrant.grant_id);
      const afterTokenHash = await ctx.row<{ token_hash: string }>("SELECT token_hash FROM devices WHERE device_id = ?", siblingGrant.device_id);

      expect(response.status).toBe(403);
      expect(body).toEqual({ error: "access_key_cannot_revoke" });
      expect(afterGrant).toBeTruthy();
      expect(afterTokenHash?.token_hash).toBe(beforeTokenHash?.token_hash);
    });

    it("owner sees every grant on the folder via GET /api/folders/:id/grants, including one issued to another principal", async () => {
      const deviceGrant = await requestJson<{ grant_id: string; device_id: string; token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/grants`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { scope_node_id: ctx.context.rootNodeId, name: `Folder Grants Agent ${crypto.randomUUID()}`, can_read: true, can_write: true },
      });

      const rows = await requestJson<Array<{ grant_id: string; name: string | null; created_by_email: string | null }>>(
        ctx.app,
        `/api/folders/${ctx.context.folderId}/grants`,
        { cookie: ctx.context.cookie },
      );

      expect(rows.length).toBeGreaterThanOrEqual(2);
      expect(rows.find((item) => item.grant_id === deviceGrant.grant_id)).toMatchObject({
        name: expect.stringContaining("Folder Grants Agent"),
        created_by_email: `${ctx.context.userId}@example.com`,
      });
    });

    it("returns 403 access_key_cannot_list_grants for an access key on GET /api/folders/:id/grants, with no email in the body", async () => {
      const accessKey = await requestJson<{ grant_id: string; device_id: string; token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/grants`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { scope_node_id: ctx.context.rootNodeId, name: `List Attempt Agent ${crypto.randomUUID()}`, can_read: true, can_write: true },
      });

      const response = await ctx.app.request(`/api/folders/${ctx.context.folderId}/grants`, {
        headers: { authorization: `Bearer ${accessKey.token}` },
      });
      const text = await response.text();

      expect(response.status).toBe(403);
      expect(JSON.parse(text)).toEqual({ error: "access_key_cannot_list_grants" });
      expect(text).not.toContain("@example.com");
    });

    it("regenerates an access key atomically, deleting the old grant before inserting the replacement", async () => {
      const original = await requestJson<{ grant_id: string; device_id: string; token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/grants`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { scope_node_id: ctx.context.rootNodeId, name: `Rotate Me ${crypto.randomUUID()}`, can_read: true, can_write: true },
      });

      const regenerated = await requestJson<{ grant_id: string; device_id: string; token: string }>(
        ctx.app,
        `/api/folders/${ctx.context.folderId}/grants/${original.grant_id}/regenerate`,
        { method: "POST", cookie: ctx.context.cookie },
      );

      expect(regenerated.grant_id).not.toBe(original.grant_id);
      const oldGrant = await ctx.row("SELECT grant_id FROM folder_grants WHERE grant_id = ?", original.grant_id);
      const oldDevice = await ctx.row<{ token_hash: string }>("SELECT token_hash FROM devices WHERE device_id = ?", original.device_id);
      expect(oldGrant).toBeFalsy();
      expect(oldDevice?.token_hash).toBe(`revoked:${original.grant_id}`);

      const staleRequest = await ctx.app.request("/api/grants", { headers: { authorization: `Bearer ${original.token}` } });
      expect([401, 403]).toContain(staleRequest.status);

      const freshRequest = await ctx.app.request("/api/grants", { headers: { authorization: `Bearer ${regenerated.token}` } });
      expect(freshRequest.status).toBe(200);
    });
  });
}
