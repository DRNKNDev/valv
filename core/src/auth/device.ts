import { randomUUID } from "node:crypto";

import { Hono } from "hono";

import {
  type AuthVariables,
  type CoreAuth,
  createAuthMiddleware,
  generateDeviceToken,
  sha256Hex,
} from "./index.js";

export type DeviceAuthRouterOptions = {
  checkPlan?: (userId: string) => Promise<{ allowed: boolean; status?: string } | null>;
  createDeviceForRoute?: (opts: { deviceId: string; userId: string; name: string; tokenHash: string }) => Promise<void>;
  onDeviceCreated?: (info: { deviceId: string; userId: string }) => Promise<void>;
};

export function createDeviceAuthRouter(auth: CoreAuth, opts: DeviceAuthRouterOptions = {}): Hono<{ Variables: AuthVariables }> {
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

    const plan = opts.checkPlan ? await opts.checkPlan(principal.userId) : null;
    if (plan?.allowed === false) {
      return ctx.json({ error: "subscription_inactive", status: plan.status }, 402);
    }

    const tokenHash = sha256Hex(token);
    if (opts.createDeviceForRoute) {
      await opts.createDeviceForRoute({ deviceId, userId: principal.userId, name, tokenHash });
    } else {
      await auth.db.insert(auth.schema.devices).values({
        deviceId,
        userId: principal.userId,
        name,
        tokenHash,
      });
    }

    if (opts.onDeviceCreated) {
      try {
        await opts.onDeviceCreated({ deviceId, userId: principal.userId });
      } catch (error) {
        console.error("onDeviceCreated hook failed", error);
      }
    }

    return ctx.json({ device_id: deviceId, token });
  });

  return router;
}
