import { readdirSync, readFileSync } from "node:fs";

import Database from "better-sqlite3";
import { drizzle } from "drizzle-orm/better-sqlite3";
import { describe, expect, it } from "vitest";

import type { CoreAuth, CoreDb, Principal } from "../auth/index.js";
import { pgSchema, sqliteSchema } from "../db/schema.js";
import {
  canDownloadChunkScopedForParity,
  canDownloadChunkUnscopedForParity,
} from "./index.js";

type ParityCase = {
  principal: Principal;
  oid: string;
  expected: boolean;
};

const parityCases: ParityCase[] = [
  { principal: { type: "user", userId: "user-1" }, oid: "shared-chunk", expected: true },
  { principal: { type: "user", userId: "user-1" }, oid: "cross-tenant-shared", expected: true },
  { principal: { type: "user", userId: "user-1" }, oid: "conflict-only", expected: true },
  { principal: { type: "user", userId: "user-1" }, oid: "secret-only", expected: false },
  { principal: { type: "user", userId: "user-1" }, oid: "missing", expected: false },
  { principal: { type: "user", userId: "user-2" }, oid: "shared-chunk", expected: false },
  { principal: { type: "user", userId: "user-2" }, oid: "cross-tenant-shared", expected: false },
  { principal: { type: "user", userId: "user-2" }, oid: "conflict-only", expected: false },
  { principal: { type: "user", userId: "user-2" }, oid: "secret-only", expected: false },
];

describe("chunk authorization parity", () => {
  it("matches old and scoped outcomes on a pgSchema-shaped CoreDb double", async () => {
    const db = new ChunkAuthParityDb();
    const auth = { db, schema: pgSchema } as unknown as CoreAuth;

    await expectParity(auth);
  });

  it("matches old and scoped outcomes on a real SQLite connection", async () => {
    const sqlite = new Database(":memory:");
    try {
      applySqliteMigrations(sqlite);
      sqlite.exec("PRAGMA foreign_keys = ON");
      seedSqliteParityFixture(sqlite);
      const db = Object.assign(drizzle(sqlite, { schema: sqliteSchema }), { __valvSqlite: true }) as CoreDb;
      const auth = { db, schema: sqliteSchema } as unknown as CoreAuth;

      await expectParity(auth);
    } finally {
      sqlite.close();
    }
  });
});

async function expectParity(auth: CoreAuth): Promise<void> {
  for (const testCase of parityCases) {
    const oldOutcome = await canDownloadChunkUnscopedForParity(auth, testCase.principal, testCase.oid);
    const scopedOutcome = await canDownloadChunkScopedForParity(auth, testCase.principal, testCase.oid);

    expect({ oid: testCase.oid, principal: testCase.principal, oldOutcome, scopedOutcome }).toEqual({
      oid: testCase.oid,
      principal: testCase.principal,
      oldOutcome: testCase.expected,
      scopedOutcome: testCase.expected,
    });
  }
}

class ChunkAuthParityDb implements CoreDb {
  insert: CoreDb["insert"];
  update: CoreDb["update"];
  delete: CoreDb["delete"];
  all: CoreDb["all"];
  nodes = [
    { nodeId: "root-a", folderId: "folder-a", parentId: null },
    { nodeId: "doc-a", folderId: "folder-a", parentId: "root-a" },
    { nodeId: "root-b", folderId: "folder-b", parentId: null },
    { nodeId: "secret-b", folderId: "folder-b", parentId: "root-b" },
  ];
  versions = [
    {
      versionId: "version-current",
      nodeId: "doc-a",
      manifest: [{ chunk_hash: "shared-chunk" }, { chunk_hash: "cross-tenant-shared" }],
    },
    {
      versionId: "version-conflict",
      nodeId: "doc-a",
      manifest: [{ chunk_hash: "shared-chunk" }, { chunk_hash: "conflict-only" }],
    },
    {
      versionId: "version-secret",
      nodeId: "secret-b",
      manifest: [{ chunk_hash: "secret-only" }, { chunk_hash: "cross-tenant-shared" }],
    },
  ];
  versionChunks = this.versions.flatMap((version) =>
    version.manifest.map((chunk) => ({
      versionId: version.versionId,
      nodeId: version.nodeId,
      chunkHash: chunk.chunk_hash,
    })),
  );

  select(): any {
    return {
      from: (table: unknown) => ({
        innerJoin: async () => {
          if (table !== pgSchema.nodes) {
            return [];
          }
          return this.versions.map((version) => ({ nodeId: version.nodeId, manifest: version.manifest }));
        },
      }),
    };
  }

  async execute(query: any): Promise<Array<{ node_id: string }>> {
    const chunkHash = query.queryChunks?.find((chunk: unknown) => typeof chunk === "string");
    const nodeIds = new Set(
      this.versionChunks
        .filter((row) => row.chunkHash === chunkHash)
        .map((row) => row.nodeId),
    );
    return [...nodeIds].map((nodeId) => ({ node_id: nodeId }));
  }

