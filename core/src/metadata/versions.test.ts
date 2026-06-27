import { describe, expect, it, vi } from "vitest";

import type { CoreAuth, CoreDb, Principal } from "../auth/index.js";
import { pgSchema } from "../db/schema.js";
import { createMetadataRouter } from "./index.js";

describe("version routes", () => {
  it("lists node versions newest first", async () => {
    const db = new VersionDb({ authorized: true });
    db.versions.push(
      version("version-1", { createdAt: new Date("2026-01-01T00:00:00.000Z") }),
      version("version-2", { createdAt: new Date("2026-01-02T00:00:00.000Z"), isConflictCopy: true }),
    );

    const response = await appFor(db, { type: "user", userId: "user-1" }).request("/folders/folder-1/nodes/doc/versions");

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toEqual([
      {
        version_id: "version-2",
        content_hash: "hash-version-2",
        size_bytes: 100,
        author_device_id: "device-1",
        created_at: "2026-01-02T00:00:00.000Z",
        is_conflict_copy: true,
      },
      {
        version_id: "version-1",
        content_hash: "hash-version-1",
        size_bytes: 100,
        author_device_id: "device-1",
        created_at: "2026-01-01T00:00:00.000Z",
        is_conflict_copy: false,
      },
    ]);
  });

  it("rejects version listing without read authorization", async () => {
    const response = await appFor(new VersionDb({ authorized: false }), { type: "user", userId: "user-1" }).request(
      "/folders/folder-1/nodes/doc/versions",
    );

    expect(response.status).toBe(403);
    await expect(response.json()).resolves.toEqual({ error: "no_grant" });
  });

  it("restores a prior version by submitting a new version op", async () => {
    const db = new VersionDb({ authorized: true });
    db.versions.push(version("old-version", { manifest: [{ chunk_hash: "chunk-1", offset: 0, length: 8 }] }));
    const hub = { notify: vi.fn() };

    const response = await appFor(db, { type: "device", deviceId: "device-1" }, hub).request(
      "/folders/folder-1/nodes/doc/versions/old-version/restore",
      { method: "POST", headers: { authorization: "Bearer device-token" } },
    );
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body).toMatchObject({ result: "applied", server_seq: 11, node_id: "doc" });
    expect(db.insertedVersions[0]).toMatchObject({
      nodeId: "doc",
      contentHash: "hash-old-version",
      sizeBytes: 100,
      authorDeviceId: "device-1",
      isConflictCopy: false,
      manifest: [{ chunk_hash: "chunk-1", offset: 0, length: 8 }],
    });
    expect(db.nodes.get("doc")?.currentVersionId).toBe(db.insertedVersions[0]?.versionId);
    expect(hub.notify).toHaveBeenCalledWith("folder-1", 11);
  });

  it("returns 404 when restoring a missing version", async () => {
    const response = await appFor(new VersionDb({ authorized: true }), { type: "device", deviceId: "device-1" }).request(
      "/folders/folder-1/nodes/doc/versions/missing/restore",
      { method: "POST", headers: { authorization: "Bearer device-token" } },
    );

    expect(response.status).toBe(404);
    await expect(response.json()).resolves.toEqual({ error: "version_not_found" });
  });
});

type ChunkRef = { chunk_hash: string; offset: number; length: number };
type TestVersion = {
  versionId: string;
  nodeId: string;
  manifest: ChunkRef[];
  contentHash: string;
  sizeBytes: number;
  authorDeviceId: string;
  createdAt: Date;
  isConflictCopy: boolean;
};

class VersionDb implements CoreDb {
  insert: CoreDb["insert"];
  update: CoreDb["update"];
  delete: CoreDb["delete"];
  execute: CoreDb["execute"];
  versions: TestVersion[] = [];
  insertedVersions: Array<Omit<TestVersion, "createdAt">> = [];
  nodes = new Map([
    ["doc", { nodeId: "doc", folderId: "folder-1", parentId: "root", name: "doc.txt", type: "file" as const, serverSeq: 10, currentVersionId: null, deletedAt: null }],
  ]);
  private devicePrincipalId?: string;

  constructor(private readonly opts: { authorized: boolean }) {}

  setDevicePrincipal(deviceId: string): void {
    this.devicePrincipalId = deviceId;
  }

  select(): any {
    return {
      from: (table: unknown) => ({
        where: () => ({
          limit: async (limit: number) => this.selectRows(table).slice(0, limit),
          orderBy: async () => this.selectRows(table),
        }),
        orderBy: async () => this.selectRows(table),
      }),
    };
  }

  async getNodeForAuthz(nodeId: string) {
    const node = this.nodes.get(nodeId);
    return node ? { nodeId: node.nodeId, folderId: node.folderId, parentId: node.parentId } : undefined;
  }

  async getGrantForAuthz(): Promise<{ grantId: string; scopeNodeId: string; canRead: boolean; canWrite: boolean } | undefined> {
    return this.opts.authorized ? { grantId: "grant-1", scopeNodeId: "doc", canRead: true, canWrite: true } : undefined;
  }

  async getNodeForOp(nodeId: string) {
    return this.nodes.get(nodeId);
  }

  async findLiveChildForOp(): Promise<undefined> {
    return undefined;
  }

  async insertNodeForOp(): Promise<void> {}

  async updateNodeForOp(nodeId: string, patch: any): Promise<void> {
    const node = this.nodes.get(nodeId);
    if (node) {
      this.nodes.set(nodeId, { ...node, ...patch });
    }
  }

  async insertVersionForOp(versionRow: Omit<TestVersion, "createdAt">): Promise<void> {
    this.insertedVersions.push(versionRow);
  }

  async incrementChunksForOp(): Promise<void> {}

  async insertOpForOp(): Promise<number> {
    return 11;
  }

  private selectRows(table: unknown): any[] {
    if (table === pgSchema.devices) {
      return this.devicePrincipalId ? [{ deviceId: this.devicePrincipalId }] : [];
    }
    if (table === pgSchema.nodes) {
      return [{ serverSeq: this.nodes.get("doc")?.serverSeq ?? 0 }];
    }
    return [...this.versions].sort((left, right) => right.createdAt.getTime() - left.createdAt.getTime());
  }
}

function appFor(db: VersionDb, principal: Principal, hub = { notify: () => undefined }) {
  if (principal.type === "device") {
    db.setDevicePrincipal(principal.deviceId);
  }
  const auth = {
    db,
    schema: pgSchema,
    api: {
      getSession: async () => (principal.type === "user" ? { user: { id: principal.userId } } : null),
    },
  } as unknown as CoreAuth;
  return createMetadataRouter({ auth, hub });
}

function version(versionId: string, opts: Partial<TestVersion> = {}): TestVersion {
  return {
    versionId,
    nodeId: "doc",
    manifest: [],
    contentHash: `hash-${versionId}`,
    sizeBytes: 100,
    authorDeviceId: "device-1",
    createdAt: new Date("2026-01-01T00:00:00.000Z"),
    isConflictCopy: false,
    ...opts,
  };
}
