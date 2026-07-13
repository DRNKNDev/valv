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

  it("rejects grant revocation from a read-only grant holder with insufficient_permission", async () => {
    const db = new LifecycleDb();
    db.folderGrants.push(grant("grant-target", { deviceId: "device-2", scopeNodeId: "work" }));
    db.authorizeScope("work", { canWrite: false });
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants/grant-target", { method: "DELETE" });

    expect(response.status).toBe(403);
    await expect(response.json()).resolves.toEqual({ error: "insufficient_permission" });
    expect(db.folderGrants).toHaveLength(1);
    expect(db.folderGrants[0]?.grantId).toBe("grant-target");
  });

  it("rejects a write-capable access key revoking a sibling key's grant, leaving the grant and token_hash untouched", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "agent-1", userId: null, name: "Agent", tokenHash: "agent-token-hash" });
    db.devices.push({ deviceId: "device-2", userId: null, name: "Sibling", tokenHash: "sibling-token-hash" });
    db.folderGrants.push(grant("grant-agent", { deviceId: "agent-1", scopeNodeId: "work", canWrite: true }));
    db.folderGrants.push(grant("grant-target", { deviceId: "device-2", scopeNodeId: "work" }));
    const app = metadataAppFor(db, { type: "device", deviceId: "agent-1" });

    const response = await app.request("/folders/folder-1/grants/grant-target", {
      method: "DELETE",
      headers: { authorization: "Bearer device-token" },
    });

    expect(response.status).toBe(403);
    await expect(response.json()).resolves.toEqual({ error: "access_key_cannot_revoke" });
    expect(db.folderGrants.find((item) => item.grantId === "grant-target")).toBeDefined();
    expect(db.devices.find((item) => item.deviceId === "device-2")?.tokenHash).toBe("sibling-token-hash");
  });

  it("rejects a write-capable access key revoking its own grant", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "agent-1", userId: null, name: "Agent", tokenHash: "agent-token-hash" });
    db.folderGrants.push(grant("grant-agent", { deviceId: "agent-1", scopeNodeId: "work", canWrite: true }));
    const app = metadataAppFor(db, { type: "device", deviceId: "agent-1" });

    const response = await app.request("/folders/folder-1/grants/grant-agent", {
      method: "DELETE",
      headers: { authorization: "Bearer device-token" },
    });

    expect(response.status).toBe(403);
    await expect(response.json()).resolves.toEqual({ error: "access_key_cannot_revoke" });
    expect(db.folderGrants.find((item) => item.grantId === "grant-agent")).toBeDefined();
    expect(db.devices.find((item) => item.deviceId === "agent-1")?.tokenHash).toBe("agent-token-hash");
  });

  it("allows a human-registered device to still revoke a grant", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "mac-1", userId: "user-1", name: "Mac", tokenHash: "mac-token-hash" });
    db.folderGrants.push(grant("grant-target", { deviceId: "device-2", scopeNodeId: "work" }));
    db.authorizedScopes.add("work");
    const app = metadataAppFor(db, { type: "device", deviceId: "mac-1" });

    const response = await app.request("/folders/folder-1/grants/grant-target", {
      method: "DELETE",
      headers: { authorization: "Bearer device-token" },
    });

    expect(response.status).toBe(204);
    expect(db.folderGrants).toHaveLength(0);
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

  it("rejects agent grant provisioning from a read-only grant holder with insufficient_permission", async () => {
    const db = new LifecycleDb();
    db.folderGrants.push(grant("grant-readonly", { scopeNodeId: "work", userId: "user-1", canWrite: false }));
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ scope_node_id: "work", name: "Agent" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(403);
    await expect(response.json()).resolves.toEqual({ error: "insufficient_permission" });
    expect(db.devices).toHaveLength(0);
    expect(db.folderGrants).toHaveLength(1);
  });

  it("rejects agent grant provisioning from a write-capable access key with access_key_cannot_issue_keys, creating no rows", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "agent-1", userId: null, name: "Agent", tokenHash: "hash" });
    db.folderGrants.push(grant("grant-agent", { scopeNodeId: "work", deviceId: "agent-1", canWrite: true }));
    const app = metadataAppFor(db, { type: "device", deviceId: "agent-1" });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ scope_node_id: "work", name: "Sub Agent" }),
      headers: { authorization: "Bearer device-token", "content-type": "application/json" },
    });

    expect(response.status).toBe(403);
    await expect(response.json()).resolves.toEqual({ error: "access_key_cannot_issue_keys" });
    expect(db.devices).toHaveLength(1);
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
    expect(db.folderGrants[0]).toMatchObject({
      deviceId: body.device_id,
      userId: null,
      canWrite: false,
      name: "Agent",
      createdByUserId: "user-1",
    });
  });

  it("records the human-registered device's user as provisioner, not the device", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "device-1", userId: "user-1", name: "Mac", tokenHash: "hash" });
    db.folderGrants.push(grant("grant-owner", { scopeNodeId: "work", userId: "user-1", canWrite: true }));
    const app = metadataAppFor(db, { type: "device", deviceId: "device-1" });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ scope_node_id: "work", name: "Agent" }),
      headers: { authorization: "Bearer device-token", "content-type": "application/json" },
    });

    expect(response.status).toBe(200);
    expect(db.folderGrants[1]?.createdByUserId).toBe("user-1");
    expect(db.folderGrants[1]?.createdByUserId).not.toBe("device-1");
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

  it("fires onGrantDeviceCreated after provisioning through the route-store branch", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("work");
    const onGrantDeviceCreated = vi.fn(async () => undefined);
    const app = metadataAppFor(db, { type: "user", userId: "user-1" }, { onGrantDeviceCreated });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ scope_node_id: "work", name: "Agent" }),
      headers: { "content-type": "application/json" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(onGrantDeviceCreated).toHaveBeenCalledWith({
      folderId: "folder-1",
      scopeNodeId: "work",
      deviceId: body.device_id,
      grantId: body.grant_id,
    });
  });

  it("fires onGrantDeviceCreated after provisioning through the fallback branch", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("work");
    (db as Partial<LifecycleDb>).createAgentGrantForRoute = undefined;
    const onGrantDeviceCreated = vi.fn(async () => undefined);
    const app = metadataAppFor(db, { type: "user", userId: "user-1" }, { onGrantDeviceCreated });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ scope_node_id: "work", name: "Agent" }),
      headers: { "content-type": "application/json" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(onGrantDeviceCreated).toHaveBeenCalledWith({
      folderId: "folder-1",
      scopeNodeId: "work",
      deviceId: body.device_id,
      grantId: body.grant_id,
    });
  });

  it("does not fail provisioning when onGrantDeviceCreated rejects", async () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const db = new LifecycleDb();
    db.authorizedScopes.add("work");
    const app = metadataAppFor(db, { type: "user", userId: "user-1" }, {
      onGrantDeviceCreated: vi.fn(async () => {
        throw new Error("side effect failed");
      }),
    });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ scope_node_id: "work", name: "Agent" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(200);
    expect(db.devices).toHaveLength(1);
    expect(db.folderGrants).toHaveLength(1);
    expect(consoleError).toHaveBeenCalledWith("onGrantDeviceCreated hook failed", expect.any(Error));
    consoleError.mockRestore();
  });

  it("blocks provisioning when checkPlanForGrant denies the folder", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("work");
    const app = metadataAppFor(db, { type: "user", userId: "user-1" }, {
      checkPlanForGrant: vi.fn(async () => ({ allowed: false, status: "canceled" })),
    });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ scope_node_id: "work", name: "Agent" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(402);
    await expect(response.json()).resolves.toEqual({ error: "subscription_inactive", status: "canceled" });
    expect(db.devices).toHaveLength(0);
    expect(db.folderGrants).toHaveLength(0);
  });

  it("allows provisioning when checkPlanForGrant approves the folder", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("work");
    const checkPlanForGrant = vi.fn(async () => ({ allowed: true }));
    const app = metadataAppFor(db, { type: "user", userId: "user-1" }, { checkPlanForGrant });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ scope_node_id: "work", name: "Agent" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(200);
    expect(checkPlanForGrant).toHaveBeenCalledWith("folder-1");
    expect(db.devices).toHaveLength(1);
    expect(db.folderGrants).toHaveLength(1);
  });

  it("does not call checkPlanForGrant when authorization fails", async () => {
    const db = new LifecycleDb();
    const checkPlanForGrant = vi.fn(async () => ({ allowed: true }));
    const app = metadataAppFor(db, { type: "user", userId: "user-1" }, { checkPlanForGrant });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ scope_node_id: "work", name: "Agent" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(403);
    expect(checkPlanForGrant).not.toHaveBeenCalled();
  });

  it("propagates createAgentGrantForRoute failures", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("work");
    db.createAgentGrantForRoute = async () => {
      throw new Error("tenant missing");
    };
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ scope_node_id: "work", name: "Agent" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(500);
    expect(db.devices).toHaveLength(0);
    expect(db.folderGrants).toHaveLength(0);
  });

  it("rejects a duplicate live access key name for the same folder", async () => {
    const db = new LifecycleDb();
    await db.createAgentGrantForRoute({
      folderId: "folder-1",
      scopeNodeId: "root",
      deviceId: "device-1",
      grantId: "grant-1",
      tokenHash: "hash-1",
      name: "build-01",
      canRead: true,
      canWrite: true,
      createdByUserId: "user-1",
    });

    await expect(
      db.createAgentGrantForRoute({
        folderId: "folder-1",
        scopeNodeId: "root",
        deviceId: "device-2",
        grantId: "grant-2",
        tokenHash: "hash-2",
        name: "build-01",
        canRead: true,
        canWrite: true,
        createdByUserId: "user-1",
      }),
    ).rejects.toThrow();
    expect(db.folderGrants).toHaveLength(1);
  });

  it("allows the same access key name on two different folders", async () => {
    const db = new LifecycleDb();
    await db.createAgentGrantForRoute({
      folderId: "folder-1",
      scopeNodeId: "root",
      deviceId: "device-1",
      grantId: "grant-1",
      tokenHash: "hash-1",
      name: "build-01",
      canRead: true,
      canWrite: true,
      createdByUserId: "user-1",
    });

    await db.createAgentGrantForRoute({
      folderId: "folder-2",
      scopeNodeId: "root-2",
      deviceId: "device-2",
      grantId: "grant-2",
      tokenHash: "hash-2",
      name: "build-01",
      canRead: true,
      canWrite: true,
      createdByUserId: "user-1",
    });

    expect(db.folderGrants).toHaveLength(2);
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

  it("deletes and revokes only the targeted grant and device on the raw fallback, leaving siblings untouched", async () => {
    const db = new LifecycleDb();
    db.devices.push({ deviceId: "device-2", userId: null, name: "Agent A", tokenHash: "active-token-a" });
    db.devices.push({ deviceId: "device-3", userId: null, name: "Agent B", tokenHash: "active-token-b" });
    db.folderGrants.push(grant("grant-target", { deviceId: "device-2", scopeNodeId: "work" }));
    db.folderGrants.push(grant("grant-other", { deviceId: "device-3", scopeNodeId: "work" }));
    db.authorizedScopes.add("work");
    (db as Partial<LifecycleDb>).deleteGrantForRoute = undefined;
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants/grant-target", { method: "DELETE" });

    expect(response.status).toBe(204);
    expect(db.folderGrants).toHaveLength(1);
    expect(db.folderGrants[0]?.grantId).toBe("grant-other");
    expect(db.devices.find((device) => device.deviceId === "device-2")?.tokenHash).toBe("revoked:grant-target");
    expect(db.devices.find((device) => device.deviceId === "device-3")?.tokenHash).toBe("active-token-b");
  });

  it("returns 409 access_key_name_taken over HTTP when provisioning a duplicate live key name, creating no rows", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("root");
    await db.createAgentGrantForRoute({
      folderId: "folder-1",
      scopeNodeId: "root",
      deviceId: "device-1",
      grantId: "grant-1",
      tokenHash: "hash-1",
      name: "build-01",
      canRead: true,
      canWrite: true,
      createdByUserId: "user-1",
    });
    const app = metadataAppFor(db, { type: "user", userId: "user-1" });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ name: "build-01" }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(409);
    await expect(response.json()).resolves.toEqual({ error: "access_key_name_taken" });
    expect(db.devices).toHaveLength(1);
    expect(db.folderGrants).toHaveLength(1);
  });

  describe("POST /folders/:id/grants/:grantId/regenerate", () => {
    it("deletes the old grant and revokes its token before inserting the replacement", async () => {
      const db = new LifecycleDb();
      db.authorizedScopes.add("work");
      await db.createAgentGrantForRoute({
        folderId: "folder-1",
        scopeNodeId: "work",
        deviceId: "old-device",
        grantId: "old-grant",
        tokenHash: "old-hash",
        name: "build-01",
        canRead: true,
        canWrite: true,
        createdByUserId: "user-1",
      });
      const app = metadataAppFor(db, { type: "user", userId: "user-2" });

      const response = await app.request("/folders/folder-1/grants/old-grant/regenerate", { method: "POST" });
      const body = await response.json();

      expect(response.status).toBe(200);
      expect(body).toMatchObject({ grant_id: expect.any(String), device_id: expect.any(String), token: expect.any(String) });
      expect(body.grant_id).not.toBe("old-grant");
      expect(db.folderGrants.find((item) => item.grantId === "old-grant")).toBeUndefined();
      expect(db.devices.find((item) => item.deviceId === "old-device")?.tokenHash).toBe("revoked:old-grant");
      const replacement = db.folderGrants.find((item) => item.grantId === body.grant_id);
      expect(replacement).toMatchObject({ name: "build-01", scopeNodeId: "work", canRead: true, canWrite: true, createdByUserId: "user-2" });
    });

    it("leaves the old grant and its token_hash untouched when the transaction fails before commit", async () => {
      const db = new LifecycleDb();
      db.authorizedScopes.add("work");
      await db.createAgentGrantForRoute({
        folderId: "folder-1",
        scopeNodeId: "work",
        deviceId: "old-device",
        grantId: "old-grant",
        tokenHash: "old-hash",
        name: "build-01",
        canRead: true,
        canWrite: true,
        createdByUserId: "user-1",
      });
      const app = metadataAppFor(db, { type: "user", userId: "user-2" }, {
        onGrantDeviceCreated: vi.fn(async () => {
          throw new Error("tenant linkage failed");
        }),
      });

      const response = await app.request("/folders/folder-1/grants/old-grant/regenerate", { method: "POST" });

      expect(response.status).toBe(500);
      expect(db.folderGrants).toHaveLength(1);
      expect(db.folderGrants[0]).toMatchObject({ grantId: "old-grant", deviceId: "old-device" });
      expect(db.devices.find((item) => item.deviceId === "old-device")?.tokenHash).toBe("old-hash");
    });

    it("attributes the replacement to the actor performing the regeneration, not the original creator", async () => {
      const db = new LifecycleDb();
      db.authorizedScopes.add("work");
      await db.createAgentGrantForRoute({
        folderId: "folder-1",
        scopeNodeId: "work",
        deviceId: "old-device",
        grantId: "old-grant",
        tokenHash: "old-hash",
        name: "build-01",
        canRead: true,
        canWrite: true,
        createdByUserId: "owner-user",
      });
      const app = metadataAppFor(db, { type: "user", userId: "collaborator-user" });

      const response = await app.request("/folders/folder-1/grants/old-grant/regenerate", { method: "POST" });
      const body = await response.json();

      expect(response.status).toBe(200);
      const replacement = db.folderGrants.find((item) => item.grantId === body.grant_id);
      expect(replacement?.createdByUserId).toBe("collaborator-user");
      expect(replacement?.createdByUserId).not.toBe("owner-user");
    });

    it("succeeds even when checkPlan denies the folder - rotation is never gated on billing", async () => {
      const db = new LifecycleDb();
      db.authorizedScopes.add("work");
      await db.createAgentGrantForRoute({
        folderId: "folder-1",
        scopeNodeId: "work",
        deviceId: "old-device",
        grantId: "old-grant",
        tokenHash: "old-hash",
        name: "build-01",
        canRead: true,
        canWrite: true,
        createdByUserId: "user-1",
      });
      const checkPlanForGrant = vi.fn(async () => ({ allowed: false, status: "canceled" }));
      const app = metadataAppFor(db, { type: "user", userId: "user-1" }, { checkPlanForGrant });

      const response = await app.request("/folders/folder-1/grants/old-grant/regenerate", { method: "POST" });

      expect(response.status).toBe(200);
      expect(checkPlanForGrant).not.toHaveBeenCalled();
    });

    it("returns 400 grant_has_no_token when the grant is a user grant", async () => {
      const db = new LifecycleDb();
      db.folderGrants.push(grant("grant-owner", { scopeNodeId: "work", userId: "user-1", canWrite: true }));
      db.authorizedScopes.add("work");
      const app = metadataAppFor(db, { type: "user", userId: "user-1" });

      const response = await app.request("/folders/folder-1/grants/grant-owner/regenerate", { method: "POST" });

      expect(response.status).toBe(400);
      await expect(response.json()).resolves.toEqual({ error: "grant_has_no_token" });
      expect(db.folderGrants.find((item) => item.grantId === "grant-owner")).toBeDefined();
    });

    it("returns 403 access_key_cannot_issue_keys when a write-capable access key calls regenerate", async () => {
      const db = new LifecycleDb();
      db.devices.push({ deviceId: "agent-1", userId: null, name: "Agent", tokenHash: "agent-token-hash" });
      db.folderGrants.push(grant("grant-agent", { deviceId: "agent-1", scopeNodeId: "work", canWrite: true }));
      const app = metadataAppFor(db, { type: "device", deviceId: "agent-1" });

      const response = await app.request("/folders/folder-1/grants/grant-agent/regenerate", {
        method: "POST",
        headers: { authorization: "Bearer device-token" },
      });

      expect(response.status).toBe(403);
      await expect(response.json()).resolves.toEqual({ error: "access_key_cannot_issue_keys" });
      expect(db.folderGrants.find((item) => item.grantId === "grant-agent")).toBeDefined();
      expect(db.devices.find((item) => item.deviceId === "agent-1")?.tokenHash).toBe("agent-token-hash");
    });

    it("returns 404 grant_not_found for an unknown grant id", async () => {
      const response = await metadataAppFor(new LifecycleDb(), { type: "user", userId: "user-1" }).request(
        "/folders/folder-1/grants/missing-grant/regenerate",
        { method: "POST" },
      );

      expect(response.status).toBe(404);
      await expect(response.json()).resolves.toEqual({ error: "grant_not_found" });
    });
  });
});
