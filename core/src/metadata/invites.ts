import { randomBytes } from "node:crypto";

import { Hono } from "hono";

import type { CoreAuth } from "../auth/index.js";
import type { SendInviteEmail } from "../email/index.js";
import {
  and,
  eq,
  getFolderRoot,
  gt,
  inTransaction,
  newId,
  requirePrincipal,
  requireUserBackedPrincipal,
  resolveEmailsByUserId,
  toIso,
  type MetadataVariables,
} from "./common.js";
import { checkGrant } from "./authz.js";

type InviteRecord = {
  inviteId: string;
  inviteToken: string;
  folderId: string;
  scopeNodeId: string;
  invitedByUserId: string;
  canWrite: boolean;
  status: "pending" | "accepted" | "revoked" | "expired";
  expiresAt: Date;
};

type InviteRouteStore = {
  createInviteForRoute?: (opts: {
    inviteId: string;
    inviteToken: string;
    folderId: string;
    scopeNodeId: string;
    invitedEmail: string;
    invitedByUserId: string;
    canWrite: boolean;
    expiresAt: Date;
  }) => Promise<{ folderName: string }>;
  getInviteForRoute?: (inviteToken: string) => Promise<InviteRecord | undefined>;
  getInviteByIdForRoute?: (inviteId: string) => Promise<InviteRecord | undefined>;
  listFolderInvitesForRoute?: (folderId: string) => Promise<unknown[]>;
  deleteInviteForRoute?: (inviteId: string) => Promise<void>;
  acceptInviteForRoute?: (opts: {
    inviteToken: string;
    userId: string;
    folderId: string;
    scopeNodeId: string;
    canWrite: boolean;
    invitedByUserId: string;
  }) => Promise<void>;
};

