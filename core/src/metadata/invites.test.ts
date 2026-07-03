import { describe, expect, it, vi } from "vitest";

import { grant, LifecycleDb, metadataAppFor } from "../../tests/support.js";

describe("invite routes", () => {
  it("defaults invite scope to folder root and does not roll back when email fails", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("root");
    const sendInviteEmail = vi.fn(async () => {
      throw new Error("smtp unavailable");
    });
    const app = metadataAppFor(db, { type: "user", userId: "user-1" }, { sendInviteEmail });

    const response = await app.request("/folders/folder-1/invites", {
      method: "POST",
      body: JSON.stringify({ invited_email: "friend@example.com" }),
      headers: { "content-type": "application/json" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.invite_token).toEqual(expect.any(String));
    expect(db.folderInvites).toHaveLength(1);
    expect(db.folderInvites[0]).toMatchObject({ scopeNodeId: "root", invitedEmail: "friend@example.com" });
    expect(sendInviteEmail).toHaveBeenCalledWith({
      to: "friend@example.com",
      inviteToken: body.invite_token,
      folderName: "Projects",
    });
  });

  it("accepts invites idempotently and grants the accepting user", async () => {
    const db = new LifecycleDb();
    db.folderInvites.push({
      inviteToken: "token-1",
      folderId: "folder-1",
      scopeNodeId: "root",
      invitedEmail: "friend@example.com",
      invitedByUserId: "user-1",
      status: "pending",
      expiresAt: new Date(Date.now() + 60_000),
    });
    const app = metadataAppFor(db, { type: "user", userId: "user-2" });

    const first = await app.request("/invites/token-1/accept", { method: "POST" });
    const second = await app.request("/invites/token-1/accept", { method: "POST" });

    expect(first.status).toBe(200);
    expect(second.status).toBe(200);
    expect(db.folderGrants).toHaveLength(1);
    expect(db.folderGrants[0]).toMatchObject({ userId: "user-2", deviceId: null, scopeNodeId: "root" });
  });

  it("rejects invite creation from a read-only grant holder", async () => {
    const db = new LifecycleDb();
    db.folderGrants.push(grant("grant-readonly", { scopeNodeId: "root", userId: "user-1", canWrite: false }));
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/invites", {
      method: "POST",
      body: JSON.stringify({ invited_email: "friend@example.com" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(403);
    expect(db.folderInvites).toHaveLength(0);
  });

  it("rejects expired invites with 410", async () => {
    const db = new LifecycleDb();
    db.folderInvites.push({
      inviteToken: "expired",
      folderId: "folder-1",
      scopeNodeId: "root",
      invitedEmail: "friend@example.com",
      invitedByUserId: "user-1",
      status: "pending",
      expiresAt: new Date(Date.now() - 60_000),
    });

    const response = await metadataAppFor(db, { type: "user", userId: "user-2" }).request("/invites/expired/accept", {
      method: "POST",
    });

    expect(response.status).toBe(410);
    expect(db.folderGrants).toHaveLength(0);
  });
});
