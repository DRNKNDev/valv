import type { AwsClient } from "aws4fetch";
import { eq } from "drizzle-orm";
import { Hono } from "hono";

import {
  createAuthMiddleware,
  type AuthVariables,
  type CoreAuth,
  type Principal,
} from "../auth/index.js";
import { checkGrant } from "../metadata/authz.js";

type BatchRequest = {
  operation: "upload" | "download";
  objects: Array<{ oid: string; size: number }>;
};

type BatchResponseObject = {
  oid: string;
  size: number;
  already_exists?: boolean;
  actions?: {
    upload?: { href: string; header?: Record<string, string>; expires_in?: number };
    download?: { href: string; expires_in?: number };
  };
  error?: { code: number; message: string };
};

export type CreateBlobstoreRouterOptions = {
  db?: CoreAuth["db"];
  auth: CoreAuth;
  s3: AwsClient;
  bucketEndpoint?: string;
  bucketName: string;
  getQuota?: (deviceId: string) => Promise<{ quota_bytes: number; usage_bytes: number; subscription_status: string } | null>;
};

export function chunkKey(hash: string): string {
  return `chunks/${hash}`;
}

export function createBlobstoreRouter(opts: CreateBlobstoreRouterOptions): Hono<{ Variables: AuthVariables }> {
  const router = new Hono<{ Variables: AuthVariables }>();
  router.use("*", createAuthMiddleware(opts.auth));

  const handleBatch = async (ctx: any) => {
    const principal = ctx.var.principal;
    if (!principal) {
      return ctx.json({ error: "unauthenticated" }, 401);
    }
    const body = (await ctx.req.json()) as BatchRequest;
    if (body.operation === "upload") {
      const uploadPlans = await Promise.all(
        body.objects.map(async (object) => ({ object, existing: await getExistingChunk(opts, object.oid) })),
      );
      if (opts.getQuota && principal.type === "device") {
        const quota = await opts.getQuota(principal.deviceId);
        if (quota) {
          if (quota.subscription_status === "canceled" || quota.subscription_status === "revoked") {
            return ctx.json({ error: "subscription_inactive", status: quota.subscription_status }, 402);
          }
          const newBytes = uploadPlans
            .filter((plan) => !plan.existing)
            .reduce((sum, plan) => sum + plan.object.size, 0);
          if (quota.usage_bytes + newBytes > quota.quota_bytes) {
            return ctx.json({ error: "over_quota", usage_bytes: quota.usage_bytes, quota_bytes: quota.quota_bytes }, 402);
          }
        }
      }
      const objects = await Promise.all(
        uploadPlans.map((plan) => handleUploadObject(opts, plan.object.oid, plan.object.size, plan.existing)),
      );
      return ctx.json({ transfer: "basic", objects });
    }

    const objects = await Promise.all(
      body.objects.map((object) => handleDownloadObject(opts, principal, object.oid, object.size)),
    );
    return ctx.json({ transfer: "basic", objects });
  };

  router.post("/objects/batch", handleBatch);

  return router;
}

async function handleUploadObject(
  opts: CreateBlobstoreRouterOptions,
  oid: string,
  size: number,
  existing: { refcount: number } | undefined,
): Promise<BatchResponseObject> {
  if (existing && existing.refcount > 0) {
    return { oid, size, already_exists: true };
  }

  if (!existing) {
    await opts.auth.db.insert(opts.auth.schema.chunks).values({ chunkHash: oid, sizeBytes: size, refcount: 0 });
  }
  const href = await presignS3(opts, chunkKey(oid), "PUT", { "Content-Type": "application/octet-stream" });

  return {
    oid,
    size,
    already_exists: false,
    actions: {
      upload: {
        href,
        header: { "Content-Type": "application/octet-stream" },
        expires_in: 900,
      },
    },
  };
}

async function handleDownloadObject(
  opts: CreateBlobstoreRouterOptions,
  principal: Principal,
  oid: string,
  size: number,
): Promise<BatchResponseObject> {
  const authorized = await canDownloadChunk(opts.auth, principal, oid);
  if (!authorized) {
    return { oid, size, error: { code: 403, message: "no grant" } };
  }

  const href = await presignS3(opts, chunkKey(oid), "GET");
  return { oid, size, actions: { download: { href, expires_in: 900 } } };
}

async function getExistingChunk(
  opts: CreateBlobstoreRouterOptions,
  oid: string,
): Promise<{ refcount: number } | undefined> {
  const existing = await opts.auth.db
    .select({ refcount: opts.auth.schema.chunks.refcount })
    .from(opts.auth.schema.chunks)
    .where(eq(opts.auth.schema.chunks.chunkHash, oid))
    .limit(1);
  return existing[0];
}

async function presignS3(
  opts: CreateBlobstoreRouterOptions,
  key: string,
  method: "GET" | "PUT",
  headers?: Record<string, string>,
): Promise<string> {
  const request = await opts.s3.sign(objectUrl(opts, key), {
    method,
    headers,
    aws: { signQuery: true },
  });
  return request.url;
}

export function objectUrl(opts: { bucketEndpoint?: string; bucketName: string }, key: string): string {
  const endpoint = opts.bucketEndpoint ?? `https://${opts.bucketName}.s3.amazonaws.com`;
  const url = new URL(endpoint.endsWith("/") ? endpoint : `${endpoint}/`);
  url.pathname = `${url.pathname}${opts.bucketName}/${key}`.replace(/\/+/g, "/");
  return url.toString();
}

async function canDownloadChunk(auth: CoreAuth, principal: Principal, oid: string): Promise<boolean> {
  const rows = await auth.db
    .select({ nodeId: auth.schema.nodes.nodeId, manifest: auth.schema.versions.manifest })
    .from(auth.schema.nodes)
    .innerJoin(auth.schema.versions, eq(auth.schema.nodes.nodeId, auth.schema.versions.nodeId));

  for (const row of rows) {
    const manifest = Array.isArray(row.manifest) ? row.manifest : [];
    const referencesChunk = manifest.some((chunk: { chunk_hash?: string }) => chunk.chunk_hash === oid);
    if (!referencesChunk) {
      continue;
    }
    const grant = await checkGrant(auth.db, row.nodeId, principal, "read", auth.schema);
    if (grant.granted) {
      return true;
    }
  }
  return false;
}