  async getNodeForAuthz(nodeId: string): Promise<{ nodeId: string; folderId: string; parentId: string | null } | undefined> {
    return this.nodes.find((node) => node.nodeId === nodeId);
  }

  async getGrantForAuthz(opts: {
    folderId: string;
    scopeNodeId: string;
    principal: Principal;
  }): Promise<{ grantId: string; scopeNodeId: string; canRead: boolean; canWrite: boolean } | undefined> {
    if (opts.principal.type === "user" && opts.principal.userId === "user-1" && opts.folderId === "folder-a" && opts.scopeNodeId === "root-a") {
      return { grantId: "grant-a", scopeNodeId: "root-a", canRead: true, canWrite: false };
    }
    return undefined;
  }
}

function applySqliteMigrations(sqlite: Database.Database): void {
  const migrationsDir = new URL("../db/migrations/sqlite/", import.meta.url);
  const migrationFiles = readdirSync(migrationsDir)
    .filter((fileName) => fileName.endsWith(".sql"))
    .sort();

  for (const fileName of migrationFiles) {
    const sql = readFileSync(new URL(fileName, migrationsDir), "utf8");
    for (const statement of sql.split("--> statement-breakpoint")) {
      const trimmed = statement.trim();
      if (trimmed) {
        sqlite.exec(trimmed);
      }
    }
  }
}

function seedSqliteParityFixture(sqlite: Database.Database): void {
  sqlite.prepare("INSERT INTO devices (device_id, name, token_hash) VALUES (?, ?, ?)").run("device-1", "Device", "hash");
  for (const folderId of ["folder-a", "folder-b"]) {
    sqlite.prepare("INSERT INTO shared_folders (folder_id, name, owner_user_id) VALUES (?, ?, ?)").run(folderId, folderId, "owner");
  }
  for (const chunkHash of ["shared-chunk", "cross-tenant-shared", "conflict-only", "secret-only"]) {
    sqlite.prepare("INSERT INTO chunks (chunk_hash, size_bytes, refcount) VALUES (?, ?, ?)").run(chunkHash, 10, 1);
  }
  for (const node of [
    { nodeId: "root-a", folderId: "folder-a", parentId: null, name: "", type: "folder" },
    { nodeId: "doc-a", folderId: "folder-a", parentId: "root-a", name: "doc.md", type: "file" },
    { nodeId: "root-b", folderId: "folder-b", parentId: null, name: "", type: "folder" },
    { nodeId: "secret-b", folderId: "folder-b", parentId: "root-b", name: "secret.md", type: "file" },
  ]) {
    sqlite.prepare(
      "INSERT INTO nodes (node_id, folder_id, parent_id, name, type, server_seq) VALUES (?, ?, ?, ?, ?, ?)",
    ).run(node.nodeId, node.folderId, node.parentId, node.name, node.type, 1);
  }
  sqlite.prepare(
    "INSERT INTO folder_grants (grant_id, folder_id, scope_node_id, user_id, role, can_read, can_write) VALUES (?, ?, ?, ?, ?, ?, ?)",
  ).run("grant-a", "folder-a", "root-a", "user-1", "collaborator", 1, 0);

  insertVersion(sqlite, "version-current", "doc-a", [
    { chunk_hash: "shared-chunk", offset: 0, length: 10 },
    { chunk_hash: "cross-tenant-shared", offset: 10, length: 10 },
  ], false);
  insertVersion(sqlite, "version-conflict", "doc-a", [
    { chunk_hash: "shared-chunk", offset: 0, length: 10 },
    { chunk_hash: "conflict-only", offset: 10, length: 10 },
  ], true);
  insertVersion(sqlite, "version-secret", "secret-b", [
    { chunk_hash: "secret-only", offset: 0, length: 10 },
    { chunk_hash: "cross-tenant-shared", offset: 10, length: 10 },
  ], false);
  sqlite.prepare("UPDATE nodes SET current_version_id = ? WHERE node_id = ?").run("version-current", "doc-a");
  sqlite.prepare("UPDATE nodes SET current_version_id = ? WHERE node_id = ?").run("version-secret", "secret-b");
}

function insertVersion(
  sqlite: Database.Database,
  versionId: string,
  nodeId: string,
  manifest: Array<{ chunk_hash: string; offset: number; length: number }>,
  isConflictCopy: boolean,
): void {
  sqlite.prepare(
    "INSERT INTO versions (version_id, node_id, manifest, content_hash, size_bytes, author_device_id, is_conflict_copy) VALUES (?, ?, ?, ?, ?, ?, ?)",
  ).run(versionId, nodeId, JSON.stringify(manifest), `${versionId}-hash`, 10, "device-1", isConflictCopy ? 1 : 0);
  for (const chunkHash of new Set(manifest.map((chunk) => chunk.chunk_hash))) {
    sqlite.prepare("INSERT INTO version_chunks (version_id, node_id, chunk_hash) VALUES (?, ?, ?)").run(versionId, nodeId, chunkHash);
  }
}
