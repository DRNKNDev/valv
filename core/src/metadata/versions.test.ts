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
        manifest: [],
        author_device_id: "device-1",
        created_at: "2026-01-02T00:00:00.000Z",
        is_conflict_copy: true,
      },
      {
        version_id: "version-1",
        content_hash: "hash-version-1",
        size_bytes: 100,
        manifest: [],
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
      {
        method: "POST",
        headers: { authorization: "Bearer device-token", "content-type": "application/json" },
        body: JSON.stringify({ based_on_seq: 10 }),
      },
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

  it.each(["rename", "move", "delete", "new_version"])(
    "restores a stale prior version as a conflict copy after a %s op advanced the node",
    async () => {
      const db = new VersionDb({ authorized: true });
      db.nodes.set("doc", { ...db.nodes.get("doc")!, serverSeq: 12 });
      db.versions.push(version("old-version", { manifest: [{ chunk_hash: "chunk-1", offset: 0, length: 8 }] }));
      db.ops.push({ serverSeq: 12, folderId: "folder-1", nodeId: "doc", opType: "new_version" });
      const hub = { notify: vi.fn() };

      const response = await appFor(db, { type: "device", deviceId: "device-1" }, hub).request(
        "/folders/folder-1/nodes/doc/versions/old-version/restore",
        {
          method: "POST",
          headers: { authorization: "Bearer device-token", "content-type": "application/json" },
          body: JSON.stringify({ based_on_seq: 10 }),
        },
      );
      const body = await response.json();

      expect(response.status).toBe(200);
      expect(body).toMatchObject({ result: "conflict_copy", server_seq: 13, node_id: "doc" });
      expect(body.conflict_version_id).toEqual(expect.any(String));
      expect(db.insertedVersions[0]).toMatchObject({
        nodeId: "doc",
        contentHash: "hash-old-version",
        sizeBytes: 100,
        authorDeviceId: "device-1",
        isConflictCopy: true,
        manifest: [{ chunk_hash: "chunk-1", offset: 0, length: 8 }],
      });
      expect(db.nodes.get("doc")?.serverSeq).toBe(13);
      expect(hub.notify).toHaveBeenCalledWith("folder-1", 13);
    },
  );

  it("returns 404 when restoring a missing version", async () => {
    const response = await appFor(new VersionDb({ authorized: true }), { type: "device", deviceId: "device-1" }).request(
      "/folders/folder-1/nodes/doc/versions/missing/restore",
      {
        method: "POST",
        headers: { authorization: "Bearer device-token", "content-type": "application/json" },
        body: JSON.stringify({ based_on_seq: 10 }),
      },
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
  delete: CoreDb["delete"];
  execute: CoreDb["execute"];
  versions: TestVersion[] = [];
  insertedVersions: Array<Omit<TestVersion, "createdAt">> = [];
  ops: Array<{ serverSeq: number; folderId: string; nodeId: string; opType: string }> = [];
  nodes = new Map([
    ["doc", { nodeId: "doc", folderId: "folder-1", parentId: "root", name: "doc.txt", type: "file" as const, serverSeq: 10, currentVersionId: null, deletedAt: null }],
  ]);
  private devicePrincipalId?: string;

  constructor(private readonly opts: { authorized: boolean }) {}

  setDevicePrincipal(deviceId: string): void {
    this.devicePrincipalId = deviceId;
  }

  select(selection?: Record<string, unknown>): any {
    return {
      from: (table: unknown) => this.selectFrom(table, selection),
    };
  }

  insert(table: unknown): any {
    return {
      values: async (value: any) => {
        if (table === pgSchema.versions) {
          this.insertedVersions.push(value);
          this.versions.push({ ...value, createdAt: new Date("2026-01-03T00:00:00.000Z") });
        }
        if (table === pgSchema.opLog) {
          const serverSeq = this.nextServerSeq();
          this.ops.push({ serverSeq, folderId: value.folderId, nodeId: value.nodeId, opType: value.opType });
        }
      },
    };
  }

  update(table: unknown): any {
    return {
      set: (patch: any) => ({
        where: async () => {
          if (table !== pgSchema.nodes) {
            return;
          }
          const node = this.nodes.get("doc");
          if (node) {
            this.nodes.set("doc", { ...node, ...patch });
          }
        },
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

  private selectFrom(table: unknown, selection?: Record<string, unknown>): any {
    const rows = () => this.selectRows(table, selection);
    return {
      where: () => ({
        limit: async (limit: number) => rows().slice(0, limit),
        orderBy: () => this.orderable(rows()),
      }),
      orderBy: () => this.orderable(rows()),
      innerJoin: (joinTable: unknown) => ({
        where: () => ({
          limit: async (limit: number) => this.innerJoinRows(table, joinTable, selection).slice(0, limit),
        }),
      }),
    };
  }

  private orderable(rows: any[]): any {
    return {
      limit: async (limit: number) => rows.slice(0, limit),
      then: (resolve: (value: any[]) => unknown, reject?: (reason: unknown) => unknown) => Promise.resolve(rows).then(resolve, reject),
    };
  }

  private selectRows(table: unknown, selection?: Record<string, unknown>): any[] {
    if (table === pgSchema.devices) {
      return this.devicePrincipalId ? [{ deviceId: this.devicePrincipalId }] : [];
    }
    if (table === pgSchema.nodes) {
      const node = this.nodes.get("doc");
      if (!node) {
        return [];
      }
      if (selection?.serverSeq) {
        return [{ serverSeq: node.serverSeq }];
      }
      return [node];
    }
    if (table === pgSchema.opLog) {
      return [...this.ops].sort((left, right) => right.serverSeq - left.serverSeq);
    }
    return [...this.versions].sort((left, right) => right.createdAt.getTime() - left.createdAt.getTime());
  }

  private innerJoinRows(table: unknown, joinTable: unknown, selection?: Record<string, unknown>): any[] {
    if (table !== pgSchema.nodes || joinTable !== pgSchema.versions || !selection?.manifest) {
      return [];
    }
    const currentVersionId = this.nodes.get("doc")?.currentVersionId;
    const version = this.versions.find((item) => item.versionId === currentVersionId);
    return version ? [{ manifest: version.manifest }] : [];
  }

  private nextServerSeq(): number {
    return Math.max(10, ...this.ops.map((op) => op.serverSeq), ...[...this.nodes.values()].map((node) => node.serverSeq)) + 1;
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
