import { describe, expect, it } from "vitest";

import type { CoreAuth, CoreDb, Principal } from "../auth/index.js";
import { pgSchema } from "../db/schema.js";
import type { MetadataHub } from "./common.js";
import { submitOp } from "./ops.js";

describe("submitOp", () => {
  it("applies a matching metadata op", async () => {
    const db = new OpTestDb();
    const hub = new TestHub();

    const response = await submitOp(authFor(db), hub, "folder-1", devicePrincipal, {
      op_type: "rename",
      node_id: "doc",
      based_on_seq: 1,
      payload: { new_name: "renamed.md" },
    });

    expect(response).toEqual({ result: "applied", server_seq: 2, node_id: "doc" });
    expect(db.nodes.get("doc")?.name).toBe("renamed.md");
    expect(db.nodes.get("doc")?.serverSeq).toBe(2);
    expect(db.ops).toHaveLength(1);
    expect(hub.notifications).toEqual([{ folderId: "folder-1", serverSeq: 2 }]);
  });

  it("returns superseded for stale metadata ops", async () => {
    const db = new OpTestDb();
    const hub = new TestHub();

    const response = await submitOp(authFor(db), hub, "folder-1", devicePrincipal, {
      op_type: "rename",
      node_id: "doc",
      based_on_seq: 0,
      payload: { new_name: "stale.md" },
    });

    expect(response).toEqual({ result: "superseded", current_seq: 1 });
    expect(db.nodes.get("doc")?.name).toBe("doc.md");
    expect(db.ops).toHaveLength(0);
    expect(hub.notifications).toEqual([]);
  });

  it("applies move and delete metadata branches", async () => {
    const db = new OpTestDb();
    const hub = new TestHub();

    await submitOp(authFor(db), hub, "folder-1", devicePrincipal, {
      op_type: "move",
      node_id: "doc",
      based_on_seq: 1,
      payload: { new_parent_id: "archive" },
    });
    await submitOp(authFor(db), hub, "folder-1", devicePrincipal, {
      op_type: "delete",
      node_id: "doc",
      based_on_seq: 2,
      payload: {},
    });

    expect(db.nodes.get("doc")?.parentId).toBe("archive");
    expect(db.nodes.get("doc")?.deletedAt).toBeInstanceOf(Date);
    expect(db.ops.map((op) => op.opType)).toEqual(["move", "delete"]);
    expect(hub.notifications).toEqual([
      { folderId: "folder-1", serverSeq: 2 },
      { folderId: "folder-1", serverSeq: 3 },
    ]);
  });

  it("applies canonical new_version ops by updating current version and chunk refcounts", async () => {
    const db = new OpTestDb();
    const hub = new TestHub();

    const response = await submitOp(authFor(db), hub, "folder-1", devicePrincipal, {
      op_type: "new_version",
      node_id: "doc",
      based_on_seq: 1,
      payload: {
        version_id: "version-1",
        content_hash: "hash-1",
        size_bytes: 22,
        manifest: [
          { chunk_hash: "chunk-1", offset: 0, length: 10 },
          { chunk_hash: "chunk-2", offset: 10, length: 12 },
        ],
      },
    });

    expect(response).toEqual({ result: "applied", server_seq: 2, node_id: "doc" });
    expect(db.versions).toEqual([
      expect.objectContaining({ versionId: "version-1", isConflictCopy: false, contentHash: "hash-1" }),
    ]);
    expect(db.nodes.get("doc")?.currentVersionId).toBe("version-1");
    expect(db.chunkRefcounts.get("chunk-1")).toBe(1);
    expect(db.chunkRefcounts.get("chunk-2")).toBe(1);
    expect(db.versionChunks).toEqual([
      { versionId: "version-1", nodeId: "doc", chunkHash: "chunk-1" },
      { versionId: "version-1", nodeId: "doc", chunkHash: "chunk-2" },
    ]);
    expect(hub.notifications).toEqual([{ folderId: "folder-1", serverSeq: 2 }]);
  });

  it("decrements previous chunk refcounts when a new_version supersedes the current version", async () => {
    const db = new OpTestDb();
    const hub = new TestHub();
    db.nodes.set("doc", { ...db.nodes.get("doc")!, currentVersionId: "old-version" });
    db.versions.push({
      versionId: "old-version",
      nodeId: "doc",
      manifest: [{ chunk_hash: "old-chunk", offset: 0, length: 10 }],
      contentHash: "old-hash",
      sizeBytes: 10,
      authorDeviceId: "device-1",
      isConflictCopy: false,
    });
    db.chunkRefcounts.set("old-chunk", 1);

    const response = await submitOp(authFor(db), hub, "folder-1", devicePrincipal, {
      op_type: "new_version",
      node_id: "doc",
      based_on_seq: 1,
      payload: {
        version_id: "new-version",
        content_hash: "new-hash",
        size_bytes: 12,
        manifest: [{ chunk_hash: "new-chunk", offset: 0, length: 12 }],
      },
    });

    expect(response).toEqual({ result: "applied", server_seq: 2, node_id: "doc" });
    expect(db.nodes.get("doc")?.currentVersionId).toBe("new-version");
    expect(db.chunkRefcounts.get("new-chunk")).toBe(1);
    expect(db.chunkRefcounts.get("old-chunk")).toBe(0);
  });

  it("creates a conflict copy for stale new_version ops", async () => {
    const db = new OpTestDb();
    const hub = new TestHub();

    const response = await submitOp(authFor(db), hub, "folder-1", devicePrincipal, {
      op_type: "new_version",
      node_id: "doc",
      based_on_seq: 0,
      payload: {
        version_id: "client-version",
        content_hash: "hash-1",
        size_bytes: 10,
        manifest: [{ chunk_hash: "chunk-1", offset: 0, length: 10 }],
      },
    });

    expect(response.result).toBe("conflict_copy");
    if (response.result !== "conflict_copy") {
      throw new Error("expected conflict_copy response");
    }
    expect(response.node_id).toBe("doc");
    expect(db.versions).toHaveLength(1);
    expect(db.versions[0]?.isConflictCopy).toBe(true);
    expect(db.versions[0]?.versionId).not.toBe("client-version");
    expect(db.chunkRefcounts.get("chunk-1")).toBe(1);
    expect(db.versionChunks).toEqual([
      { versionId: db.versions[0]?.versionId, nodeId: "doc", chunkHash: "chunk-1" },
    ]);
    expect(db.nodes.get("doc")?.currentVersionId).toBeNull();
    expect(db.nodes.get("doc")?.serverSeq).toBe(response.server_seq);
    expect(hub.notifications).toEqual([{ folderId: "folder-1", serverSeq: 2 }]);
  });

  it("returns superseded for duplicate create ops", async () => {
    const db = new OpTestDb();
    const hub = new TestHub();

    const response = await submitOp(authFor(db), hub, "folder-1", devicePrincipal, {
      op_type: "create",
      payload: { node_id: "duplicate", parent_id: "root", name: "doc.md", type: "file" },
    });

    expect(response).toEqual({ result: "superseded", current_seq: 1 });
    expect(db.ops).toHaveLength(0);
    expect(hub.notifications).toEqual([]);
  });
});

