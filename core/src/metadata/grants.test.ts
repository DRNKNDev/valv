import { describe, expect, it } from "vitest";

import { sha256Hex } from "../auth/index.js";
import { grant, LifecycleDb, metadataAppFor } from "../../tests/test-helper.js";

describe("grant routes", () => {
  it("requires covering authorization before revoking a grant", async () => {
    const db = new LifecycleDb();
    db.folderGrants.push(grant("grant-target", { deviceId: "device-2", scopeNodeId: "work" }));
    db.authorizedScopes.add("work");
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants/grant-target", { method: "DELETE" });

    expect(response.status).toBe(204);
    expect(db.folderGrants).toHaveLength(0);
  });

  it("denies grant revocation without covering authorization", async () => {
    const db = new LifecycleDb();
    db.folderGrants.push(grant("grant-target", { deviceId: "device-2", scopeNodeId: "work" }));
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants/grant-target", { method: "DELETE" });

    expect(response.status).toBe(403);
    expect(db.folderGrants).toHaveLength(1);
  });

  it("returns 400 when agent grant scope_node_id is missing", async () => {
    const response = await metadataAppFor(new LifecycleDb(), { type: "user", userId: "user-1" }).request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ name: "Agent" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(400);
    await expect(response.json()).resolves.toEqual({ error: "invalid_scope_node_id" });
  });

  it("provisions agent grants with null user_id and hashed token", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("work");
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ scope_node_id: "work", name: "Agent", can_read: true, can_write: false }),
      headers: { "content-type": "application/json" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.token).toEqual(expect.any(String));
    expect(db.devices[0]).toMatchObject({ userId: null, name: "Agent" });
    expect(db.devices[0]?.tokenHash).toBe(sha256Hex(body.token));
    expect(db.folderGrants[0]).toMatchObject({ deviceId: body.device_id, userId: null, canWrite: false });
  });
});
