import { readdirSync, readFileSync } from "node:fs";

import Database from "better-sqlite3";
import { drizzle } from "drizzle-orm/better-sqlite3";
import { describe, expect, it } from "vitest";

import type { CoreDb } from "../auth/index.js";
import { sqliteSchema } from "./schema.js";
import { backfillVersionChunks } from "./version-chunks-backfill.js";

describe("core schema", () => {
  it("cascade-deletes version_chunks rows with their version rows", () => {
    const sqlite = new Database(":memory:");
    try {
      applySqliteMigrations(sqlite);
      sqlite.exec("PRAGMA foreign_keys = ON");

      sqlite.prepare("INSERT INTO devices (device_id, name, token_hash) VALUES (?, ?, ?)").run("device-1", "Device", "hash");
      sqlite.prepare("INSERT INTO shared_folders (folder_id, name, owner_user_id) VALUES (?, ?, ?)").run("folder-1", "Folder", "user-1");
      sqlite.prepare("INSERT INTO chunks (chunk_hash, size_bytes, refcount) VALUES (?, ?, ?)").run("chunk-1", 10, 1);
      sqlite.prepare(
        "INSERT INTO nodes (node_id, folder_id, parent_id, name, type, server_seq) VALUES (?, ?, ?, ?, ?, ?)",
      ).run("node-1", "folder-1", null, "doc.md", "file", 1);
      sqlite.prepare(
        "INSERT INTO versions (version_id, node_id, manifest, content_hash, size_bytes, author_device_id, is_conflict_copy) VALUES (?, ?, ?, ?, ?, ?, ?)",
      ).run("version-1", "node-1", JSON.stringify([{ chunk_hash: "chunk-1", offset: 0, length: 10 }]), "hash-1", 10, "device-1", 0);
      sqlite.prepare("INSERT INTO version_chunks (version_id, node_id, chunk_hash) VALUES (?, ?, ?)").run(
        "version-1",
        "node-1",
        "chunk-1",
      );
      sqlite.prepare("UPDATE nodes SET current_version_id = ? WHERE node_id = ?").run("version-1", "node-1");

      sqlite.prepare("DELETE FROM nodes WHERE node_id = ?").run("node-1");

      expect(sqlite.prepare("SELECT COUNT(*) AS count FROM versions").get()).toEqual({ count: 0 });
      expect(sqlite.prepare("SELECT COUNT(*) AS count FROM version_chunks").get()).toEqual({ count: 0 });
    } finally {
      sqlite.close();
    }
  });

  it("backfills version_chunks idempotently from parsed manifests", async () => {
    const sqlite = new Database(":memory:");
    try {
      applySqliteMigrations(sqlite);
      sqlite.exec("PRAGMA foreign_keys = ON");
      seedVersionChunkBackfillFixture(sqlite);
      const db = Object.assign(drizzle(sqlite, { schema: sqliteSchema }), { __valvSqlite: true }) as CoreDb;

      const first = await backfillVersionChunks(db, sqliteSchema, { pageSize: 1 });
      const second = await backfillVersionChunks(db, sqliteSchema, { pageSize: 1 });

      expect(first).toEqual({ versionsScanned: 2, rowsAttempted: 3 });
      expect(second).toEqual({ versionsScanned: 2, rowsAttempted: 3 });
      expect(sqlite.prepare("SELECT version_id, node_id, chunk_hash FROM version_chunks ORDER BY version_id, chunk_hash").all())
        .toEqual([
          { version_id: "version-1", node_id: "node-1", chunk_hash: "chunk-1" },
          { version_id: "version-1", node_id: "node-1", chunk_hash: "chunk-2" },
          { version_id: "version-2", node_id: "node-1", chunk_hash: "chunk-2" },
        ]);
    } finally {
      sqlite.close();
    }
  });
});

function applySqliteMigrations(sqlite: Database.Database): void {
  const migrationsDir = new URL("./migrations/sqlite/", import.meta.url);
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

function seedVersionChunkBackfillFixture(sqlite: Database.Database): void {
  sqlite.prepare("INSERT INTO devices (device_id, name, token_hash) VALUES (?, ?, ?)").run("device-1", "Device", "hash");
  sqlite.prepare("INSERT INTO shared_folders (folder_id, name, owner_user_id) VALUES (?, ?, ?)").run("folder-1", "Folder", "user-1");
  for (const chunkHash of ["chunk-1", "chunk-2"]) {
    sqlite.prepare("INSERT INTO chunks (chunk_hash, size_bytes, refcount) VALUES (?, ?, ?)").run(chunkHash, 10, 1);
  }
  sqlite.prepare(
    "INSERT INTO nodes (node_id, folder_id, parent_id, name, type, server_seq) VALUES (?, ?, ?, ?, ?, ?)",
  ).run("node-1", "folder-1", null, "doc.md", "file", 1);
  sqlite.prepare(
    "INSERT INTO versions (version_id, node_id, manifest, content_hash, size_bytes, author_device_id, is_conflict_copy) VALUES (?, ?, ?, ?, ?, ?, ?)",
  ).run(
    "version-1",
    "node-1",
    JSON.stringify([
      { chunk_hash: "chunk-1", offset: 0, length: 10 },
      { chunk_hash: "chunk-2", offset: 10, length: 10 },
      { chunk_hash: "chunk-2", offset: 20, length: 10 },
    ]),
    "hash-1",
    30,
    "device-1",
    0,
  );
  sqlite.prepare(
    "INSERT INTO versions (version_id, node_id, manifest, content_hash, size_bytes, author_device_id, is_conflict_copy) VALUES (?, ?, ?, ?, ?, ?, ?)",
  ).run(
    "version-2",
    "node-1",
    JSON.stringify([{ chunk_hash: "chunk-2", offset: 0, length: 10 }]),
    "hash-2",
    10,
    "device-1",
    1,
  );
}
