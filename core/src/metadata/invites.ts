import { randomBytes } from "node:crypto";

import { Hono } from "hono";

import type { CoreAuth } from "../auth/index.js";
import type { SendInviteEmail } from "../email/index.js";
import {
  and,
  eq,
  getFolderRoot,
  inTransaction,
  newId,
  requirePrincipal,
  type MetadataVariables,
} from "./common.js";
import { checkGrant } from "./authz.js";

type InviteRouteStore = {
  createInviteForRoute?: (opts: {
    inviteToken: string;
    folderId: string;
    scopeNodeId: string;
    invitedEmail: string;
    invitedByUserId: string;
    canWrite: boolean;
    expiresAt: Date;
  }) => Promise<{ folderName: string }>;
  getInviteForRoute?: (inviteToken: string) => Promise<{
    inviteToken: string;
    folderId: string;
    scopeNodeId: string;
    canWrite: boolean;
    status: "pending" | "accepted" | "revoked" | "expired";
    expiresAt: Date;
  } | undefined>;
  acceptInviteForRoute?: (opts: {
    inviteToken: string;
    userId: string;
    folderId: string;
    scopeNodeId: string;
    canWrite: boolean;
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

    // Defaults to true (read-write) to preserve existing behavior for callers that
    // don't pass this field. Gated on the caller's own write capability above, not on
    // what's being requested for the invite - same principle as POST
    // /folders/:id/grants's can_write (folder-grants spec).
    const canWrite = typeof body.can_write === "boolean" ? body.can_write : true;

    const inviteToken = randomBytes(32).toString("base64url");
    const expiresAt = new Date(Date.now() + 7 * 24 * 60 * 60 * 1000);
    let folderName = "folder";

    if (hasInviteRouteStore(auth.db) && auth.db.createInviteForRoute) {
      const created = await auth.db.createInviteForRoute({
        inviteToken,
        folderId,
        scopeNodeId,
        invitedEmail,
        invitedByUserId: principal.type === "user" ? principal.userId : principal.deviceId,
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
        inviteToken,
        folderId,
        scopeNodeId,
        invitedEmail,
        invitedByUserId: principal.type === "user" ? principal.userId : principal.deviceId,
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
}

function hasInviteRouteStore(db: CoreAuth["db"]): db is CoreAuth["db"] & InviteRouteStore {
  return "createInviteForRoute" in db || "getInviteForRoute" in db || "acceptInviteForRoute" in db;
}
