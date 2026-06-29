import { createHash, randomBytes } from "node:crypto";

import { betterAuth, type Auth } from "better-auth";
import { drizzleAdapter } from "better-auth/adapters/drizzle";
import { eq } from "drizzle-orm";
import type { Context, MiddlewareHandler } from "hono";
import { createMiddleware } from "hono/factory";

import { pgSchema, sqliteSchema } from "../db/schema.js";

export type Principal =
  | { type: "user"; userId: string }
  | { type: "device"; deviceId: string };

export type UnauthenticatedPrincipal = { type: "unauthenticated" };

export type AuthResult = Principal | UnauthenticatedPrincipal;

export type CoreSchema = typeof pgSchema | typeof sqliteSchema;

export type CoreDb = {
  all?: any;
  select: any;
  insert: any;
  update: any;
  delete: any;
  execute?: any;
  transaction?: any;
};

export type CoreAuth = Auth<any> & {
  db: CoreDb;
  schema: CoreSchema;
};

export type CreateAuthOptions = {
  secret: string;
  baseURL?: string;
  provider?: "pg" | "sqlite";
  schema?: CoreSchema;
};

export type AuthVariables = {
  auth?: CoreAuth;
  principal?: Principal;
};

export function sha256Hex(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

export function generateDeviceToken(): string {
  return randomBytes(32).toString("base64url");
}

export function createAuth(db: CoreDb, opts: CreateAuthOptions): CoreAuth {
  const schema = opts.schema ?? (opts.provider === "sqlite" ? sqliteSchema : pgSchema);
  const auth = betterAuth({
    secret: opts.secret,
    baseURL: opts.baseURL,
    database: drizzleAdapter(db, {
      provider: opts.provider ?? "pg",
      schema,
    }),
    emailAndPassword: {
      enabled: true,
    },
  });

  return Object.assign(auth, { db, schema });
}

export async function authenticateRequest(
  ctx: Context<{ Variables: AuthVariables }>,
  auth = ctx.var.auth,
): Promise<AuthResult> {
  if (!auth) {
    return { type: "unauthenticated" };
  }

  const rawToken = getBearerToken(ctx) ?? ctx.req.query("token");
  if (rawToken) {
    const tokenHash = sha256Hex(rawToken);
    const rows = await auth.db
      .select({ deviceId: auth.schema.devices.deviceId })
      .from(auth.schema.devices)
      .where(eq(auth.schema.devices.tokenHash, tokenHash))
      .limit(1);
    const device = rows[0];
    if (device) {
      const principal: Principal = { type: "device", deviceId: device.deviceId };
      ctx.set("principal", principal);
      return principal;
    }
  }

  const session = await auth.api.getSession({ headers: ctx.req.raw.headers });
  const userId = session?.user?.id;
  if (userId) {
    const principal: Principal = { type: "user", userId };
    ctx.set("principal", principal);
    return principal;
  }

  return { type: "unauthenticated" };
}

export function createAuthMiddleware(auth: CoreAuth): MiddlewareHandler<{ Variables: AuthVariables }> {
  return createMiddleware<{ Variables: AuthVariables }>(async (ctx, next) => {
    ctx.set("auth", auth);
    const principal = await authenticateRequest(ctx, auth);
    if (principal.type === "unauthenticated") {
      return ctx.json({ error: "unauthenticated" }, 401);
    }
    await next();
  });
}

function getBearerToken(ctx: Context): string | undefined {
  const authorization = ctx.req.header("authorization");
  const [scheme, token] = authorization?.split(" ") ?? [];
  if (scheme?.toLowerCase() !== "bearer" || !token) {
    return undefined;
  }
  return token;
}
