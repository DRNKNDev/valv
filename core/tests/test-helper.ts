import type { Hono } from "hono";

import type { CoreAuth, CoreDb, Principal } from "../src/auth/index.js";
import { pgSchema } from "../src/db/schema.js";
import type { SendInviteEmail } from "../src/email/index.js";
import { createMetadataRouter, type MetadataHub } from "../src/metadata/index.js";

export type FolderGrant = {
  grantId: string;
  folderId: string;
  scopeNodeId: string;
  userId: string | null;
  deviceId: string | null;
  role: "owner" | "collaborator";
  canRead: boolean;
  canWrite: boolean;
};

export type FolderInvite = {
  inviteToken: string;
  folderId: string;
  scopeNodeId: string;
  invitedEmail: string;
  invitedByUserId: string;
  status: "pending" | "accepted" | "revoked" | "expired";
  expiresAt: Date;
};

export class LifecycleDb implements CoreDb {
  insert: CoreDb["insert"];
  update: CoreDb["update"];
  delete: CoreDb["delete"];
  execute: CoreDb["execute"];
  sharedFolders: Array<{ folderId: string; name: string; ownerUserId: string }> = [];
  nodes: Array<{ nodeId: string; folderId: string; parentId: string | null; name: string; type: string }> = [
    { nodeId: "root", folderId: "folder-1", parentId: null, name: "", type: "folder" },
    { nodeId: "work", folderId: "folder-1", parentId: "root", name: "work", type: "folder" },
  ];
  folderGrants: FolderGrant[] = [];
  folderInvites: FolderInvite[] = [];
  devices: Array<{ deviceId: string; userId: string | null; name: string; tokenHash: string }> = [];
  authorizedScopes = new Set<string>();
  private devicePrincipalId?: string;

  setDevicePrincipal(deviceId: string): void {
    this.devicePrincipalId = deviceId;
  }

  select(): any {
    return {
      from: () => ({
        where: () => ({
          limit: async () => (this.devicePrincipalId ? [{ deviceId: this.devicePrincipalId }] : []),
        }),
      }),
    };
  }

  async getFolderRootForAuthz(folderId: string): Promise<string | undefined> {
    return this.nodes.find((node) => node.folderId === folderId && node.parentId === null)?.nodeId;
  }

  async getNodeForAuthz(nodeId: string): Promise<{ nodeId: string; folderId: string; parentId: string | null } | undefined> {
    const node = this.nodes.find((item) => item.nodeId === nodeId);
    return node ? { nodeId: node.nodeId, folderId: node.folderId, parentId: node.parentId } : undefined;
  }

  async getGrantForAuthz(opts: {
    folderId: string;
    scopeNodeId: string;
    principal: Principal;
  }): Promise<{ grantId: string; scopeNodeId: string; canRead: boolean; canWrite: boolean } | undefined> {
    if (this.authorizedScopes.has(opts.scopeNodeId)) {
      return { grantId: "grant-authz", scopeNodeId: opts.scopeNodeId, canRead: true, canWrite: true };
    }
    return undefined;
  }

  async createFolderForRoute(opts: {
    folderId: string;
    rootNodeId: string;
    grantId: string;
    name: string;
    ownerUserId: string;
  }): Promise<void> {
    this.sharedFolders.push({ folderId: opts.folderId, name: opts.name, ownerUserId: opts.ownerUserId });
    this.nodes.push({ nodeId: opts.rootNodeId, folderId: opts.folderId, parentId: null, name: "", type: "folder" });
    this.folderGrants.push(
      grant(opts.grantId, { userId: opts.ownerUserId, scopeNodeId: opts.rootNodeId, folderId: opts.folderId, role: "owner" }),
    );
  }

  async listGrantsForRoute(principal: Principal): Promise<unknown[]> {
    return this.folderGrants
      .filter((item) =>
        principal.type === "user" ? item.userId === principal.userId : item.deviceId === principal.deviceId,
      )
      .map((item) => ({
        grant_id: item.grantId,
        folder_id: item.folderId,
        scope_node_id: item.scopeNodeId,
        role: item.role,
        can_read: item.canRead,
        can_write: item.canWrite,
      }));
  }

  async createInviteForRoute(opts: {
    inviteToken: string;
    folderId: string;
    scopeNodeId: string;
    invitedEmail: string;
    invitedByUserId: string;
    expiresAt: Date;
  }): Promise<{ folderName: string }> {
    this.folderInvites.push({ ...opts, status: "pending" });
    return { folderName: "Projects" };
  }

  async getInviteForRoute(inviteToken: string): Promise<FolderInvite | undefined> {
    return this.folderInvites.find((item) => item.inviteToken === inviteToken);
  }

  async acceptInviteForRoute(opts: { inviteToken: string; userId: string; folderId: string; scopeNodeId: string }): Promise<void> {
    const invite = this.folderInvites.find((item) => item.inviteToken === opts.inviteToken);
    if (!invite || invite.status === "accepted") {
      return;
    }
    this.folderGrants.push(grant(`accepted-${opts.inviteToken}`, { userId: opts.userId, scopeNodeId: opts.scopeNodeId }));
    invite.status = "accepted";
  }

  async createAgentGrantForRoute(opts: {
    folderId: string;
    scopeNodeId: string;
    deviceId: string;
    grantId: string;
    tokenHash: string;
    name: string;
    canRead: boolean;
    canWrite: boolean;
  }): Promise<void> {
    this.devices.push({ deviceId: opts.deviceId, userId: null, name: opts.name, tokenHash: opts.tokenHash });
    this.folderGrants.push(
      grant(opts.grantId, {
        folderId: opts.folderId,
        scopeNodeId: opts.scopeNodeId,
        deviceId: opts.deviceId,
        canRead: opts.canRead,
        canWrite: opts.canWrite,
      }),
    );
  }

  async getGrantScopeForRoute(grantId: string): Promise<string | undefined> {
    return this.folderGrants.find((item) => item.grantId === grantId)?.scopeNodeId;
  }

  async deleteGrantForRoute(grantId: string): Promise<void> {
    this.folderGrants = this.folderGrants.filter((item) => item.grantId !== grantId);
  }
}

export function metadataAppFor(
  db: CoreDb,
  principal: Principal,
  opts: { hub?: MetadataHub; sendInviteEmail?: SendInviteEmail } = {},
): Hono {
  if (principal.type === "device" && "setDevicePrincipal" in db && typeof db.setDevicePrincipal === "function") {
    db.setDevicePrincipal(principal.deviceId);
  }
  const auth = {
    db,
    schema: pgSchema,
    api: {
      getSession: async () => (principal.type === "user" ? { user: { id: principal.userId } } : null),
    },
  } as unknown as CoreAuth;
  return createMetadataRouter({
    auth,
    hub: opts.hub ?? { notify: () => undefined },
    sendInviteEmail: opts.sendInviteEmail,
  }) as Hono;
}

export function grant(
  grantId: string,
  opts: {
    folderId?: string;
    scopeNodeId: string;
    userId?: string;
    deviceId?: string;
    role?: "owner" | "collaborator";
    canRead?: boolean;
    canWrite?: boolean;
  },
): FolderGrant {
  return {
    grantId,
    folderId: opts.folderId ?? "folder-1",
    scopeNodeId: opts.scopeNodeId,
    userId: opts.userId ?? null,
    deviceId: opts.deviceId ?? null,
    role: opts.role ?? "collaborator",
    canRead: opts.canRead ?? true,
    canWrite: opts.canWrite ?? true,
  };
}
