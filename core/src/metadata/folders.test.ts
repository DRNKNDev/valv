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

  it("rejects device principals when creating folders", async () => {
    const response = await metadataAppFor(new LifecycleDb(), { type: "device", deviceId: "device-1" }).request("/folders", {
      method: "POST",
      headers: { authorization: "Bearer device-token" },
    });

    expect(response.status).toBe(403);
    await expect(response.json()).resolves.toEqual({ error: "user_required" });
  });

  it("filters GET /grants by user or device principal", async () => {
    const db = new LifecycleDb();
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
    expect((await deviceResponse.json()).map((item: any) => item.grant_id)).toEqual(["grant-device"]);
  });
});
