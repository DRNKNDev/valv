import { describe, expect, it } from "vitest";

import type { CoreAuth, CoreDb, Principal } from "../auth/index.js";
import { pgSchema } from "../db/schema.js";
import { pullDelta, pullTree } from "./delta.js";

describe("delta pull scope filtering", () => {
  it("returns only in-scope ops for a partial-scope device and excludes siblings", async () => {
    const db = new DeltaTestDb();

    const response = await pullDelta(authFor(db), "folder-1", devicePrincipal, 0);

    expect(response.ops.map((op) => op.node_id)).toEqual(["work", "work-doc"]);
    expect(response.ops.map((op) => op.node_id)).not.toContain("personal-doc");
    expect(response.up_to_seq).toBe(2);
  });

  it("parses sqlite JSON string op payloads before returning delta ops", async () => {
    const db = new DeltaTestDb();
    db.ops[0].op_payload = JSON.stringify({ parent_id: "root", name: "docs", type: "folder" });

    const response = await pullDelta(authFor(db), "folder-1", devicePrincipal, 0);

    expect(response.ops[0].op_payload).toEqual({ parent_id: "root", name: "docs", type: "folder" });
  });

  it("includes tombstoned nodes in tree and masks the scope root parent", async () => {
    const db = new DeltaTestDb();

    const response = await pullTree(authFor(db), "folder-1", devicePrincipal);

    expect(response.nodes.map((node) => node.node_id)).toEqual(["work", "work-doc", "deleted-doc"]);
    expect(response.nodes.find((node) => node.node_id === "work")?.parent_id).toBeNull();
    expect(response.nodes.find((node) => node.node_id === "deleted-doc")?.deleted_at).toBe("2026-01-01T00:00:00.000Z");
    expect(response.up_to_seq).toBe(4);
  });
});

const devicePrincipal: Principal = { type: "device", deviceId: "device-1" };

type TestNode = {
  node_id: string;
  parent_id: string | null;
  folder_id: string;
  name: string;
  type: "file" | "folder";
  current_version_id: string | null;
  server_seq: number;
  deleted_at: Date | null;
};

type TestOp = {
  server_seq: number;
  folder_id: string;
  node_id: string;
  op_type: string;
  op_payload: Record<string, unknown> | string;
  actor_device_id: string;
  applied_at: Date;
};

class DeltaTestDb implements CoreDb {
  select: CoreDb["select"];
  insert: CoreDb["insert"];
  update: CoreDb["update"];
  delete: CoreDb["delete"];

  nodes = new Map<string, TestNode>([
    ["root", node("root", null, "", "folder", 0)],
    ["work", node("work", "root", "work", "folder", 1)],
    ["work-doc", node("work-doc", "work", "work.md", "file", 2)],
    ["deleted-doc", node("deleted-doc", "work", "old.md", "file", 4, new Date("2026-01-01T00:00:00.000Z"))],
    ["personal", node("personal", "root", "personal", "folder", 1)],
    ["personal-doc", node("personal-doc", "personal", "secret.md", "file", 3)],
  ]);

  ops: TestOp[] = [
    op(1, "work"),
    op(2, "work-doc"),
    op(3, "personal-doc"),
  ];

  async getFolderRootForAuthz(folderId: string): Promise<string | undefined> {
    return [...this.nodes.values()].find((item) => item.folder_id === folderId && item.parent_id === null)?.node_id;
  }

  async getNodeForAuthz(nodeId: string): Promise<{ nodeId: string; folderId: string; parentId: string | null } | undefined> {
    const item = this.nodes.get(nodeId);
    return item ? { nodeId: item.node_id, folderId: item.folder_id, parentId: item.parent_id } : undefined;
  }

  async getGrantForAuthz(opts: {
    folderId: string;
    scopeNodeId: string;
    principal: Principal;
  }): Promise<{ grantId: string; scopeNodeId: string; canRead: boolean; canWrite: boolean } | undefined> {
    if (opts.folderId === "folder-1" && opts.scopeNodeId === "root" && opts.principal.type === "device") {
      return { grantId: "grant-work", scopeNodeId: "work", canRead: true, canWrite: false };
    }
    return undefined;
  }

  async getDeltaOpsForScope(opts: { folderId: string; scopeNodeId: string; since: number; limit: number }): Promise<TestOp[]> {
    const subtree = this.subtree(opts.scopeNodeId, false);
    return this.ops
      .filter((item) => item.folder_id === opts.folderId && item.server_seq > opts.since && subtree.has(item.node_id))
      .sort((a, b) => a.server_seq - b.server_seq)
      .slice(0, opts.limit);
  }

  async getTreeNodesForScope(opts: { folderId: string; scopeNodeId: string }): Promise<TestNode[]> {
    const subtree = this.subtree(opts.scopeNodeId, true);
    return [...this.nodes.values()]
      .filter((item) => item.folder_id === opts.folderId && subtree.has(item.node_id))
      .sort((a, b) => a.server_seq - b.server_seq);
  }

  async getFolderHeadSeqForDelta(folderId: string): Promise<number> {
    return Math.max(
      0,
      ...this.ops.filter((item) => item.folder_id === folderId).map((item) => item.server_seq),
      ...[...this.nodes.values()].filter((item) => item.folder_id === folderId).map((item) => item.server_seq),
    );
  }

  private subtree(scopeNodeId: string, includeTombstoned: boolean): Set<string> {
    const ids = new Set<string>([scopeNodeId]);
    let changed = true;
    while (changed) {
      changed = false;
      for (const item of this.nodes.values()) {
        if (ids.has(item.node_id) || !item.parent_id || !ids.has(item.parent_id)) {
          continue;
        }
        if (!includeTombstoned && item.deleted_at) {
          continue;
        }
        ids.add(item.node_id);
        changed = true;
      }
    }
    return ids;
  }
}

function node(
  node_id: string,
  parent_id: string | null,
  name: string,
  type: "file" | "folder",
  server_seq: number,
  deleted_at: Date | null = null,
): TestNode {
  return {
    node_id,
    parent_id,
    folder_id: "folder-1",
    name,
    type,
    current_version_id: null,
    server_seq,
    deleted_at,
  };
}

function op(server_seq: number, node_id: string): TestOp {
  return {
    server_seq,
    folder_id: "folder-1",
    node_id,
    op_type: "rename",
    op_payload: { new_name: `${node_id}.txt` },
    actor_device_id: "device-1",
    applied_at: new Date("2026-01-01T00:00:00.000Z"),
  };
}

function authFor(db: DeltaTestDb): CoreAuth {
  return { db, schema: pgSchema } as unknown as CoreAuth;
}
