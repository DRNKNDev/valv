import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { cleanupAppContext, createAppContext, row } from "../setup/api.js";
import { requestJson } from "../setup/helpers.js";

describe("invite API", () => {
  let ctx: Awaited<ReturnType<typeof createAppContext>>;

  beforeAll(async () => {
    ctx = await createAppContext();
  });

  afterAll(async () => cleanupAppContext(ctx));

  it("creates, accepts, idempotently re-accepts, and rejects expired invites", async () => {
    const invite = await requestJson<{ invite_token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/invites`, {
      method: "POST",
      cookie: ctx.context.cookie,
      body: { invited_email: "friend@example.com" },
    });
    expect(row(ctx.sqlite, "SELECT invite_token FROM folder_invites WHERE invite_token = ?", invite.invite_token)).toBeTruthy();

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
    expect(row<{ count: number }>(ctx.sqlite, "SELECT COUNT(*) AS count FROM folder_grants WHERE user_id IS NOT NULL AND folder_id = ?", ctx.context.folderId)?.count).toBe(2);

    const expired = await requestJson<{ invite_token: string }>(ctx.app, `/api/folders/${ctx.context.folderId}/invites`, {
      method: "POST",
      cookie: ctx.context.cookie,
      body: { invited_email: "late@example.com" },
    });
    ctx.sqlite.prepare("UPDATE folder_invites SET expires_at = ? WHERE invite_token = ?").run(Date.now() - 1000, expired.invite_token);
    const expiredResponse = await ctx.app.request(`/api/invites/${expired.invite_token}/accept`, {
      method: "POST",
      headers: { cookie: friendCookie ?? "" },
    });
    expect(expiredResponse.status).toBe(410);
  });
});
