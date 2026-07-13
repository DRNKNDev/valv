import { describe, expect, it, vi } from "vitest";

import { grant, LifecycleDb, metadataAppFor } from "../../tests/support.js";

describe("folder routes", () => {
  it("creates a folder root and owner grant atomically", async () => {
    const db = new LifecycleDb();
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders", {
      method: "POST",
      body: JSON.stringify({ name: "Projects" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(200);
    expect(db.sharedFolders).toHaveLength(1);
    const createdRoot = db.nodes.find((node) => node.folderId === db.sharedFolders[0]?.folderId && node.parentId === null);
    expect(createdRoot).toBeDefined();
    expect(db.folderGrants).toHaveLength(1);
    expect(db.folderGrants[0]).toMatchObject({ userId: "user-1", deviceId: null, role: "owner", createdByUserId: "user-1" });
    expect(db.folderGrants[0]?.scopeNodeId).toBe(createdRoot?.nodeId);
  });

  it("allows human-registered devices to create user-owned folders", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "device-1", userId: "user-1", name: "Mac", tokenHash: "hash" });

    const response = await metadataAppFor(db, { type: "device", deviceId: "device-1" }).request("/folders", {
      method: "POST",
      body: JSON.stringify({ name: "Projects" }),
      headers: { authorization: "Bearer device-token", "content-type": "application/json" },
    });

    expect(response.status).toBe(200);
    expect(db.folderGrants[0]).toMatchObject({ userId: "user-1", deviceId: null, role: "owner", createdByUserId: "user-1" });
    expect(db.folderGrants[0]?.createdByUserId).not.toBe("device-1");
  });

  it("fires onFolderCreated with the new folder, owner, and grant", async () => {
    const db = new LifecycleDb();
    const onFolderCreated = vi.fn(async () => undefined);
    const app = metadataAppFor(db, { type: "user", userId: "user-1" }, { onFolderCreated });

    const response = await app.request("/folders", {
      method: "POST",
      body: JSON.stringify({ name: "Projects" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(200);
    expect(onFolderCreated).toHaveBeenCalledWith({
      folderId: db.sharedFolders[0]?.folderId,
      ownerUserId: "user-1",
      grantId: db.folderGrants[0]?.grantId,
    });
  });

  it("does not fail folder creation when onFolderCreated rejects", async () => {
    const db = new LifecycleDb();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const app = metadataAppFor(db, { type: "user", userId: "user-1" }, {
      onFolderCreated: vi.fn(async () => {
        throw new Error("link failed");
      }),
    });

    const response = await app.request("/folders", {
      method: "POST",
      body: JSON.stringify({ name: "Projects" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(200);
    expect(db.sharedFolders).toHaveLength(1);
    expect(db.folderGrants).toHaveLength(1);
    expect(consoleError).toHaveBeenCalledWith("onFolderCreated hook failed", expect.any(Error));
    consoleError.mockRestore();
  });

  it("keeps no-hook self-hosted folder creation behavior unchanged", async () => {
    const db = new LifecycleDb();
    const response = await metadataAppFor(db, { type: "user", userId: "user-1" }).request("/folders", {
      method: "POST",
      body: JSON.stringify({ name: "Projects" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(200);
    expect(db.sharedFolders).toHaveLength(1);
    expect(db.folderGrants).toHaveLength(1);
  });

  it("rejects agent devices when creating folders", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "device-1", userId: null, name: "Agent", tokenHash: "hash" });

    const response = await metadataAppFor(db, { type: "device", deviceId: "device-1" }).request("/folders", {
      method: "POST",
      headers: { authorization: "Bearer device-token" },
    });

    expect(response.status).toBe(403);
    await expect(response.json()).resolves.toEqual({ error: "agent_devices_cannot_create_folders" });
  });

  it("filters GET /grants by user or device principal", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "device-1", userId: "user-1", name: "Mac", tokenHash: "hash" });
    db.folderGrants.push(
      grant("grant-user", { userId: "user-1", scopeNodeId: "root" }),
      grant("grant-device", { deviceId: "device-1", scopeNodeId: "root" }),
      grant("grant-other", { userId: "user-2", scopeNodeId: "root" }),
    );

    const userResponse = await metadataAppFor(db, { type: "user", userId: "user-1" }).request("/grants");
    const deviceResponse = await metadataAppFor(db, { type: "device", deviceId: "device-1" }).request("/grants", {
      headers: { authorization: "Bearer device-token" },
    });

    expect((await userResponse.json()).map((item: any) => item.grant_id)).toEqual(["grant-user"]);
    expect((await deviceResponse.json()).map((item: any) => item.grant_id)).toEqual(["grant-user", "grant-device"]);
  });

  it("GET /grants includes grantee_email for a user-held grant and device_name for a device-held grant", async () => {
    const db = new LifecycleDb();
    db.users.push({ id: "user-1", email: "alice@example.com" });
    db.devices.push({ deviceId: "device-1", userId: null, name: "CI Agent", tokenHash: "hash" });
    db.folderGrants.push(
      grant("grant-user", { userId: "user-1", scopeNodeId: "root" }),
      grant("grant-device", { deviceId: "device-1", scopeNodeId: "root" }),
    );

    const userResponse = await metadataAppFor(db, { type: "user", userId: "user-1" }).request("/grants");
    const deviceResponse = await metadataAppFor(db, { type: "device", deviceId: "device-1" }).request("/grants", {
      headers: { authorization: "Bearer device-token" },
    });

    expect(userResponse.status).toBe(200);
    expect(deviceResponse.status).toBe(200);
    const userGrant = (await userResponse.json()).find((item: any) => item.grant_id === "grant-user");
    const deviceGrant = (await deviceResponse.json()).find((item: any) => item.grant_id === "grant-device");
    expect(userGrant).toMatchObject({ user_id: "user-1", device_id: null, grantee_email: "alice@example.com", device_name: null });
    expect(deviceGrant).toMatchObject({ user_id: null, device_id: "device-1", grantee_email: null, device_name: "CI Agent" });
  });

  it("GET /grants includes name from folder_grants", async () => {
    const db = new LifecycleDb();
    db.folderGrants.push(grant("grant-user", { userId: "user-1", scopeNodeId: "root", name: "build-01" }));

    const response = await metadataAppFor(db, { type: "user", userId: "user-1" }).request("/grants");
    const body = await response.json();

    expect(body[0]).toMatchObject({ name: "build-01" });
  });

  it("GET /folders/:id/grants returns every grant on the folder, and a collaborator-provisioned key carries that collaborator's created_by_email", async () => {
    const db = new LifecycleDb();
    db.users.push({ id: "owner-1", email: "owner@example.com" });
    db.users.push({ id: "collab-1", email: "collab@example.com" });
    db.folderGrants.push(grant("grant-owner", { userId: "owner-1", scopeNodeId: "root", role: "owner", createdByUserId: "owner-1" }));
    db.folderGrants.push(grant("grant-collab", { userId: "collab-1", scopeNodeId: "root", createdByUserId: "owner-1" }));
    await db.createAgentGrantForRoute({
      folderId: "folder-1",
      scopeNodeId: "root",
      deviceId: "device-1",
      grantId: "grant-key",
      tokenHash: "hash",
      name: "build-01",
      canRead: true,
      canWrite: true,
      createdByUserId: "collab-1",
    });
    db.authorizedScopes.add("root");

    const response = await metadataAppFor(db, { type: "user", userId: "owner-1" }).request("/folders/folder-1/grants");
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body).toHaveLength(3);
    const ownerRow = body.find((item: any) => item.grant_id === "grant-owner");
    const collabRow = body.find((item: any) => item.grant_id === "grant-collab");
    const keyRow = body.find((item: any) => item.grant_id === "grant-key");
    expect(ownerRow).toMatchObject({ grantee_email: "owner@example.com", created_by_email: "owner@example.com" });
    expect(collabRow).toMatchObject({ grantee_email: "collab@example.com", created_by_email: "owner@example.com" });
    expect(keyRow).toMatchObject({ name: "build-01", device_id: "device-1", created_by_email: "collab@example.com" });
  });

  it("GET /folders/:id/grants returns 403 insufficient_permission for a read-only user", async () => {
    const db = new LifecycleDb();
    db.authorizeScope("root", { canWrite: false });

    const response = await metadataAppFor(db, { type: "user", userId: "user-1" }).request("/folders/folder-1/grants");

    expect(response.status).toBe(403);
    await expect(response.json()).resolves.toEqual({ error: "insufficient_permission" });
  });

  it("GET /folders/:id/grants returns 403 access_key_cannot_list_grants for an access key, with no email in the body", async () => {
    const db = new LifecycleDb();
    db.users.push({ id: "owner-1", email: "owner@example.com" });
    db.devices.push({ deviceId: "agent-1", userId: null, name: "Agent", tokenHash: "hash" });
    db.folderGrants.push(grant("grant-owner", { userId: "owner-1", scopeNodeId: "root" }));
    db.folderGrants.push(grant("grant-agent", { deviceId: "agent-1", scopeNodeId: "root", canWrite: true }));
    const app = metadataAppFor(db, { type: "device", deviceId: "agent-1" });

    const response = await app.request("/folders/folder-1/grants", { headers: { authorization: "Bearer device-token" } });
    const text = await response.text();

    expect(response.status).toBe(403);
    expect(JSON.parse(text)).toEqual({ error: "access_key_cannot_list_grants" });
    expect(text).not.toContain("owner@example.com");
  });

  it("GET /folders/:id/grants resolves a human-registered device to its user", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "mac-1", userId: "user-1", name: "Mac", tokenHash: "hash" });
    db.folderGrants.push(grant("grant-owner", { userId: "user-1", scopeNodeId: "root" }));
    const app = metadataAppFor(db, { type: "device", deviceId: "mac-1" });

    const response = await app.request("/folders/folder-1/grants", { headers: { authorization: "Bearer device-token" } });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.map((item: any) => item.grant_id)).toContain("grant-owner");
  });

  it("GET /folders/:id/grants returns 404 for an unknown folder", async () => {
    const response = await metadataAppFor(new LifecycleDb(), { type: "user", userId: "user-1" }).request("/folders/unknown-folder/grants");

    expect(response.status).toBe(404);
    await expect(response.json()).resolves.toEqual({ error: "folder_not_found" });
  });

  it("GET /folders/:id returns the folder's name for an authorized principal", async () => {
    const db = new LifecycleDb();
    db.sharedFolders.push({ folderId: "folder-1", name: "Design Docs", ownerUserId: "user-1" });
    db.authorizedScopes.add("root");

    const response = await metadataAppFor(db, { type: "user", userId: "user-1" }).request("/folders/folder-1");
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body).toEqual({ folder_id: "folder-1", name: "Design Docs" });
  });

  it("GET /folders/:id returns 403 for a principal with no covering grant", async () => {
    const db = new LifecycleDb();
    db.sharedFolders.push({ folderId: "folder-1", name: "Design Docs", ownerUserId: "user-1" });

    const response = await metadataAppFor(db, { type: "user", userId: "user-2" }).request("/folders/folder-1");

    expect(response.status).toBe(403);
  });

  it("GET /folders/:id returns 404 for an unknown folder_id", async () => {
    const db = new LifecycleDb();

    const response = await metadataAppFor(db, { type: "user", userId: "user-1" }).request("/folders/unknown-folder");

    expect(response.status).toBe(404);
  });
});
