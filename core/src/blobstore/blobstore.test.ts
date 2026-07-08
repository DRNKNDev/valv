import { afterEach, describe, expect, it, vi } from "vitest";

import type { CoreAuth, CoreDb, Principal } from "../auth/index.js";
import { pgSchema } from "../db/schema.js";
import { CHUNK_AUTH_SCAN_WARN_ROWS, chunkKey, createBlobstoreRouter } from "./index.js";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("blobstore batch coordination", () => {
  it("computes the OSS chunk key layout", () => {
    expect(chunkKey("abc123")).toBe("chunks/abc123");
  });

  it("deduplicates upload hits and stages upload misses", async () => {
    const db = new BlobTestDb({ existingChunks: [{ chunkHash: "known", refcount: 1 }] });
    const app = appFor(db);

    const response = await app.request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({
        operation: "upload",
        objects: [
          { oid: "known", size: 1 },
          { oid: "new", size: 2 },
        ],
      }),
      headers: { "content-type": "application/json" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.transfer).toBe("basic");
    expect(body.objects[0]).toEqual({ oid: "known", size: 1, already_exists: true });
    expect(body.objects[1]).toEqual({
      oid: "new",
      size: 2,
      already_exists: false,
      actions: {
        upload: {
          href: "signed:PutObjectCommand:chunks/new",
          header: { "Content-Type": "application/octet-stream", "Content-Length": "2" },
          expires_in: 900,
        },
      },
    });
    expect(db.insertedChunks).toEqual([{ chunkHash: "new", sizeBytes: 2, refcount: 0 }]);
  });

  it("retries upload for existing chunks that are not referenced yet", async () => {
    const db = new BlobTestDb({ existingChunks: [{ chunkHash: "pending", refcount: 0 }] });
    const app = appFor(db);

    const response = await app.request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "upload", objects: [{ oid: "pending", size: 3 }] }),
      headers: { "content-type": "application/json" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.objects).toEqual([
      {
        oid: "pending",
        size: 3,
        already_exists: false,
        actions: {
          upload: {
            href: "signed:PutObjectCommand:chunks/pending",
            header: { "Content-Type": "application/octet-stream", "Content-Length": "3" },
            expires_in: 900,
          },
        },
      },
    ]);
    expect(db.insertedChunks).toEqual([]);
  });

  it.each(["none", "incomplete"])("rejects %s subscriptions before issuing upload URLs", async (status) => {
    const getQuota = vi.fn(async () => ({
      quota_bytes: 100,
      usage_bytes: 0,
      subscription_status: status,
      current_period_end: null,
    }));
    const response = await appFor(new BlobTestDb(), { type: "device", deviceId: "device-1" }, { getQuota }).request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "upload", objects: [{ oid: "new", size: 2 }] }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });

    expect(response.status).toBe(402);
    await expect(response.json()).resolves.toEqual({ error: "subscription_inactive", status });
  });

  it("allows past_due subscriptions within the grace period", async () => {
    const currentPeriodEnd = new Date(Date.now() - 2 * 24 * 60 * 60 * 1000).toISOString();
    const response = await appFor(new BlobTestDb(), { type: "device", deviceId: "device-1" }, {
      getQuota: async () => ({
        quota_bytes: 100,
        usage_bytes: 0,
        subscription_status: "past_due",
        current_period_end: currentPeriodEnd,
      }),
      pastDueGraceDays: 5,
    }).request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "upload", objects: [{ oid: "new", size: 2 }] }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });

    expect(response.status).toBe(200);
  });

  it("rejects past_due subscriptions after the grace period", async () => {
    const currentPeriodEnd = new Date(Date.now() - 10 * 24 * 60 * 60 * 1000).toISOString();
    const response = await appFor(new BlobTestDb(), { type: "device", deviceId: "device-1" }, {
      getQuota: async () => ({
        quota_bytes: 100,
        usage_bytes: 0,
        subscription_status: "past_due",
        current_period_end: currentPeriodEnd,
      }),
      pastDueGraceDays: 5,
    }).request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "upload", objects: [{ oid: "new", size: 2 }] }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });

    expect(response.status).toBe(402);
    await expect(response.json()).resolves.toEqual({ error: "subscription_inactive", status: "past_due" });
  });

  it("applies quota checks to user principals", async () => {
    const getQuota = vi.fn(async () => ({
      quota_bytes: 10,
      usage_bytes: 10,
      subscription_status: "active",
      current_period_end: null,
    }));
    const response = await appFor(new BlobTestDb(), { type: "user", userId: "user-1" }, { getQuota }).request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "upload", objects: [{ oid: "new", size: 1 }] }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(402);
    await expect(response.json()).resolves.toEqual({ error: "over_quota", usage_bytes: 10, quota_bytes: 10 });
    expect(getQuota).toHaveBeenCalledWith({ type: "user", id: "user-1" });
  });

  it("treats duplicate first-time chunk inserts in one batch as deduped", async () => {
    const db = new BlobTestDb({ atomicConflicts: true });
    const response = await appFor(db).request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({
        operation: "upload",
        objects: [
          { oid: "new", size: 2 },
          { oid: "new", size: 2 },
        ],
      }),
      headers: { "content-type": "application/json" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(db.insertedChunks).toEqual([{ chunkHash: "new", sizeBytes: 2, refcount: 0 }]);
    expect(body.objects).toEqual([
      expect.objectContaining({ oid: "new", already_exists: false }),
      { oid: "new", size: 2, already_exists: true },
    ]);
  });

  it("keeps default upload signing and global existence behavior when hooks are absent", async () => {
    const db = new BlobTestDb({ atomicConflicts: true, existingChunks: [{ chunkHash: "known", refcount: 1 }] });
    const app = appFor(db, { type: "user", userId: "user-1" }, {});
    const s3 = (app as any).s3;

    const response = await app.request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({
        operation: "upload",
        objects: [
          { oid: "known", size: 1 },
          { oid: "new", size: 7 },
          { oid: "known", size: 1 },
        ],
      }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.objects[0]).toEqual({ oid: "known", size: 1, already_exists: true });
    expect(body.objects[1]).toEqual({
      oid: "new",
      size: 7,
      already_exists: false,
      actions: {
        upload: {
          href: "signed:PutObjectCommand:chunks/new",
          header: { "Content-Type": "application/octet-stream", "Content-Length": "7" },
          expires_in: 900,
        },
      },
    });
    expect(body.objects[2]).toEqual({ oid: "known", size: 1, already_exists: true });
    expect(db.globalChunkSelectCount).toBeGreaterThanOrEqual(3);
    expect(s3.sign).toHaveBeenCalledWith(
      "https://bucket.s3.amazonaws.com/bucket/chunks/new",
      expect.objectContaining({
        method: "PUT",
        headers: { "Content-Type": "application/octet-stream", "Content-Length": "7" },
        aws: { signQuery: true, allHeaders: true },
      }),
    );
  });

  it("uses chunkKeyForPrincipal for upload and download keys", async () => {
    const db = new BlobTestDb({ authorized: true, downloadRows: [{ nodeId: "doc", manifest: [{ chunk_hash: "oid-1" }] }] });
    const chunkKeyForPrincipal = vi.fn(async (oid: string, principal: Principal) => {
      expect(principal).toEqual({ type: "device", deviceId: "device-1" });
      return `chunks/tenant-a/${oid}`;
    });
    const app = appFor(db, { type: "device", deviceId: "device-1" }, { chunkKeyForPrincipal });

    const upload = await app.request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "upload", objects: [{ oid: "oid-1", size: 10 }] }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });
    const uploadBody = await upload.json();
    expect(uploadBody.objects[0].actions.upload.href).toBe("signed:PutObjectCommand:chunks/tenant-a/oid-1");

    const download = await app.request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "download", objects: [{ oid: "oid-1", size: 10 }] }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });
    const downloadBody = await download.json();
    expect(downloadBody.objects[0].actions.download.href).toBe("signed:GetObjectCommand:chunks/tenant-a/oid-1");
    expect(chunkKeyForPrincipal).toHaveBeenCalledTimes(2);
  });

  it("uses getExistingChunkForPrincipal instead of global lookup and ignores global insert conflicts", async () => {
    const db = new BlobTestDb({ atomicConflicts: true, existingChunks: [{ chunkHash: "shared", refcount: 1 }] });
    const getExistingChunkForPrincipal = vi.fn(async () => undefined);
    const app = appFor(db, { type: "device", deviceId: "device-b" }, {
      chunkKeyForPrincipal: (oid) => `chunks/tenant-b/${oid}`,
      getExistingChunkForPrincipal,
    });

    const response = await app.request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "upload", objects: [{ oid: "shared", size: 4 }] }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(getExistingChunkForPrincipal).toHaveBeenCalledWith("shared", { type: "device", deviceId: "device-b" });
    expect(db.globalChunkSelectCount).toBe(0);
    expect(body.objects).toEqual([
      {
        oid: "shared",
        size: 4,
        already_exists: false,
        actions: {
          upload: {
            href: "signed:PutObjectCommand:chunks/tenant-b/shared",
            header: { "Content-Type": "application/octet-stream", "Content-Length": "4" },
            expires_in: 900,
          },
        },
      },
    ]);
    expect(db.insertedChunks).toEqual([]);
  });

  it("returns already_exists from the per-principal hook when the tenant refcount is positive", async () => {
    const db = new BlobTestDb({ atomicConflicts: true });
    const app = appFor(db, { type: "device", deviceId: "device-a" }, {
      getExistingChunkForPrincipal: async () => ({ refcount: 1 }),
    });

    const response = await app.request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "upload", objects: [{ oid: "known", size: 3 }] }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.objects).toEqual([{ oid: "known", size: 3, already_exists: true }]);
    expect(db.insertedChunks).toEqual([]);
  });

  it("issues download URLs only when a grant covers a referencing node", async () => {
    const db = new BlobTestDb({ authorized: true, downloadRows: [{ nodeId: "doc", manifest: [{ chunk_hash: "oid-1" }] }] });
    const app = appFor(db, { type: "device", deviceId: "device-1" });

    const response = await app.request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "download", objects: [{ oid: "oid-1", size: 10 }] }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.objects).toEqual([
      { oid: "oid-1", size: 10, actions: { download: { href: "signed:GetObjectCommand:chunks/oid-1", expires_in: 900 } } },
    ]);
  });

  it("logs oversized download authorization scans without changing the response", async () => {
    const consoleWarn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const downloadRows = Array.from({ length: CHUNK_AUTH_SCAN_WARN_ROWS + 1 }, (_value, index) => ({
      nodeId: `doc-${index}`,
      manifest: [{ chunk_hash: "oid-1" }],
    }));
    const db = new BlobTestDb({ authorized: true, downloadRows });
    const app = appFor(db, { type: "device", deviceId: "device-1" });

    const response = await app.request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "download", objects: [{ oid: "oid-1", size: 10 }] }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.objects).toEqual([
      { oid: "oid-1", size: 10, actions: { download: { href: "signed:GetObjectCommand:chunks/oid-1", expires_in: 900 } } },
    ]);
    expect(consoleWarn).toHaveBeenCalledWith("canDownloadChunk scanned an oversized candidate set", {
      oid: "oid-1",
      rowCount: CHUNK_AUTH_SCAN_WARN_ROWS + 1,
      durationMs: expect.any(Number),
    });
  });

  it("does not log normal-sized download authorization scans", async () => {
    const consoleWarn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const db = new BlobTestDb({ authorized: true, downloadRows: [{ nodeId: "doc", manifest: [{ chunk_hash: "oid-1" }] }] });
    const app = appFor(db, { type: "device", deviceId: "device-1" });

    const response = await app.request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "download", objects: [{ oid: "oid-1", size: 10 }] }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });

    expect(response.status).toBe(200);
    expect(consoleWarn).not.toHaveBeenCalled();
  });

  it("returns per-object 403 when no grant covers a referencing node", async () => {
    const db = new BlobTestDb({ authorized: false, downloadRows: [{ nodeId: "doc", manifest: [{ chunk_hash: "oid-1" }] }] });
    const app = appFor(db, { type: "device", deviceId: "device-1" });

    const response = await app.request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "download", objects: [{ oid: "oid-1", size: 10 }] }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.objects).toEqual([{ oid: "oid-1", size: 10, error: { code: 403, message: "no grant" } }]);
  });

  it("rejects unauthenticated uploads", async () => {
    const response = await appFor(new BlobTestDb(), null).request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "upload", objects: [{ oid: "new", size: 2 }] }),
      headers: { "content-type": "application/json" },
    });

    expect(response.status).toBe(401);
  });
});

