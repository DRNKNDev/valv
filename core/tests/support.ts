import type { Hono } from "hono";

import type { CoreAuth, CoreDb, Principal } from "../src/auth/index.js";
import { pgSchema } from "../src/db/schema.js";
import type { SendInviteEmail } from "../src/email/index.js";
import { createMetadataRouter, type MetadataHub, type OnGrantCreated } from "../src/metadata/index.js";

export type FolderGrant = {
  grantId: string;
  folderId: string;
  scopeNodeId: string;
  userId: string | null;
  deviceId: string | null;
  name: string | null;
  role: "owner" | "collaborator";
  canRead: boolean;
  canWrite: boolean;
  createdByUserId: string | null;
};

export type FolderInvite = {
  inviteId: string;
  inviteToken: string;
  folderId: string;
  scopeNodeId: string;
  invitedEmail: string;
  invitedByUserId: string;
  canWrite: boolean;
  status: "pending" | "accepted" | "revoked" | "expired";
  expiresAt: Date;
  createdAt?: Date;
};

export class UniqueConstraintError extends Error {
  code = "23505";
  constraint: string;

  constructor(constraint: string) {
    super(`duplicate key value violates unique constraint "${constraint}"`);
    this.name = "UniqueConstraintError";
    this.constraint = constraint;
  }
}

function extractEqValue(condition: unknown): string | undefined {
  if (!condition || typeof condition !== "object" || !("queryChunks" in condition)) {
    return undefined;
  }
  const chunks = (condition as { queryChunks: unknown[] }).queryChunks;
  for (const chunk of chunks) {
    if (chunk && typeof chunk === "object" && "value" in chunk) {
      const value = (chunk as { value: unknown }).value;
      if (typeof value === "string") {
        return value;
      }
    }
  }
  return undefined;
}

