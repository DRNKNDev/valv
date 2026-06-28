import { describe, expect, it } from "vitest";

import { grant, LifecycleDb, metadataAppFor } from "../../tests/test-helper.js";

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
});
