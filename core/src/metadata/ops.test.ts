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
        size_bytes: 10,
        manifest: [{ chunk_hash: "chunk-1", offset: 0, length: 10 }],
      },
    });

    expect(response).toEqual({ result: "applied", server_seq: 2, node_id: "doc" });
    expect(db.versions).toEqual([
      expect.objectContaining({ versionId: "version-1", isConflictCopy: false, contentHash: "hash-1" }),
    ]);
    expect(db.nodes.get("doc")?.currentVersionId).toBe("version-1");
    expect(db.chunkRefcounts.get("chunk-1")).toBe(1);
    expect(hub.notifications).toEqual([{ folderId: "folder-1", serverSeq: 2 }]);
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

class OpTestDb implements CoreDb {
  select: CoreDb["select"];
  insert: CoreDb["insert"];
  update: CoreDb["update"];
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
  ops: Array<{ serverSeq: number; folderId: string; nodeId: string; opType: string }> = [];
  chunkRefcounts = new Map<string, number>();

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

  async getNodeForOp(nodeId: string): Promise<TestNode | undefined> {
    return this.nodes.get(nodeId);
  }

  async findLiveChildForOp(opts: { folderId: string; parentId: string; name: string }): Promise<TestNode | undefined> {
    return [...this.nodes.values()].find(
      (node) =>
        node.folderId === opts.folderId &&
        node.parentId === opts.parentId &&
        node.name === opts.name &&
        node.deletedAt === null,
    );
  }

  async insertNodeForOp(node: TestNode): Promise<void> {
    this.nodes.set(node.nodeId, node);
  }

  async updateNodeForOp(nodeId: string, patch: Partial<TestNode>): Promise<void> {
    const node = this.nodes.get(nodeId);
    if (!node) {
      throw new Error(`missing node ${nodeId}`);
    }
    this.nodes.set(nodeId, { ...node, ...patch });
  }

  async insertVersionForOp(version: TestVersion): Promise<void> {
    this.versions.push(version);
  }

  async incrementChunksForOp(chunkHashes: string[]): Promise<void> {
    for (const hash of chunkHashes) {
      this.chunkRefcounts.set(hash, (this.chunkRefcounts.get(hash) ?? 0) + 1);
    }
  }

  async insertOpForOp(op: { folderId: string; nodeId: string; opType: string }): Promise<number> {
    const serverSeq = this.ops.length + 2;
    this.ops.push({ serverSeq, folderId: op.folderId, nodeId: op.nodeId, opType: op.opType });
    return serverSeq;
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
