import { describe, expect, it, vi } from "vitest";

import type { CoreAuth, CoreDb, Principal } from "../auth/index.js";
import { pgSchema } from "../db/schema.js";
import { chunkKey, createBlobstoreRouter } from "./index.js";

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
  private selectCalls = 0;

  constructor(opts: { existingChunks?: Array<{ chunkHash: string; refcount: number }>; downloadRows?: Array<{ nodeId: string; manifest: Array<{ chunk_hash: string }> }>; authorized?: boolean } = {}) {
    this.existingChunks = opts.existingChunks ?? [];
    this.downloadRows = opts.downloadRows ?? [];
    this.authorized = opts.authorized ?? true;
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
      values: async (value: { chunkHash: string; sizeBytes: number; refcount: number }) => {
        this.insertedChunks.push(value);
        this.existingChunks.push({ chunkHash: value.chunkHash, refcount: value.refcount });
      },
    };
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

function appFor(db: BlobTestDb, principal: Principal | null = { type: "user", userId: "user-1" }) {
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
  return createBlobstoreRouter({ auth, s3, bucketName: "bucket" });
}