class BlobTestDb implements CoreDb {
  update: CoreDb["update"];
  delete: CoreDb["delete"];
  insertedChunks: Array<{ chunkHash: string; sizeBytes: number; refcount: number }> = [];
  private existingChunks: Array<{ chunkHash: string; refcount: number }>;
  private downloadRows: Array<{ nodeId: string; manifest: Array<{ chunk_hash: string }> }>;
  private authorized: boolean;
  private atomicConflicts: boolean;
  private selectCalls = 0;

  constructor(opts: { existingChunks?: Array<{ chunkHash: string; refcount: number }>; downloadRows?: Array<{ nodeId: string; manifest: Array<{ chunk_hash: string }> }>; authorized?: boolean; atomicConflicts?: boolean } = {}) {
    this.existingChunks = opts.existingChunks ?? [];
    this.downloadRows = opts.downloadRows ?? [];
    this.authorized = opts.authorized ?? true;
    this.atomicConflicts = opts.atomicConflicts ?? false;
  }

  get globalChunkSelectCount(): number {
    return this.selectCalls;
  }

  select(): any {
    const callIndex = this.selectCalls;
    this.selectCalls += 1;
    return {
      from: () => ({
        where: (_condition: unknown) => ({
          limit: async () => this.chunkSelectRows(callIndex),
        }),
        innerJoin: async () => this.downloadRows,
      }),
    };
  }