export function registerInviteRoutes(
  router: Hono<{ Variables: MetadataVariables }>,
  auth: CoreAuth,
  sendInviteEmail?: SendInviteEmail,
): void {
  router.post("/folders/:id/invites", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const folderId = ctx.req.param("id");
    const body = await ctx.req.json().catch(() => ({}));
    const invitedEmail = body.invited_email;
    if (typeof invitedEmail !== "string" || invitedEmail.length === 0) {
      return ctx.json({ error: "invalid_invited_email" }, 400);
    }

    const rootNodeId = await getFolderRoot(auth, folderId);
    const scopeNodeId = typeof body.scope_node_id === "string" ? body.scope_node_id : rootNodeId;
    if (!scopeNodeId) {
      return ctx.json({ error: "folder_not_found" }, 404);
    }

    const grant = await checkGrant(auth.db, scopeNodeId, principal, "write", auth.schema);
    if (!grant.granted) {
      return ctx.json({ error: grant.reason }, 403);
    }

    const ownerCheck = await requireUserBackedPrincipal(auth, ctx, principal, "access_key_cannot_invite_people");
    if (ownerCheck instanceof Response) {
      return ownerCheck;
    }
    const invitedByUserId = ownerCheck;

    const canWrite = typeof body.can_write === "boolean" ? body.can_write : true;

    const inviteId = newId();
    const inviteToken = randomBytes(32).toString("base64url");
    const expiresAt = new Date(Date.now() + 7 * 24 * 60 * 60 * 1000);
    let folderName = "folder";

    if (hasInviteRouteStore(auth.db) && auth.db.createInviteForRoute) {
      const created = await auth.db.createInviteForRoute({
        inviteId,
        inviteToken,
        folderId,
        scopeNodeId,
        invitedEmail,
        invitedByUserId,
        canWrite,
        expiresAt,
      });
      folderName = created.folderName;
    } else {
      await inTransaction(auth, async (tx) => {
      const folders = await tx
        .select({ name: auth.schema.sharedFolders.name })
        .from(auth.schema.sharedFolders)
        .where(eq(auth.schema.sharedFolders.folderId, folderId))
        .limit(1);
      folderName = folders[0]?.name ?? folderName;
      await tx.insert(auth.schema.folderInvites).values({
        inviteId,
        inviteToken,
        folderId,
        scopeNodeId,
        invitedEmail,
        invitedByUserId,
        canWrite,
        status: "pending",
        expiresAt,
      });
      });
    }

    if (sendInviteEmail) {
      await sendInviteEmail({ to: invitedEmail, inviteToken, folderName }).catch((error: unknown) => {
        console.error("Failed to send invite email", error);
      });
    }

    return ctx.json({ invite_token: inviteToken });
  });

  router.post("/invites/:token/accept", async (ctx) => {
    const principal = requirePrincipal(ctx);
    if (principal.type !== "user") {
      return ctx.json({ error: "user_required" }, 403);
    }

    const inviteToken = ctx.req.param("token");
    const routeInvite = hasInviteRouteStore(auth.db) && auth.db.getInviteForRoute
      ? await auth.db.getInviteForRoute(inviteToken)
      : undefined;
    const invites = routeInvite ? [routeInvite] : await auth.db
      .select()
      .from(auth.schema.folderInvites)
      .where(eq(auth.schema.folderInvites.inviteToken, inviteToken))
      .limit(1);
    const invite = invites[0];
    if (!invite) {
      return ctx.json({ error: "invite_not_found" }, 404);
    }
    if (invite.status === "accepted") {
      return ctx.json({ accepted: true });
    }
    if (invite.status !== "pending") {
      return ctx.json({ error: "invite_not_pending" }, 409);
    }
    if (new Date(invite.expiresAt).getTime() <= Date.now()) {
      return ctx.json({ error: "invite_expired" }, 410);
    }

    if (hasInviteRouteStore(auth.db) && auth.db.acceptInviteForRoute) {
      await auth.db.acceptInviteForRoute({
        inviteToken,
        userId: principal.userId,
        folderId: invite.folderId,
        scopeNodeId: invite.scopeNodeId,
        canWrite: invite.canWrite,
        invitedByUserId: invite.invitedByUserId,
      });
    } else {
      await inTransaction(auth, async (tx) => {
      await tx.insert(auth.schema.folderGrants).values({
        grantId: newId(),
        folderId: invite.folderId,
        scopeNodeId: invite.scopeNodeId,
        userId: principal.userId,
        deviceId: null,
        role: "collaborator",
        canRead: true,
        canWrite: invite.canWrite,
        createdByUserId: invite.invitedByUserId,
      });
      await tx
        .update(auth.schema.folderInvites)
        .set({ status: "accepted" })
        .where(
          and(
            eq(auth.schema.folderInvites.inviteToken, inviteToken),
            eq(auth.schema.folderInvites.status, "pending"),
          ),
        );
      });
    }

    return ctx.json({ accepted: true });
  });

  router.get("/folders/:id/invites", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const folderId = ctx.req.param("id");
    const rootNodeId = await getFolderRoot(auth, folderId);
    if (!rootNodeId) {
      return ctx.json({ error: "folder_not_found" }, 404);
    }

    const ownerCheck = await requireUserBackedPrincipal(auth, ctx, principal, "access_key_cannot_list_grants");
    if (ownerCheck instanceof Response) {
      return ownerCheck;
    }

    const grant = await checkGrant(auth.db, rootNodeId, principal, "write", auth.schema);
    if (!grant.granted) {
      return ctx.json({ error: grant.reason }, 403);
    }

    if (hasInviteRouteStore(auth.db) && auth.db.listFolderInvitesForRoute) {
      return ctx.json(await auth.db.listFolderInvitesForRoute(folderId));
    }

    const rows = await auth.db
      .select({
        invite_id: auth.schema.folderInvites.inviteId,
        invited_email: auth.schema.folderInvites.invitedEmail,
        scope_node_id: auth.schema.folderInvites.scopeNodeId,
        can_write: auth.schema.folderInvites.canWrite,
        created_at: auth.schema.folderInvites.createdAt,
        expires_at: auth.schema.folderInvites.expiresAt,
        created_by_user_id: auth.schema.folderInvites.invitedByUserId,
      })
      .from(auth.schema.folderInvites)
      .where(
        and(
          eq(auth.schema.folderInvites.folderId, folderId),
          eq(auth.schema.folderInvites.status, "pending"),
          gt(auth.schema.folderInvites.expiresAt, new Date()),
        ),
      );

    const creatorEmails = await resolveEmailsByUserId(
      auth,
      rows.map((row: { created_by_user_id: string | null }) => row.created_by_user_id),
    );
    return ctx.json(
      rows.map((row: { created_by_user_id: string | null; created_at: unknown; expires_at: unknown }) => ({
        ...row,
        created_at: toIso(row.created_at as Date),
        expires_at: toIso(row.expires_at as Date),
        created_by_email: row.created_by_user_id ? (creatorEmails.get(row.created_by_user_id) ?? null) : null,
      })),
    );
  });

  router.delete("/folders/:id/invites/:inviteId", async (ctx) => {
    const principal = requirePrincipal(ctx);
    const folderId = ctx.req.param("id");
    const inviteId = ctx.req.param("inviteId");
    const rootNodeId = await getFolderRoot(auth, folderId);
    if (!rootNodeId) {
      return ctx.json({ error: "folder_not_found" }, 404);
    }

    const ownerCheck = await requireUserBackedPrincipal(auth, ctx, principal, "access_key_cannot_revoke");
    if (ownerCheck instanceof Response) {
      return ownerCheck;
    }

    const grant = await checkGrant(auth.db, rootNodeId, principal, "write", auth.schema);
    if (!grant.granted) {
      return ctx.json({ error: grant.reason }, 403);
    }

    const invite =
      hasInviteRouteStore(auth.db) && auth.db.getInviteByIdForRoute
        ? await auth.db.getInviteByIdForRoute(inviteId)
        : (
            await auth.db
              .select()
              .from(auth.schema.folderInvites)
              .where(eq(auth.schema.folderInvites.inviteId, inviteId))
              .limit(1)
          )[0];
    if (!invite || invite.folderId !== folderId) {
      return ctx.json({ error: "invite_not_found" }, 404);
    }
    if (invite.status === "accepted") {
      return ctx.json({ error: "invite_already_accepted" }, 409);
    }

    if (hasInviteRouteStore(auth.db) && auth.db.deleteInviteForRoute) {
      await auth.db.deleteInviteForRoute(inviteId);
    } else {
      await auth.db.delete(auth.schema.folderInvites).where(eq(auth.schema.folderInvites.inviteId, inviteId));
    }
    return ctx.body(null, 204);
  });
}

function hasInviteRouteStore(db: CoreAuth["db"]): db is CoreAuth["db"] & InviteRouteStore {
  return (
    "createInviteForRoute" in db ||
    "getInviteForRoute" in db ||
    "getInviteByIdForRoute" in db ||
    "acceptInviteForRoute" in db ||
    "listFolderInvitesForRoute" in db ||
    "deleteInviteForRoute" in db
  );
}
