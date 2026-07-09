import type { AwsClient } from "aws4fetch";
import { eq, sql } from "drizzle-orm";
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

export type QuotaPrincipal = { type: "device"; id: string } | { type: "user"; id: string };
export type QuotaInfo = {
  quota_bytes: number | null;
  usage_bytes: number;
  subscription_status: string;
  current_period_end: string | Date | null;
};

export type CreateBlobstoreRouterOptions = {
  db?: CoreAuth["db"];
  auth: CoreAuth;
  s3: AwsClient;
  bucketEndpoint?: string;
  bucketName: string;
  chunkKeyForPrincipal?: (oid: string, principal: Principal) => string | Promise<string>;
  getExistingChunkForPrincipal?: (oid: string, principal: Principal) => Promise<{ refcount: number } | undefined>;
  getQuota?: (principal: QuotaPrincipal) => Promise<QuotaInfo | null>;
  pastDueGraceDays?: number;
};

export const CHUNK_AUTH_SCAN_WARN_ROWS = 1_000;
export const CHUNK_AUTH_SCAN_WARN_MS = 100;

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
        body.objects.map(async (object) => ({
          object,
          existing: await getExistingChunkForBatch(opts, object.oid, principal),
        })),
      );
      if (opts.getQuota) {
        const quota = await opts.getQuota(quotaPrincipal(principal));
        if (quota) {
          if (isBlockedSubscription(quota, opts.pastDueGraceDays ?? 5)) {
            return ctx.json({ error: "subscription_inactive", status: quota.subscription_status }, 402);
          }
          const newBytes = uploadPlans
            .filter((plan) => !plan.existing)
            .reduce((sum, plan) => sum + plan.object.size, 0);
          if (quota.quota_bytes !== null && quota.usage_bytes + newBytes > quota.quota_bytes) {
            return ctx.json({ error: "over_quota", usage_bytes: quota.usage_bytes, quota_bytes: quota.quota_bytes }, 402);
          }
        }
      }
      const objects = await Promise.all(
        uploadPlans.map((plan) => handleUploadObject(opts, principal, plan.object.oid, plan.object.size, plan.existing)),
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
  principal: Principal,
  oid: string,
  size: number,
  existing: { refcount: number } | undefined,
): Promise<BatchResponseObject> {
  if (existing && existing.refcount > 0) {
    return { oid, size, already_exists: true };
  }

  if (opts.getExistingChunkForPrincipal) {
    await insertChunkIfAbsent(opts, oid, size);
  } else if (!existing) {
    const inserted = await insertChunkIfAbsent(opts, oid, size);
    if (!inserted) {
      return { oid, size, already_exists: true };
    }
  }
  const uploadHeaders = { "Content-Type": "application/octet-stream", "Content-Length": String(size) };
  const href = await presignS3(opts, await resolveChunkKey(opts, oid, principal), "PUT", uploadHeaders);

  return {
    oid,
    size,
    already_exists: false,
    actions: {
      upload: {
        href,
        header: uploadHeaders,
        expires_in: 900,
      },
    },
  };
}

function quotaPrincipal(principal: Principal): QuotaPrincipal {
  return principal.type === "device"
    ? { type: "device", id: principal.deviceId }
    : { type: "user", id: principal.userId };
}

export function withinGracePeriod(currentPeriodEnd: string | Date | null, graceDays: number, now = new Date()): boolean {
  if (!currentPeriodEnd) {
    return false;
  }
  const periodEnd = currentPeriodEnd instanceof Date ? currentPeriodEnd : new Date(currentPeriodEnd);
  if (Number.isNaN(periodEnd.getTime())) {
    return false;
  }
  const graceMs = Math.max(0, graceDays) * 24 * 60 * 60 * 1000;
  return now.getTime() <= periodEnd.getTime() + graceMs;
}

function isBlockedSubscription(quota: QuotaInfo, graceDays: number): boolean {
  const blockedStatuses = new Set(["none", "incomplete", "canceled", "revoked"]);
  return blockedStatuses.has(quota.subscription_status)
    || (quota.subscription_status === "past_due" && !withinGracePeriod(quota.current_period_end, graceDays));
}

async function insertChunkIfAbsent(opts: CreateBlobstoreRouterOptions, oid: string, size: number): Promise<boolean> {
  const insert = opts.auth.db
    .insert(opts.auth.schema.chunks)
    .values({ chunkHash: oid, sizeBytes: size, refcount: 0 }) as any;
  if (typeof insert.onConflictDoNothing !== "function") {
    await insert;
    return true;
  }

  const onConflict = insert.onConflictDoNothing({ target: opts.auth.schema.chunks.chunkHash });
  if (typeof onConflict.returning === "function") {
    const rows = await onConflict.returning({ chunkHash: opts.auth.schema.chunks.chunkHash });
    return rows.length > 0;
  }
  await onConflict;
  return true;
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

  const href = await presignS3(opts, await resolveChunkKey(opts, oid, principal), "GET");
  return { oid, size, actions: { download: { href, expires_in: 900 } } };
}

