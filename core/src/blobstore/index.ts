import { GetObjectCommand, PutObjectCommand, type S3Client } from "@aws-sdk/client-s3";
import { getSignedUrl } from "@aws-sdk/s3-request-presigner";
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
  actions?: {
    upload?: { href: string; header?: Record<string, string>; expires_in?: number };
    download?: { href: string; expires_in?: number };
  };
  error?: { code: number; message: string };
};

export type CreateBlobstoreRouterOptions = {
  db?: CoreAuth["db"];
  auth: CoreAuth;
  s3Client: S3Client;
  bucketName: string;
};

export function chunkKey(hash: string): string {
  return `chunks/${hash}`;
}

export function createBlobstoreRouter(opts: CreateBlobstoreRouterOptions): Hono<{ Variables: AuthVariables }> {
  const router = new Hono<{ Variables: AuthVariables }>();
  router.use("*", createAuthMiddleware(opts.auth));

  router.post("/objects/batch", async (ctx) => {
    const principal = ctx.var.principal;
    if (!principal) {
      return ctx.json({ error: "unauthenticated" }, 401);
    }
    const body = (await ctx.req.json()) as BatchRequest;
    if (body.operation === "upload") {
      const objects = await Promise.all(
        body.objects.map((object) => handleUploadObject(opts, object.oid, object.size)),
      );
      return ctx.json({ transfer: "basic", objects });
    }

    const objects = await Promise.all(
      body.objects.map((object) => handleDownloadObject(opts, principal, object.oid, object.size)),
    );
    return ctx.json({ transfer: "basic", objects });
  });

  return router;
}

async function handleUploadObject(
  opts: CreateBlobstoreRouterOptions,
  oid: string,
  size: number,
): Promise<BatchResponseObject> {
  const existing = await opts.auth.db
    .select({ chunkHash: opts.auth.schema.chunks.chunkHash, refcount: opts.auth.schema.chunks.refcount })
    .from(opts.auth.schema.chunks)
    .where(eq(opts.auth.schema.chunks.chunkHash, oid))
    .limit(1);
  if (existing[0] && existing[0].refcount > 0) {
    return { oid, size };
  }

  if (!existing[0]) {
    await opts.auth.db.insert(opts.auth.schema.chunks).values({ chunkHash: oid, sizeBytes: size, refcount: 0 });
  }
  const href = await getSignedUrl(
    opts.s3Client,
    new PutObjectCommand({
      Bucket: opts.bucketName,
      Key: chunkKey(oid),
      ContentType: "application/octet-stream",
    }),
    { expiresIn: 900 },
  );

  return {
    oid,
    size,
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

  const href = await getSignedUrl(
    opts.s3Client,
    new GetObjectCommand({ Bucket: opts.bucketName, Key: chunkKey(oid) }),
    { expiresIn: 900 },
  );
  return { oid, size, actions: { download: { href, expires_in: 900 } } };
}

async function canDownloadChunk(auth: CoreAuth, principal: Principal, oid: string): Promise<boolean> {
  const rows = await auth.db
    .select({ nodeId: auth.schema.nodes.nodeId, manifest: auth.schema.versions.manifest })
    .from(auth.schema.nodes)
    .innerJoin(auth.schema.versions, eq(auth.schema.nodes.currentVersionId, auth.schema.versions.versionId));

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