const devicePrincipal: Principal = { type: "device", deviceId: "device-1" };

type TestNode = {
  nodeId: string;
  folderId: string;
  parentId: string | null;
  name: string;
  type: "file" | "folder";
  serverSeq: number;
  currentVersionId: string | null;
  deletedAt: Date | null;
};

type TestVersion = {
  versionId: string;
  nodeId: string;
  manifest: Array<{ chunk_hash: string; offset: number; length: number }>;
  contentHash: string;
  sizeBytes: number;
  authorDeviceId: string;
  isConflictCopy: boolean;
};

type TestVersionChunk = {
  versionId: string;
  nodeId: string;
  chunkHash: string;
};

class OpTestDb implements CoreDb {
  delete: CoreDb["delete"];
  nodes = new Map<string, TestNode>([
    [
      "root",
      {
        nodeId: "root",
        folderId: "folder-1",
        parentId: null,
        name: "",
        type: "folder",
        serverSeq: 1,
        currentVersionId: null,
        deletedAt: null,
      },
    ],
    [
      "archive",
      {
        nodeId: "archive",
        folderId: "folder-1",
        parentId: "root",
        name: "archive",
        type: "folder",
        serverSeq: 1,
        currentVersionId: null,
        deletedAt: null,
      },
    ],
    [
      "doc",
      {
        nodeId: "doc",
        folderId: "folder-1",
        parentId: "root",
        name: "doc.md",
        type: "file",
        serverSeq: 1,
        currentVersionId: null,
        deletedAt: null,
      },
    ],
  ]);
  versions: TestVersion[] = [];
  versionChunks: TestVersionChunk[] = [];
  ops: Array<{ serverSeq: number; folderId: string; nodeId: string; opType: string; opPayload: unknown }> = [];
  chunkRefcounts = new Map<string, number>();
  private lastInsertedVersionManifest: TestVersion["manifest"] = [];
  private previousVersionManifest: TestVersion["manifest"] = [];
  private chunkUpdatePhase: "idle" | "increment" | "decrement" = "idle";

  select(selection?: Record<string, unknown>): any {
    return {
      from: (table: unknown) => this.selectFrom(table, selection),
    };
  }

