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
      canWrite: true,
      status: "pending",
      expiresAt: new Date(Date.now() + 60_000),
    });
    const app = metadataAppFor(db, { type: "user", userId: "user-2" });

    const first = await app.request("/invites/token-1/accept", { method: "POST" });
    const second = await app.request("/invites/token-1/accept", { method: "POST" });

    expect(first.status).toBe(200);
    expect(second.status).toBe(200);
    expect(db.folderGrants).toHaveLength(1);
    expect(db.folderGrants[0]).toMatchObject({ userId: "user-2", deviceId: null, scopeNodeId: "root", canWrite: true });
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

  it("defaults invite can_write to true when omitted", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("root");
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/invites", {
      method: "POST",
      body: JSON.stringify({ invited_email: "friend@example.com" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(200);
    expect(db.folderInvites[0]).toMatchObject({ canWrite: true });
  });

  it("creates a read-only invite when can_write is false", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("root");
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/invites", {
      method: "POST",
      body: JSON.stringify({ invited_email: "friend@example.com", can_write: false }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(200);
    expect(db.folderInvites[0]).toMatchObject({ canWrite: false });
  });

  it("attributes invites created by user principals to the user id", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("root");
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/invites", {
      method: "POST",
      body: JSON.stringify({ invited_email: "friend@example.com" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(200);
    expect(db.folderInvites[0]).toMatchObject({ invitedByUserId: "user-1" });
  });

  it("attributes invites created by human devices to the device owner", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "device-1", userId: "user-1", name: "Laptop", tokenHash: "hash" });
    db.folderGrants.push(grant("grant-owner", { scopeNodeId: "root", userId: "user-1", canWrite: true }));
    const app = metadataAppFor(db, { type: "device", deviceId: "device-1" });

    const response = await app.request("/folders/folder-1/invites", {
      method: "POST",
      body: JSON.stringify({ invited_email: "friend@example.com" }),
      headers: { authorization: "Bearer device-token", "content-type": "application/json" },
    });

    expect(response.status).toBe(200);
    expect(db.folderInvites[0]).toMatchObject({ invitedByUserId: "user-1" });
    expect(db.folderInvites[0]?.invitedByUserId).not.toBe("device-1");
  });

  it("rejects invite creation from agent devices even with write-capable grants", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "agent-1", userId: null, name: "Agent", tokenHash: "hash" });
    db.folderGrants.push(grant("grant-agent", { scopeNodeId: "root", deviceId: "agent-1", canWrite: true }));
    const app = metadataAppFor(db, { type: "device", deviceId: "agent-1" });

    const response = await app.request("/folders/folder-1/invites", {
      method: "POST",
      body: JSON.stringify({ invited_email: "friend@example.com" }),
      headers: { authorization: "Bearer device-token", "content-type": "application/json" },
    });

    expect(response.status).toBe(403);
    await expect(response.json()).resolves.toEqual({ error: "agent_devices_cannot_create_invites" });
    expect(db.folderInvites).toHaveLength(0);
  });

  it("accepting a read-only invite grants a read-only, not read-write, scope", async () => {
    const db = new LifecycleDb();
    db.folderInvites.push({
      inviteToken: "readonly-token",
      folderId: "folder-1",
      scopeNodeId: "root",
      invitedEmail: "friend@example.com",
      invitedByUserId: "user-1",
      canWrite: false,
      status: "pending",
      expiresAt: new Date(Date.now() + 60_000),
    });
    const app = metadataAppFor(db, { type: "user", userId: "user-2" });

    const response = await app.request("/invites/readonly-token/accept", { method: "POST" });

    expect(response.status).toBe(200);
    expect(db.folderGrants[0]).toMatchObject({ canWrite: false, canRead: true });
  });

  it("rejects expired invites with 410", async () => {
    const db = new LifecycleDb();
    db.folderInvites.push({
      inviteToken: "expired",
      folderId: "folder-1",
      scopeNodeId: "root",
      invitedEmail: "friend@example.com",
      invitedByUserId: "user-1",
      canWrite: true,
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