  insert(): any {
    return {
      values: (value: { chunkHash: string; sizeBytes: number; refcount: number }) => {
        const insert = async () => {
          this.insertedChunks.push(value);
          this.existingChunks.push({ chunkHash: value.chunkHash, refcount: value.refcount });
          return [{ chunkHash: value.chunkHash }];
        };
        if (!this.atomicConflicts) {
          return insert();
        }
        return {
          onConflictDoNothing: () => ({
            returning: async () => {
              if (this.existingChunks.some((chunk) => chunk.chunkHash === value.chunkHash)) {
                return [];
              }
              return insert();
            },
          }),
          then: (resolve: (value: unknown) => unknown, reject: (reason: unknown) => unknown) => insert().then(resolve, reject),
        };
      },
    };
  }

  async execute(query: any): Promise<Array<{ node_id: string }>> {
    const chunkHash = query.queryChunks?.find((chunk: unknown) => typeof chunk === "string");
    const nodeIds = new Set(
      this.downloadRows
        .filter((row) => row.manifest.some((chunk) => chunk.chunk_hash === chunkHash))
        .map((row) => row.nodeId),
    );
    return [...nodeIds].map((nodeId) => ({ node_id: nodeId }));
  }

  async getNodeForAuthz(nodeId: string): Promise<{ nodeId: string; folderId: string; parentId: string | null } | undefined> {
    return { nodeId, folderId: "folder-1", parentId: null };
  }

