import { beforeEach, describe, expect, it } from "vitest";

import type { CoreDb, Principal } from "../auth/index.js";
import { checkGrant } from "./authz.js";

type TestNode = { nodeId: string; folderId: string; parentId: string | null };
type TestGrant = {
  grantId: string;
  folderId: string;
  scopeNodeId: string;
  userId?: string;
  deviceId?: string;
  canRead: boolean;
  canWrite: boolean;
};

describe("checkGrant", () => {
  let db: TestAuthzDb;

  beforeEach(() => {
    db = new TestAuthzDb();
    seedTree(db);
  });

  it("grants whole-folder access to descendants", async () => {
    db.grantUser("grant-root", "root", true, true);

    await expect(checkGrant(db, "doc", { type: "user", userId: "user-1" }, "read"))
      .resolves.toMatchObject({ granted: true, grantId: "grant-root", scopeNodeId: "root" });
  });

  it("grants subtree access and denies siblings", async () => {
    db.grantUser("grant-work", "work", true, true);

    await expect(checkGrant(db, "doc", { type: "user", userId: "user-1" }, "read"))
      .resolves.toMatchObject({ granted: true, scopeNodeId: "work" });
    await expect(checkGrant(db, "personal-doc", { type: "user", userId: "user-1" }, "read"))
      .resolves.toEqual({ granted: false, reason: "no_grant" });
  });

  it("returns no_grant when no ancestor grant exists", async () => {
    await expect(checkGrant(db, "doc", { type: "user", userId: "user-1" }, "read"))
      .resolves.toEqual({ granted: false, reason: "no_grant" });
  });

  it("matches device-scoped grants only for the same device", async () => {
    db.grantDevice("grant-device", "work", "device-1", true, true);

    await expect(checkGrant(db, "doc", { type: "device", deviceId: "device-1" }, "read"))
      .resolves.toMatchObject({ granted: true, grantId: "grant-device" });
    await expect(checkGrant(db, "doc", { type: "device", deviceId: "device-2" }, "read"))
      .resolves.toEqual({ granted: false, reason: "no_grant" });
  });

  it("lets human-registered devices inherit user grants", async () => {
    db.devices.set("device-1", "user-1");
    db.grantUser("grant-user", "work", true, true);

    await expect(checkGrant(db, "doc", { type: "device", deviceId: "device-1" }, "read"))
      .resolves.toMatchObject({ granted: true, grantId: "grant-user", scopeNodeId: "work" });
  });

  it("does not let agent devices inherit user grants", async () => {
    db.grantUser("grant-user", "work", true, true);

    await expect(checkGrant(db, "doc", { type: "device", deviceId: "agent-device" }, "read"))
      .resolves.toEqual({ granted: false, reason: "no_grant" });
  });

  it("returns insufficient_permission for read-only grant when write is required", async () => {
    db.grantUser("grant-read", "work", true, false);

    await expect(checkGrant(db, "doc", { type: "user", userId: "user-1" }, "write"))
      .resolves.toEqual({ granted: false, reason: "insufficient_permission" });
  });

  it("uses the deeper grant before a shallower grant", async () => {
    db.grantUser("grant-root", "root", true, false);
    db.grantUser("grant-work", "work", true, true);

    await expect(checkGrant(db, "doc", { type: "user", userId: "user-1" }, "write"))
      .resolves.toMatchObject({ granted: true, grantId: "grant-work", canWrite: true });
  });
});

class TestAuthzDb implements CoreDb {
  select: CoreDb["select"];
  insert: CoreDb["insert"];
  update: CoreDb["update"];
  delete: CoreDb["delete"];
  nodes = new Map<string, TestNode>();
  grants: TestGrant[] = [];
  devices = new Map<string, string>();

  async getNodeForAuthz(nodeId: string): Promise<TestNode | undefined> {
    return this.nodes.get(nodeId);
  }

  async getGrantForAuthz(opts: {
    folderId: string;
    scopeNodeId: string;
    principal: Principal;
  }): Promise<TestGrant | undefined> {
    return this.grants.find((grant) => {
      const deviceUserId = opts.principal.type === "device" ? this.devices.get(opts.principal.deviceId) : undefined;
      const principalMatches =
        opts.principal.type === "user"
          ? grant.userId === opts.principal.userId
          : grant.deviceId === opts.principal.deviceId || (deviceUserId !== undefined && grant.userId === deviceUserId);
      return grant.folderId === opts.folderId && grant.scopeNodeId === opts.scopeNodeId && principalMatches;
    });
  }

  async getDeviceUserIdForAuthz(deviceId: string): Promise<string | undefined> {
    return this.devices.get(deviceId);
  }

  grantUser(grantId: string, scopeNodeId: string, canRead: boolean, canWrite: boolean): void {
    this.grants.push({
      grantId,
      folderId: "folder-1",
      scopeNodeId,
      userId: "user-1",
      canRead,
      canWrite,
    });
  }

  grantDevice(
    grantId: string,
    scopeNodeId: string,
    deviceId: string,
    canRead: boolean,
    canWrite: boolean,
  ): void {
    this.grants.push({ grantId, folderId: "folder-1", scopeNodeId, deviceId, canRead, canWrite });
  }
}

function seedTree(db: TestAuthzDb): void {
  db.nodes.set("root", { nodeId: "root", folderId: "folder-1", parentId: null });
  db.nodes.set("work", { nodeId: "work", folderId: "folder-1", parentId: "root" });
  db.nodes.set("doc", { nodeId: "doc", folderId: "folder-1", parentId: "work" });
  db.nodes.set("personal", { nodeId: "personal", folderId: "folder-1", parentId: "root" });
  db.nodes.set("personal-doc", { nodeId: "personal-doc", folderId: "folder-1", parentId: "personal" });
}
