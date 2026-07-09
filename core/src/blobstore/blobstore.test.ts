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
          header: { "Content-Type": "application/octet-stream" },
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
            header: { "Content-Type": "application/octet-stream" },
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

  it("treats active null quota as unlimited", async () => {
    const response = await appFor(new BlobTestDb(), { type: "device", deviceId: "device-1" }, {
      getQuota: async () => ({
        quota_bytes: null,
        usage_bytes: 999_999,
        subscription_status: "active",
        current_period_end: null,
      }),
    }).request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "upload", objects: [{ oid: "new", size: 2 }] }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });

    expect(response.status).toBe(200);
  });

  it("still denies blocked subscriptions with null quota", async () => {
    const response = await appFor(new BlobTestDb(), { type: "device", deviceId: "device-1" }, {
      getQuota: async () => ({
        quota_bytes: null,
        usage_bytes: 0,
        subscription_status: "none",
        current_period_end: null,
      }),
    }).request("/objects/batch", {
      method: "POST",
      body: JSON.stringify({ operation: "upload", objects: [{ oid: "new", size: 2 }] }),
      headers: { "content-type": "application/json", authorization: "Bearer token" },
    });

    expect(response.status).toBe(402);
    await expect(response.json()).resolves.toEqual({ error: "subscription_inactive", status: "none" });
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
      quota_bytes: number | null;
      usage_bytes: number;
      subscription_status: string;
      current_period_end: string | null;
    } | null>;
    pastDueGraceDays?: number;
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
      const key = new URL(url).pathname.split("/").slice(-2).join("/");
      const command = init?.method === "PUT" ? "PutObjectCommand" : "GetObjectCommand";
      return new Request(`signed:${command}:${key}`);
    }),
  } as any;
  return createBlobstoreRouter({ auth, s3, bucketName: "bucket", getQuota: opts.getQuota, pastDueGraceDays: opts.pastDueGraceDays });
}
