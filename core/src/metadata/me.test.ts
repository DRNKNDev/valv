import { describe, expect, it } from "vitest";

import { grant, LifecycleDb, metadataAppFor } from "../../tests/support.js";

describe("GET /me", () => {
  it("reports account with an email for a user principal", async () => {
    const db = new LifecycleDb();
    db.users.push({ id: "user-1", email: "alice@example.com" });
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/me");
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body).toEqual({ type: "account", email: "alice@example.com", scopes: [] });
  });

  it("reports account with no scopes for a human-registered device", async () => {
    const db = new LifecycleDb();
    db.users.push({ id: "user-1", email: "alice@example.com" });
    db.devices.push({ deviceId: "mac-1", userId: "user-1", name: "Mac", tokenHash: "hash" });
    const app = metadataAppFor(db, { type: "device", deviceId: "mac-1" });

    const response = await app.request("/me", { headers: { authorization: "Bearer device-token" } });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body).toEqual({ type: "account", email: "alice@example.com", scopes: [] });
  });

  it("reports access_key with its scopes and no email for an access key, including the legacy device_token shape", async () => {
    const db = new LifecycleDb();
    db.sharedFolders.push({ folderId: "folder-1", name: "Design", ownerUserId: "owner-1" });
    db.devices.push({ deviceId: "agent-1", userId: null, name: "Agent", tokenHash: "hash" });
    db.folderGrants.push(grant("grant-agent", { deviceId: "agent-1", folderId: "folder-1", scopeNodeId: "root", canWrite: true }));
    const app = metadataAppFor(db, { type: "device", deviceId: "agent-1" });

    const response = await app.request("/me", { headers: { authorization: "Bearer device-token" } });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.type).toBe("access_key");
    expect(body.email).toBeUndefined();
    expect(body.scopes).toEqual([{ folder_id: "folder-1", folder_name: "Design", scope_label: "Design", can_write: true }]);
  });

  it("labels a subtree-scoped access key with the node's own name, not the folder's", async () => {
    const db = new LifecycleDb();
    db.sharedFolders.push({ folderId: "folder-1", name: "Design", ownerUserId: "owner-1" });
    db.devices.push({ deviceId: "agent-1", userId: null, name: "Agent", tokenHash: "hash" });
    db.folderGrants.push(grant("grant-agent", { deviceId: "agent-1", folderId: "folder-1", scopeNodeId: "work", canWrite: false }));
    const app = metadataAppFor(db, { type: "device", deviceId: "agent-1" });

    const response = await app.request("/me", { headers: { authorization: "Bearer device-token" } });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.scopes).toEqual([{ folder_id: "folder-1", folder_name: "Design", scope_label: "work", can_write: false }]);
  });
});
