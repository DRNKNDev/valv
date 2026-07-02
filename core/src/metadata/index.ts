import { Hono } from "hono";

import { createAuthMiddleware, type CoreAuth } from "../auth/index.js";
import type { SendInviteEmail } from "../email/index.js";
import type { MetadataHub, MetadataVariables } from "./common.js";
import { registerFolderRoutes } from "./folders.js";
import { registerGrantRoutes } from "./grants.js";
import { registerInviteRoutes } from "./invites.js";
import { registerDeltaRoutes } from "./delta.js";
import { registerOpRoutes } from "./ops.js";
import { registerVersionRoutes } from "./versions.js";

export type CreateMetadataRouterOptions = {
  db?: CoreAuth["db"];
  auth: CoreAuth;
  hub: MetadataHub;
  sendInviteEmail?: SendInviteEmail;
  onOpCommitted?: (folderId: string, serverSeq: number) => Promise<void>;
};

export function createMetadataRouter(opts: CreateMetadataRouterOptions): Hono<{ Variables: MetadataVariables }> {
  const router = new Hono<{ Variables: MetadataVariables }>();
  router.use("*", createAuthMiddleware(opts.auth));
  registerFolderRoutes(router, opts.auth);
  registerInviteRoutes(router, opts.auth, opts.sendInviteEmail);
  registerGrantRoutes(router, opts.auth);
  registerOpRoutes(router, opts.auth, opts.hub, opts.onOpCommitted);
  registerDeltaRoutes(router, opts.auth);
  registerVersionRoutes(router, opts.auth, opts.hub);
  return router;
}

export type { MetadataHub } from "./common.js";
export { checkGrant } from "./authz.js";
