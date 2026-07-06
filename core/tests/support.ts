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
  canWrite: boolean;
  status: "pending" | "accepted" | "revoked" | "expired";
  expiresAt: Date;
};

export class LifecycleDb implements CoreDb {
  insert: CoreDb["insert"];
  update: CoreDb["update"] = () => ({
    set: (values: Partial<{ tokenHash: string }>) => ({
      where: async () => {
        if (values.tokenHash !== undefined) {
          for (const device of this.devices) {
            device.tokenHash = values.tokenHash;
          }
        }
      },
    }),
  });
  delete: CoreDb["delete"] = () => ({
    where: async () => {
      this.folderGrants = [];
    },
  });
  execute: CoreDb["execute"];
  sharedFolders: Array<{ folderId: string; name: string; ownerUserId: string }> = [];
  nodes: Array<{ nodeId: string; folderId: string; parentId: string | null; name: string; type: string }> = [
    { nodeId: "root", folderId: "folder-1", parentId: null, name: "", type: "folder" },
    { nodeId: "work", folderId: "folder-1", parentId: "root", name: "work", type: "folder" },
  ];
  folderGrants: FolderGrant[] = [];
  folderInvites: FolderInvite[] = [];
  devices: Array<{ deviceId: string; userId: string | null; name: string; tokenHash: string }> = [];
  users: Array<{ id: string; email: string }> = [];
  authorizedScopes = new Set<string>();
  private authorizedScopeCapabilities = new Map<string, { canRead: boolean; canWrite: boolean }>();
  private devicePrincipalId?: string;

  authorizeScope(scopeNodeId: string, opts: { canRead?: boolean; canWrite?: boolean } = {}): void {
    this.authorizedScopes.add(scopeNodeId);
    this.authorizedScopeCapabilities.set(scopeNodeId, {
      canRead: opts.canRead ?? true,
      canWrite: opts.canWrite ?? true,
    });
  }

  setDevicePrincipal(deviceId: string): void {
    this.devicePrincipalId = deviceId;
  }

  select(selection?: Record<string, unknown>): any {
    return {
      from: () => ({
        where: () => ({
          limit: async () => this.selectRows(selection),
        }),
      }),
    };
  }

  private selectRows(selection?: Record<string, unknown>): unknown[] {
    const keys = Object.keys(selection ?? {});
    if (keys.includes("scopeNodeId")) {
      return this.folderGrants.map((item) => ({ scopeNodeId: item.scopeNodeId, deviceId: item.deviceId }));
    }
    if (keys.includes("userId")) {
      return this.devices.map((item) => ({ userId: item.userId }));
    }
    if (keys.includes("name")) {
      return this.sharedFolders.map((item) => ({ name: item.name }));
    }
    return this.devicePrincipalId ? [{ deviceId: this.devicePrincipalId }] : [];
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
      const capability = this.authorizedScopeCapabilities.get(opts.scopeNodeId) ?? { canRead: true, canWrite: true };
      return { grantId: "grant-authz", scopeNodeId: opts.scopeNodeId, ...capability };
    }
    const deviceUserId = opts.principal.type === "device" ? await this.getDeviceUserIdForAuthz(opts.principal.deviceId) : undefined;
    return this.folderGrants.find((grant) => {
      const principalMatches =
        opts.principal.type === "user"
          ? grant.userId === opts.principal.userId
          : grant.deviceId === opts.principal.deviceId || (deviceUserId !== undefined && grant.userId === deviceUserId);
      return grant.folderId === opts.folderId && grant.scopeNodeId === opts.scopeNodeId && principalMatches;
    });
  }

  async getDeviceUserIdForAuthz(deviceId: string): Promise<string | undefined> {
    return this.devices.find((device) => device.deviceId === deviceId)?.userId ?? undefined;
  }

  async getDeviceUserIdForRoute(deviceId: string): Promise<string | undefined> {
    return this.getDeviceUserIdForAuthz(deviceId);
  }

  async getFolderForRoute(folderId: string): Promise<{ name: string } | undefined> {
    const folder = this.sharedFolders.find((item) => item.folderId === folderId);
    return folder ? { name: folder.name } : undefined;
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
    const deviceUserId = principal.type === "device" ? await this.getDeviceUserIdForRoute(principal.deviceId) : undefined;
    return this.folderGrants
      .filter((item) =>
        principal.type === "user"
          ? item.userId === principal.userId
          : item.deviceId === principal.deviceId || (deviceUserId !== undefined && item.userId === deviceUserId),
      )
      .map((item) => ({
        grant_id: item.grantId,
        folder_id: item.folderId,
        scope_node_id: item.scopeNodeId,
        role: item.role,
        can_read: item.canRead,
        can_write: item.canWrite,
        user_id: item.userId,
        device_id: item.deviceId,
        grantee_email: item.userId ? this.users.find((user) => user.id === item.userId)?.email ?? null : null,
        device_name: item.deviceId ? this.devices.find((device) => device.deviceId === item.deviceId)?.name ?? null : null,
      }));
  }

  async createInviteForRoute(opts: {
    inviteToken: string;
    folderId: string;
    scopeNodeId: string;
    invitedEmail: string;
    invitedByUserId: string;
    canWrite: boolean;
    expiresAt: Date;
  }): Promise<{ folderName: string }> {
    this.folderInvites.push({ ...opts, status: "pending" });
    return { folderName: "Projects" };
  }

  async getInviteForRoute(inviteToken: string): Promise<FolderInvite | undefined> {
    return this.folderInvites.find((item) => item.inviteToken === inviteToken);
  }

  async acceptInviteForRoute(opts: {
    inviteToken: string;
    userId: string;
    folderId: string;
    scopeNodeId: string;
    canWrite: boolean;
  }): Promise<void> {
    const invite = this.folderInvites.find((item) => item.inviteToken === opts.inviteToken);
    if (!invite || invite.status === "accepted") {
      return;
    }
    this.folderGrants.push(
      grant(`accepted-${opts.inviteToken}`, { userId: opts.userId, scopeNodeId: opts.scopeNodeId, canWrite: opts.canWrite }),
    );
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

  async getGrantForRoute(grantId: string): Promise<{ scopeNodeId: string; deviceId: string | null } | undefined> {
    const grantRow = this.folderGrants.find((item) => item.grantId === grantId);
    return grantRow ? { scopeNodeId: grantRow.scopeNodeId, deviceId: grantRow.deviceId } : undefined;
  }

  async deleteGrantForRoute(grantId: string): Promise<void> {
    this.folderGrants = this.folderGrants.filter((item) => item.grantId !== grantId);
  }
}

export function metadataAppFor(
  db: CoreDb,
  principal: Principal,
  opts: {
    hub?: MetadataHub;
    sendInviteEmail?: SendInviteEmail;
    onFolderCreated?: (info: { folderId: string; ownerUserId: string; grantId: string }) => Promise<void>;
  } = {},
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
    onFolderCreated: opts.onFolderCreated,
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
