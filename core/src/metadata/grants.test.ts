import { describe, expect, it, vi } from "vitest";

import { sha256Hex } from "../auth/index.js";
import { grant, LifecycleDb, metadataAppFor } from "../../tests/support.js";

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

  it("rejects grant revocation from a read-only grant holder", async () => {
    const db = new LifecycleDb();
    db.folderGrants.push(grant("grant-target", { deviceId: "device-2", scopeNodeId: "work" }));
    db.authorizeScope("work", { canWrite: false });
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants/grant-target", { method: "DELETE" });

    expect(response.status).toBe(403);
    expect(db.folderGrants).toHaveLength(1);
    expect(db.folderGrants[0]?.grantId).toBe("grant-target");
  });

  it("returns 400 when agent grant scope_node_id is present but non-string", async () => {
    const response = await metadataAppFor(new LifecycleDb(), { type: "user", userId: "user-1" }).request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ scope_node_id: null, name: "Agent" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(400);
    await expect(response.json()).resolves.toEqual({ error: "invalid_scope_node_id" });
  });

  it("defaults an omitted agent grant scope_node_id to the folder root", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("root");
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ name: "Agent", can_read: true, can_write: false }),
      headers: { "content-type": "application/json" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body).toMatchObject({ grant_id: expect.any(String), device_id: expect.any(String), token: expect.any(String) });
    expect(db.folderGrants[0]).toMatchObject({ folderId: "folder-1", scopeNodeId: "root", deviceId: body.device_id });
  });

  it("returns 404 when omitted scope_node_id cannot resolve a folder root", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("root");
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/missing-folder/grants", {
      method: "POST",
      body: JSON.stringify({ name: "Agent" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(404);
    await expect(response.json()).resolves.toEqual({ error: "folder_not_found" });
    expect(db.devices).toHaveLength(0);
    expect(db.folderGrants).toHaveLength(0);
  });

  it("rejects agent grant provisioning from a read-only grant holder", async () => {
    const db = new LifecycleDb();
    db.folderGrants.push(grant("grant-readonly", { scopeNodeId: "work", userId: "user-1", canWrite: false }));
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ scope_node_id: "work", name: "Agent" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(403);
    expect(db.devices).toHaveLength(0);
    expect(db.folderGrants).toHaveLength(1);
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

  it("fires onGrantCreated with the new folder, grant, and device ids", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("root");
    const onGrantCreated = vi.fn(async () => undefined);
    const app = metadataAppFor(db, { type: "user", userId: "user-1" }, { onGrantCreated });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ name: "Agent", can_read: true, can_write: false }),
      headers: { "content-type": "application/json" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(onGrantCreated).toHaveBeenCalledWith({
      folderId: "folder-1",
      grantId: body.grant_id,
      deviceId: body.device_id,
    });
  });

  it("does not fail grant creation when onGrantCreated rejects", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("root");
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const app = metadataAppFor(db, { type: "user", userId: "user-1" }, {
      onGrantCreated: vi.fn(async () => {
        throw new Error("boom");
      }),
    });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ name: "Agent" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(200);
    expect(consoleError).toHaveBeenCalledWith("onGrantCreated hook failed", expect.any(Error));
    consoleError.mockRestore();
  });

  it("revokes device tokens when deleting through the full route-store hooks", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "device-2", userId: null, name: "Agent", tokenHash: "active-token" });
    db.folderGrants.push(grant("grant-target", { deviceId: "device-2", scopeNodeId: "work" }));
    db.authorizedScopes.add("work");
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants/grant-target", { method: "DELETE" });

    expect(response.status).toBe(204);
    expect(db.folderGrants).toHaveLength(0);
    expect(db.devices[0]?.tokenHash).toBe("revoked:grant-target");
  });

  it("fails loudly when a legacy scope hook is present without a delete hook", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "device-2", userId: null, name: "Agent", tokenHash: "active-token" });
    db.folderGrants.push(grant("grant-target", { deviceId: "device-2", scopeNodeId: "work" }));
    db.authorizedScopes.add("work");
    (db as Partial<LifecycleDb>).getGrantForRoute = undefined;
    (db as Partial<LifecycleDb>).deleteGrantForRoute = undefined;
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants/grant-target", { method: "DELETE" });

    expect(response.status).toBe(500);
    await expect(response.json()).resolves.toEqual({ error: "incomplete_grant_route_store" });
    expect(db.folderGrants).toHaveLength(1);
    expect(db.devices[0]?.tokenHash).toBe("active-token");
  });

  it("fails loudly when legacy scope and delete hooks are present without a grant loader", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "device-2", userId: null, name: "Agent", tokenHash: "active-token" });
    db.folderGrants.push(grant("grant-target", { deviceId: "device-2", scopeNodeId: "work" }));
    db.authorizedScopes.add("work");
    (db as Partial<LifecycleDb>).getGrantForRoute = undefined;
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants/grant-target", { method: "DELETE" });

    expect(response.status).toBe(500);
    await expect(response.json()).resolves.toEqual({ error: "incomplete_grant_route_store" });
    expect(db.folderGrants).toHaveLength(1);
    expect(db.devices[0]?.tokenHash).toBe("active-token");
  });

  it("revokes device tokens on the default delete path when no grant route hooks are supplied", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "device-2", userId: null, name: "Agent", tokenHash: "active-token" });
    db.folderGrants.push(grant("grant-target", { deviceId: "device-2", scopeNodeId: "work" }));
    db.authorizedScopes.add("work");
    (db as Partial<LifecycleDb>).createAgentGrantForRoute = undefined;
    (db as Partial<LifecycleDb>).getGrantForRoute = undefined;
    (db as Partial<LifecycleDb>).getGrantScopeForRoute = undefined;
    (db as Partial<LifecycleDb>).deleteGrantForRoute = undefined;
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants/grant-target", { method: "DELETE" });

    expect(response.status).toBe(204);
    expect(db.folderGrants).toHaveLength(0);
    expect(db.devices[0]?.tokenHash).toBe("revoked:grant-target");
  });
});