export class LifecycleDb implements CoreDb {
  insert: CoreDb["insert"] = (table: unknown) => ({
    values: async (value: Record<string, unknown>) => {
      if (table === pgSchema.devices) {
        this.devices.push({
          deviceId: String(value.deviceId),
          userId: value.userId === null ? null : String(value.userId),
          name: String(value.name),
          tokenHash: String(value.tokenHash),
        });
        return;
      }
      if (table === pgSchema.sharedFolders) {
        this.sharedFolders.push({
          folderId: String(value.folderId),
          name: String(value.name),
          ownerUserId: String(value.ownerUserId),
        });
        return;
      }
      if (table === pgSchema.nodes) {
        this.nodes.push({
          nodeId: String(value.nodeId),
          folderId: String(value.folderId),
          parentId: value.parentId === null ? null : String(value.parentId),
          name: String(value.name),
          type: String(value.type),
        });
        return;
      }
      if (table === pgSchema.folderGrants) {
        this.folderGrants.push(
          grant(String(value.grantId), {
            folderId: String(value.folderId),
            scopeNodeId: String(value.scopeNodeId),
            userId: value.userId === null || value.userId === undefined ? undefined : String(value.userId),
            deviceId: value.deviceId === null || value.deviceId === undefined ? undefined : String(value.deviceId),
            name: value.name === null || value.name === undefined ? undefined : String(value.name),
            role: value.role as "owner" | "collaborator" | undefined,
            canRead: Boolean(value.canRead),
            canWrite: Boolean(value.canWrite),
            createdByUserId:
              value.createdByUserId === null || value.createdByUserId === undefined ? undefined : String(value.createdByUserId),
          }),
        );
      }
    },
  });
  update: CoreDb["update"] = (table: unknown) => ({
    set: (values: Partial<{ tokenHash: string; status: string }>) => ({
      where: async (condition?: unknown) => {
        if (table === pgSchema.devices) {
          const deviceId = extractEqValue(condition);
          for (const device of this.devices) {
            if (deviceId !== undefined && device.deviceId !== deviceId) {
              continue;
            }
            if (values.tokenHash !== undefined) {
              device.tokenHash = values.tokenHash;
            }
          }
          return;
        }
        if (table === pgSchema.folderInvites) {
          const inviteToken = extractEqValue(condition);
          for (const invite of this.folderInvites) {
            if (inviteToken !== undefined && invite.inviteToken !== inviteToken) {
              continue;
            }
            if (values.status !== undefined) {
              invite.status = values.status as FolderInvite["status"];
            }
          }
        }
      },
    }),
  });
  delete: CoreDb["delete"] = (table: unknown) => ({
    where: async (condition?: unknown) => {
      if (table === pgSchema.folderGrants) {
        const grantId = extractEqValue(condition);
        if (grantId === undefined) {
          return;
        }
        this.folderGrants = this.folderGrants.filter((item) => item.grantId !== grantId);
      }
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
      grant(opts.grantId, {
        userId: opts.ownerUserId,
        scopeNodeId: opts.rootNodeId,
        folderId: opts.folderId,
        role: "owner",
        createdByUserId: opts.ownerUserId,
      }),
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
        name: item.name,
        grantee_email: item.userId ? this.users.find((user) => user.id === item.userId)?.email ?? null : null,
        device_name: item.deviceId ? this.devices.find((device) => device.deviceId === item.deviceId)?.name ?? null : null,
      }));
  }

  async listFolderGrantsForRoute(folderId: string): Promise<unknown[]> {
    return this.folderGrants
      .filter((item) => item.folderId === folderId)
      .map((item) => ({
        grant_id: item.grantId,
        folder_id: item.folderId,
        scope_node_id: item.scopeNodeId,
        role: item.role,
        can_read: item.canRead,
        can_write: item.canWrite,
        user_id: item.userId,
        device_id: item.deviceId,
        name: item.name,
        grantee_email: item.userId ? this.users.find((user) => user.id === item.userId)?.email ?? null : null,
        device_name: item.deviceId ? this.devices.find((device) => device.deviceId === item.deviceId)?.name ?? null : null,
        created_by_user_id: item.createdByUserId,
        created_by_email: item.createdByUserId
          ? this.users.find((user) => user.id === item.createdByUserId)?.email ?? null
          : null,
      }));
  }

  async createInviteForRoute(opts: {
    inviteId: string;
    inviteToken: string;
    folderId: string;
    scopeNodeId: string;
    invitedEmail: string;
    invitedByUserId: string;
    canWrite: boolean;
    expiresAt: Date;
  }): Promise<{ folderName: string }> {
    this.folderInvites.push({ ...opts, status: "pending", createdAt: new Date() });
    return { folderName: "Projects" };
  }

  async getInviteForRoute(inviteToken: string): Promise<FolderInvite | undefined> {
    return this.folderInvites.find((item) => item.inviteToken === inviteToken);
  }

  async getInviteByIdForRoute(inviteId: string): Promise<FolderInvite | undefined> {
    return this.folderInvites.find((item) => item.inviteId === inviteId);
  }

  async listFolderInvitesForRoute(folderId: string): Promise<unknown[]> {
    const now = Date.now();
    return this.folderInvites
      .filter((item) => item.folderId === folderId && item.status === "pending" && item.expiresAt.getTime() > now)
      .map((item) => ({
        invite_id: item.inviteId,
        invited_email: item.invitedEmail,
        scope_node_id: item.scopeNodeId,
        can_write: item.canWrite,
        created_at: item.createdAt ?? new Date(),
        expires_at: item.expiresAt,
        created_by_user_id: item.invitedByUserId,
        created_by_email: this.users.find((user) => user.id === item.invitedByUserId)?.email ?? null,
      }));
  }

  async deleteInviteForRoute(inviteId: string): Promise<void> {
    this.folderInvites = this.folderInvites.filter((item) => item.inviteId !== inviteId);
  }

  async acceptInviteForRoute(opts: {
    inviteToken: string;
    userId: string;
    folderId: string;
    scopeNodeId: string;
    canWrite: boolean;
    invitedByUserId: string;
  }): Promise<void> {
    const invite = this.folderInvites.find((item) => item.inviteToken === opts.inviteToken);
    if (!invite || invite.status === "accepted") {
      return;
    }
    this.folderGrants.push(
      grant(`accepted-${opts.inviteToken}`, {
        userId: opts.userId,
        scopeNodeId: opts.scopeNodeId,
        canWrite: opts.canWrite,
        createdByUserId: opts.invitedByUserId,
      }),
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
    createdByUserId: string | undefined;
  }): Promise<void> {
    const nameTaken = this.folderGrants.some(
      (item) => item.folderId === opts.folderId && item.deviceId !== null && item.name === opts.name,
    );
    if (nameTaken) {
      throw new UniqueConstraintError("folder_grants_folder_name_unique");
    }
    this.devices.push({ deviceId: opts.deviceId, userId: null, name: opts.name, tokenHash: opts.tokenHash });
    this.folderGrants.push(
      grant(opts.grantId, {
        folderId: opts.folderId,
        scopeNodeId: opts.scopeNodeId,
        deviceId: opts.deviceId,
        name: opts.name,
        canRead: opts.canRead,
        canWrite: opts.canWrite,
        createdByUserId: opts.createdByUserId,
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

  async regenerateGrantForRoute(opts: {
    oldGrantId: string;
    newDeviceId: string;
    newGrantId: string;
    tokenHash: string;
    createdByUserId: string | undefined;
    onBeforeCommit?: (info: { folderId: string; scopeNodeId: string; deviceId: string; grantId: string }) => Promise<void>;
  }): Promise<{ folderId: string; scopeNodeId: string; name: string | null; canRead: boolean; canWrite: boolean } | undefined> {
    const old = this.folderGrants.find((item) => item.grantId === opts.oldGrantId);
    if (!old || old.deviceId === null) {
      return undefined;
    }
    const oldDevice = this.devices.find((item) => item.deviceId === old.deviceId);
    const priorTokenHash = oldDevice?.tokenHash;
    const priorGrant = { ...old };

    this.folderGrants = this.folderGrants.filter((item) => item.grantId !== opts.oldGrantId);
    if (oldDevice) {
      oldDevice.tokenHash = `revoked:${opts.oldGrantId}`;
    }
    this.devices.push({ deviceId: opts.newDeviceId, userId: null, name: old.name ?? "Agent", tokenHash: opts.tokenHash });
    this.folderGrants.push(
      grant(opts.newGrantId, {
        folderId: old.folderId,
        scopeNodeId: old.scopeNodeId,
        deviceId: opts.newDeviceId,
        name: old.name ?? undefined,
        canRead: old.canRead,
        canWrite: old.canWrite,
        createdByUserId: opts.createdByUserId,
      }),
    );

    if (opts.onBeforeCommit) {
      try {
        await opts.onBeforeCommit({ folderId: old.folderId, scopeNodeId: old.scopeNodeId, deviceId: opts.newDeviceId, grantId: opts.newGrantId });
      } catch (error) {
        this.folderGrants = this.folderGrants.filter((item) => item.grantId !== opts.newGrantId);
        this.devices = this.devices.filter((item) => item.deviceId !== opts.newDeviceId);
        if (oldDevice && priorTokenHash !== undefined) {
          oldDevice.tokenHash = priorTokenHash;
        }
        this.folderGrants.push(priorGrant);
        throw error;
      }
    }
    return { folderId: old.folderId, scopeNodeId: old.scopeNodeId, name: old.name, canRead: old.canRead, canWrite: old.canWrite };
  }

  async resolvePrincipalStatusForRoute(principal: Principal): Promise<{
    type: "account" | "access_key";
    email?: string;
    scopes: Array<{ folder_id: string; folder_name: string; scope_label: string; can_write: boolean }>;
  }> {
    const effectiveUserId = principal.type === "user" ? principal.userId : this.devices.find((d) => d.deviceId === principal.deviceId)?.userId;
    if (effectiveUserId) {
      return { type: "account", email: this.users.find((user) => user.id === effectiveUserId)?.email, scopes: [] };
    }
    const deviceId = (principal as { type: "device"; deviceId: string }).deviceId;
    const scopes = this.folderGrants
      .filter((item) => item.deviceId === deviceId)
      .map((item) => {
        const folder = this.sharedFolders.find((f) => f.folderId === item.folderId);
        const rootNodeId = this.nodes.find((node) => node.folderId === item.folderId && node.parentId === null)?.nodeId;
        const node = this.nodes.find((n) => n.nodeId === item.scopeNodeId);
        const scopeLabel = item.scopeNodeId === rootNodeId ? (folder?.name ?? "") : (node?.name ?? folder?.name ?? "");
        return { folder_id: item.folderId, folder_name: folder?.name ?? "", scope_label: scopeLabel, can_write: item.canWrite };
      });
    return { type: "access_key", scopes };
  }
}

export function metadataAppFor(
  db: CoreDb,
  principal: Principal,
  opts: {
    hub?: MetadataHub;
    sendInviteEmail?: SendInviteEmail;
    onFolderCreated?: (info: { folderId: string; ownerUserId: string; grantId: string }) => Promise<void>;
    minProtocolVersion?: number;
    onGrantCreated?: OnGrantCreated;
    onGrantDeviceCreated?: (info: { folderId: string; scopeNodeId: string; deviceId: string; grantId: string }) => Promise<void>;
    checkPlanForGrant?: (folderId: string) => Promise<{ allowed: boolean; status?: string } | null>;
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
    minProtocolVersion: opts.minProtocolVersion,
    onGrantCreated: opts.onGrantCreated,
    onGrantDeviceCreated: opts.onGrantDeviceCreated,
    checkPlanForGrant: opts.checkPlanForGrant,
  }) as Hono;
}

export function grant(
  grantId: string,
  opts: {
    folderId?: string;
    scopeNodeId: string;
    userId?: string;
    deviceId?: string;
    name?: string;
    role?: "owner" | "collaborator";
    canRead?: boolean;
    canWrite?: boolean;
    createdByUserId?: string;
  },
): FolderGrant {
  return {
    grantId,
    folderId: opts.folderId ?? "folder-1",
    scopeNodeId: opts.scopeNodeId,
    userId: opts.userId ?? null,
    deviceId: opts.deviceId ?? null,
    name: opts.name ?? null,
    role: opts.role ?? "collaborator",
    canRead: opts.canRead ?? true,
    canWrite: opts.canWrite ?? true,
    createdByUserId: opts.createdByUserId ?? null,
  };
}
