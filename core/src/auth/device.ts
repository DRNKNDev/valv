import { randomUUID } from "node:crypto";

import { Hono } from "hono";

import {
  type AuthVariables,
  type CoreAuth,
  createAuthMiddleware,
  generateDeviceToken,
  sha256Hex,
} from "./index.js";

export function createDeviceAuthRouter(auth: CoreAuth): Hono<{ Variables: AuthVariables }> {
  const router = new Hono<{ Variables: AuthVariables }>();
  router.use("*", createAuthMiddleware(auth));

  router.post("/device", async (ctx) => {
    const principal = ctx.var.principal;
    if (principal?.type !== "user") {
      return ctx.json({ error: "forbidden" }, 403);
    }

    const body = await ctx.req.json().catch(() => ({}));
    const name = typeof body.name === "string" && body.name.length > 0 ? body.name : "Device";
    const deviceId = randomUUID();
    const token = generateDeviceToken();

    await auth.db.insert(auth.schema.devices).values({
      deviceId,
      userId: principal.userId,
      name,
      tokenHash: sha256Hex(token),
    });

    return ctx.json({ device_id: deviceId, token });
  });

  return router;
}
