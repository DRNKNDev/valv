import { describe, expect, it } from "vitest";

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
    expect(db.folderGrants[0]).toMatchObject({ userId: "user-1", deviceId: null, role: "owner" });
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
    expect(db.folderGrants[0]).toMatchObject({ userId: "user-1", deviceId: null, role: "owner" });
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