async function resolveChunkKey(opts: CreateBlobstoreRouterOptions, oid: string, principal: Principal): Promise<string> {
  return opts.chunkKeyForPrincipal ? opts.chunkKeyForPrincipal(oid, principal) : chunkKey(oid);
}

async function getExistingChunkForBatch(
  opts: CreateBlobstoreRouterOptions,
  oid: string,
  principal: Principal,
): Promise<{ refcount: number } | undefined> {
  return opts.getExistingChunkForPrincipal
    ? opts.getExistingChunkForPrincipal(oid, principal)
    : getExistingChunk(opts, oid);
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
    aws: { signQuery: true, allHeaders: true },
  });
  return request.url;
}

export function objectUrl(opts: { bucketEndpoint?: string; bucketName: string }, key: string): string {
  const endpoint = opts.bucketEndpoint ?? `https://${opts.bucketName}.s3.amazonaws.com`;
  const url = new URL(endpoint.endsWith("/") ? endpoint : `${endpoint}/`);
  url.pathname = `${url.pathname}${opts.bucketName}/${key}`.replace(/\/+/g, "/");
  return url.toString();
}

export async function canDownloadChunkUnscopedForParity(auth: CoreAuth, principal: Principal, oid: string): Promise<boolean> {
  const nodeIds = await unscopedChunkCandidateNodeIds(auth, oid);
  return canDownloadFromCandidateNodes(auth, principal, nodeIds);
}

export async function canDownloadChunkScopedForParity(auth: CoreAuth, principal: Principal, oid: string): Promise<boolean> {
  const nodeIds = await scopedChunkCandidateNodeIds(auth, oid);
  return canDownloadFromCandidateNodes(auth, principal, nodeIds);
}

async function canDownloadChunk(auth: CoreAuth, principal: Principal, oid: string): Promise<boolean> {
  return canDownloadChunkScopedForParity(auth, principal, oid);
}

async function unscopedChunkCandidateNodeIds(auth: CoreAuth, oid: string): Promise<string[]> {
  const scanStartedAt = Date.now();
  const rows = await auth.db
    .select({ nodeId: auth.schema.nodes.nodeId, manifest: auth.schema.versions.manifest })
    .from(auth.schema.nodes)
    .innerJoin(auth.schema.versions, eq(auth.schema.nodes.nodeId, auth.schema.versions.nodeId));
  const durationMs = Date.now() - scanStartedAt;
  warnOnOversizedChunkAuthScan(oid, rows.length, durationMs);

  const nodeIds: string[] = [];
  for (const row of rows) {
    const manifest = Array.isArray(row.manifest) ? row.manifest : [];
    const referencesChunk = manifest.some((chunk: { chunk_hash?: string }) => chunk.chunk_hash === oid);
    if (!referencesChunk) {
      continue;
    }
    nodeIds.push(row.nodeId);
  }
  return nodeIds;
}

async function scopedChunkCandidateNodeIds(auth: CoreAuth, oid: string): Promise<string[]> {
  const scanStartedAt = Date.now();
  const rows = await executeRows(auth.db, sql`
    SELECT DISTINCT node_id FROM version_chunks WHERE chunk_hash = ${oid}
  `);
  const durationMs = Date.now() - scanStartedAt;
  warnOnOversizedChunkAuthScan(oid, rows.length, durationMs);

  return rows.map((row) => String(row.node_id ?? row.nodeId));
}

async function canDownloadFromCandidateNodes(
  auth: CoreAuth,
  principal: Principal,
  nodeIds: string[],
): Promise<boolean> {
  for (const nodeId of nodeIds) {
    const grant = await checkGrant(auth.db, nodeId, principal, "read", auth.schema);
    if (grant.granted) {
      return true;
    }
  }
  return false;
}

async function executeRows(db: CoreAuth["db"], query: unknown): Promise<any[]> {
  if (typeof db.all === "function") {
    return db.all(query);
  }
  if (typeof db.execute !== "function") {
    return [];
  }
  const result = await db.execute(query);
  if (Array.isArray(result)) {
    return result;
  }
  if (Array.isArray(result.rows)) {
    return result.rows;
  }
  return [];
}

function warnOnOversizedChunkAuthScan(oid: string, rowCount: number, durationMs: number): void {
  if (rowCount <= CHUNK_AUTH_SCAN_WARN_ROWS && durationMs <= CHUNK_AUTH_SCAN_WARN_MS) {
    return;
  }
  console.warn("canDownloadChunk scanned an oversized candidate set", { oid, rowCount, durationMs });
}
