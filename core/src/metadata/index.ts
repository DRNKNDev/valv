import { Hono } from "hono";
import { createMiddleware } from "hono/factory";
import { PROTOCOL_HEADER } from "@valv/contracts-sync";

import { createAuthMiddleware, type CoreAuth } from "../auth/index.js";
import type { SendInviteEmail } from "../email/index.js";
import type { MetadataHub, MetadataVariables } from "./common.js";
import { registerFolderRoutes } from "./folders.js";
import { registerGrantRoutes, type OnGrantCreated } from "./grants.js";
import { registerInviteRoutes } from "./invites.js";
import { registerDeltaRoutes } from "./delta.js";
import { registerOpRoutes, type CommittedOp } from "./ops.js";
import { registerVersionRoutes } from "./versions.js";

export type CreateMetadataRouterOptions = {
  db?: CoreAuth["db"];
  auth: CoreAuth;
  hub: MetadataHub;
  sendInviteEmail?: SendInviteEmail;
  onFolderCreated?: (info: { folderId: string; ownerUserId: string; grantId: string }) => Promise<void>;
  onGrantCreated?: OnGrantCreated;
  onGrantDeviceCreated?: (info: { folderId: string; scopeNodeId: string; deviceId: string; grantId: string }) => Promise<void>;
  checkPlanForGrant?: (folderId: string) => Promise<{ allowed: boolean; status?: string } | null>;
  onOpCommitted?: (op: CommittedOp) => Promise<void>;
  minProtocolVersion?: number;
};

export function createMetadataRouter(opts: CreateMetadataRouterOptions): Hono<{ Variables: MetadataVariables }> {
  const router = new Hono<{ Variables: MetadataVariables }>();
  const protocolVersionMiddleware = createProtocolVersionMiddleware(opts.minProtocolVersion);
  router.use("/folders/:id/ops", protocolVersionMiddleware);
  router.use("/folders/:id/tree", protocolVersionMiddleware);
  router.use("*", createAuthMiddleware(opts.auth));
  registerFolderRoutes(router, opts.auth, opts.onFolderCreated);
  registerInviteRoutes(router, opts.auth, opts.sendInviteEmail);
  registerGrantRoutes(router, opts.auth, {
    onGrantCreated: opts.onGrantCreated,
    onGrantDeviceCreated: opts.onGrantDeviceCreated,
    checkPlan: opts.checkPlanForGrant,
  });
  registerOpRoutes(router, opts.auth, opts.hub, opts.onOpCommitted);
  registerDeltaRoutes(router, opts.auth);
  registerVersionRoutes(router, opts.auth, opts.hub, opts.onOpCommitted);
  return router;
}

export function createProtocolVersionMiddleware(minVersion: number | undefined) {
  return createMiddleware(async (ctx, next) => {
    if (minVersion === undefined) {
      await next();
      return;
    }
    const rawVersion = ctx.req.header(PROTOCOL_HEADER);
    const version = rawVersion === undefined ? 0 : Number.parseInt(rawVersion, 10);
    if (!Number.isFinite(version) || version < minVersion) {
      return ctx.json(
        {
          error: "protocol_too_old",
          min_protocol: minVersion,
          message: `Valv protocol ${minVersion} or newer is required. Update Valv to keep syncing.`,
        },
        426,
      );
    }
    await next();
  });
}

export type { MetadataHub } from "./common.js";
export type { CommittedOp } from "./ops.js";
export type { OnGrantCreated } from "./grants.js";
export { checkGrant } from "./authz.js";
