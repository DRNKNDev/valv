import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { requestJson } from "./helpers.js";
import type { SeededHarness } from "./types.js";

export function inviteScenarios(harness: SeededHarness): void {
  describe("invite API", () => {
    let ctx: Awaited<ReturnType<SeededHarness["createApp"]>>;

    beforeAll(async () => {
      ctx = await harness.createApp();
    });

    afterAll(async () => ctx?.cleanup());

    it("creates, accepts, idempotently re-accepts, and rejects expired invites", async () => {
      const invite = await requestJson<{ invite_token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/invites`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { invited_email: "friend@example.com" },
      });
      expect(await ctx.row("SELECT invite_token FROM folder_invites WHERE invite_token = ?", invite.invite_token)).toBeTruthy();

      const signup = await ctx.app.request("/api/auth/sign-up/email", {
        method: "POST",
        body: JSON.stringify({ name: "Friend", email: "friend@example.com", password: "password1234" }),
        headers: { "content-type": "application/json" },
      });
      const friendCookie = signup.headers.get("set-cookie")?.split(";")[0];
      expect(friendCookie).toContain("better-auth.session_token");

      const first = await requestJson<{ accepted: boolean }>(ctx.app, `/api/invites/${invite.invite_token}/accept`, {
        method: "POST",
        cookie: friendCookie,
      });
      const second = await requestJson<{ accepted: boolean }>(ctx.app, `/api/invites/${invite.invite_token}/accept`, {
        method: "POST",
        cookie: friendCookie,
      });
      expect(first.accepted).toBe(true);
      expect(second.accepted).toBe(true);
      expect((await ctx.row<{ count: number }>("SELECT COUNT(*) AS count FROM folder_grants WHERE user_id IS NOT NULL AND folder_id = ?", ctx.context.folderId))?.count).toBe(2);

      const expired = await requestJson<{ invite_token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/invites`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { invited_email: "late@example.com" },
      });
      await ctx.exec("UPDATE folder_invites SET expires_at = ? WHERE invite_token = ?", Date.now() - 1000, expired.invite_token);
      const expiredResponse = await ctx.app.request(`/api/invites/${expired.invite_token}/accept`, {
        method: "POST",
        headers: { cookie: friendCookie ?? "" },
      });
      expect(expiredResponse.status).toBe(410);
    });

    it("creates a read-only invite when can_write is false, and acceptance grants read-only access", async () => {
      const invite = await requestJson<{ invite_token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/invites`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { invited_email: "readonly-friend@example.com", can_write: false },
      });
      expect(
        (await ctx.row<{ can_write: number | boolean }>("SELECT can_write FROM folder_invites WHERE invite_token = ?", invite.invite_token))
          ?.can_write,
      ).toBeFalsy();

      const signup = await ctx.app.request("/api/auth/sign-up/email", {
        method: "POST",
        body: JSON.stringify({ name: "Readonly Friend", email: "readonly-friend@example.com", password: "password1234" }),
        headers: { "content-type": "application/json" },
      });
      const friendCookie = signup.headers.get("set-cookie")?.split(";")[0];

      const accept = await requestJson<{ accepted: boolean }>(ctx.app, `/api/invites/${invite.invite_token}/accept`, {
        method: "POST",
        cookie: friendCookie,
      });
      expect(accept.accepted).toBe(true);

      const grant = await ctx.row<{ can_read: number | boolean; can_write: number | boolean }>(
        `SELECT fg.can_read AS can_read, fg.can_write AS can_write
         FROM folder_grants fg
         JOIN "user" u ON u.id = fg.user_id
         WHERE fg.folder_id = ? AND u.email = ?`,
        ctx.context.folderId,
        "readonly-friend@example.com",
      );
      expect(grant?.can_read).toBeTruthy();
      expect(grant?.can_write).toBeFalsy();
    });

    it("rejects invite creation from a read-only grant holder", async () => {
      const readOnlyGrant = await requestJson<{ grant_id: string; device_id: string; token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/grants`, {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { scope_node_id: ctx.context.rootNodeId, name: "Read Only Agent", can_read: true, can_write: false },
      });

      const response = await ctx.app.request(`/api/folders/${ctx.context.folderId}/invites`, {
        method: "POST",
        body: JSON.stringify({ invited_email: "should-fail@example.com" }),
        headers: { "content-type": "application/json", authorization: `Bearer ${readOnlyGrant.token}` },
      });

      expect(response.status).toBe(403);
    });
  });
}