  async getGrantForAuthz(_opts: { principal: Principal }): Promise<{ grantId: string; scopeNodeId: string; canRead: boolean; canWrite: boolean } | undefined> {
    return this.authorized ? { grantId: "grant-1", scopeNodeId: "doc", canRead: true, canWrite: false } : undefined;
  }

  private chunkSelectRows(callIndex: number): Array<{ chunkHash: string; refcount: number }> {
    const known = this.existingChunks[callIndex];
    return known ? [known] : [];
  }
}

function appFor(
  db: BlobTestDb,
  principal: Principal | null = { type: "user", userId: "user-1" },
  opts: {
    getQuota?: (principal: { type: "device" | "user"; id: string }) => Promise<{
      quota_bytes: number;
      usage_bytes: number;
      subscription_status: string;
      current_period_end: string | null;
    } | null>;
    pastDueGraceDays?: number;
    chunkKeyForPrincipal?: (oid: string, principal: Principal) => string | Promise<string>;
    getExistingChunkForPrincipal?: (oid: string, principal: Principal) => Promise<{ refcount: number } | undefined>;
  } = {},
) {
  const auth = {
    db,
    schema: pgSchema,
    api: {
      getSession: async () => (principal?.type === "user" ? { user: { id: principal.userId } } : null),
    },
  } as unknown as CoreAuth;
  if (principal?.type === "device") {
    db.select = () => ({
      from: () => ({
        where: () => ({ limit: async () => [{ deviceId: principal.deviceId }] }),
        innerJoin: async () => db["downloadRows"],
      }),
    });
  }
  const s3 = {
    sign: vi.fn(async (url: string, init?: { method?: string }) => {
      const key = new URL(url).pathname.replace(/^\/bucket\//, "");
      const command = init?.method === "PUT" ? "PutObjectCommand" : "GetObjectCommand";
      return new Request(`signed:${command}:${key}`);
    }),
  } as any;
  const app = createBlobstoreRouter({
    auth,
    s3,
    bucketName: "bucket",
    getQuota: opts.getQuota,
    pastDueGraceDays: opts.pastDueGraceDays,
    chunkKeyForPrincipal: opts.chunkKeyForPrincipal,
    getExistingChunkForPrincipal: opts.getExistingChunkForPrincipal,
  });
  return Object.assign(app, { s3 });
}