  insert(table: unknown): any {
    return {
      values: async (value: any) => {
        if (table === pgSchema.nodes) {
          const duplicate = [...this.nodes.values()].find(
            (node) =>
              node.folderId === value.folderId &&
              node.parentId === value.parentId &&
              node.name === value.name &&
              node.deletedAt === null,
          );
          if (duplicate) {
            throw new Error("duplicate node");
          }
          this.nodes.set(value.nodeId, { currentVersionId: null, deletedAt: null, ...value });
        }
        if (table === pgSchema.versions) {
          const version = value as TestVersion;
          this.versions.push(version);
          this.lastInsertedVersionManifest = version.manifest;
          this.chunkUpdatePhase = "increment";
        }
        if (table === pgSchema.versionChunks) {
          this.versionChunks.push(...(Array.isArray(value) ? value : [value]));
        }
        if (table === pgSchema.opLog) {
          const serverSeq = this.nextServerSeq();
          this.ops.push({
            serverSeq,
            folderId: value.folderId,
            nodeId: value.nodeId,
            opType: value.opType,
            opPayload: value.opPayload,
          });
        }
      },
    };
  }

  update(table: unknown): any {
    return {
      set: (patch: any) => ({
        where: async () => {
          if (table === pgSchema.nodes) {
            const node = this.nodes.get("doc");
            if (!node) {
              throw new Error("missing node doc");
            }
            this.nodes.set("doc", { ...node, ...patch });
          }
          if (table === pgSchema.chunks) {
            this.applyChunkUpdate();
          }
        },
      }),
    };
  }

  async getNodeForAuthz(nodeId: string): Promise<{ nodeId: string; folderId: string; parentId: string | null } | undefined> {
    return this.nodes.get(nodeId);
  }

  async getGrantForAuthz(opts: {
    folderId: string;
    scopeNodeId: string;
    principal: Principal;
  }): Promise<{ grantId: string; scopeNodeId: string; canRead: boolean; canWrite: boolean } | undefined> {
    if (opts.folderId === "folder-1" && opts.scopeNodeId === "root" && opts.principal.type === "device") {
      return { grantId: "grant-1", scopeNodeId: "root", canRead: true, canWrite: true };
    }
    return undefined;
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
    const keys = Object.keys(selection ?? {});
    if (table === pgSchema.nodes) {
      const doc = this.nodes.get("doc");
      if (keys.includes("type")) {
        return [this.nodes.get("archive")].filter(Boolean);
      }
      if (keys.includes("name") && keys.includes("parentId")) {
        return doc ? [{ name: doc.name, parentId: doc.parentId }] : [];
      }
      if (keys.includes("nodeId") && keys.includes("serverSeq")) {
        return doc ? [{ nodeId: doc.nodeId, serverSeq: doc.serverSeq }] : [];
      }
      if (keys.includes("nodeId")) {
        return [];
      }
      if (keys.includes("serverSeq")) {
        return doc ? [{ serverSeq: doc.serverSeq }] : [];
      }
      return [...this.nodes.values()];
    }
    if (table === pgSchema.opLog) {
      return [...this.ops].sort((left, right) => right.serverSeq - left.serverSeq);
    }
    if (table === pgSchema.versions) {
      return [...this.versions];
    }
    return [];
  }

  private innerJoinRows(table: unknown, joinTable: unknown, selection?: Record<string, unknown>): any[] {
    if (table !== pgSchema.nodes || joinTable !== pgSchema.versions || !selection?.manifest) {
      return [];
    }
    const currentVersionId = this.nodes.get("doc")?.currentVersionId;
    const version = this.versions.find((item) => item.versionId === currentVersionId);
    this.previousVersionManifest = version?.manifest ?? [];
    return version ? [{ manifest: version.manifest }] : [];
  }

  private applyChunkUpdate(): void {
    if (this.chunkUpdatePhase === "increment") {
      for (const chunk of this.lastInsertedVersionManifest) {
        this.chunkRefcounts.set(chunk.chunk_hash, (this.chunkRefcounts.get(chunk.chunk_hash) ?? 0) + 1);
      }
      this.chunkUpdatePhase = this.previousVersionManifest.length > 0 ? "decrement" : "idle";
      return;
    }
    if (this.chunkUpdatePhase === "decrement") {
      for (const chunk of this.previousVersionManifest) {
        this.chunkRefcounts.set(chunk.chunk_hash, Math.max((this.chunkRefcounts.get(chunk.chunk_hash) ?? 0) - 1, 0));
      }
      this.previousVersionManifest = [];
      this.chunkUpdatePhase = "idle";
    }
  }

  private nextServerSeq(): number {
    return Math.max(1, ...this.ops.map((op) => op.serverSeq), ...[...this.nodes.values()].map((node) => node.serverSeq)) + 1;
  }
}

class TestHub implements MetadataHub {
  notifications: Array<{ folderId: string; serverSeq: number }> = [];

  notify(folderId: string, serverSeq: number): void {
    this.notifications.push({ folderId, serverSeq });
  }
}

function authFor(db: OpTestDb): CoreAuth {
  return { db, schema: pgSchema } as unknown as CoreAuth;
}
