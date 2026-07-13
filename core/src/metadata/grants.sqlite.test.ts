import { readdirSync, readFileSync } from "node:fs";

import Database from "better-sqlite3";
import { drizzle } from "drizzle-orm/better-sqlite3";
import { describe, expect, it, vi } from "vitest";

import type { CoreAuth, CoreDb } from "../auth/index.js";
import { sqliteSchema } from "../db/schema.js";
import { createMetadataRouter } from "./index.js";

describe("regenerate atomicity on a real sqlite driver", () => {
  it("rolls back the delete, token revocation, and inserts when onGrantDeviceCreated fails before commit", async () => {
    const sqlite = new Database(":memory:");
    try {
      applySqliteMigrations(sqlite);
      sqlite.exec("PRAGMA foreign_keys = ON");
      seedRegenerateFixture(sqlite);

      const app = createMetadataRouter({
        auth: makeAuth(sqlite),
        hub: { notify: () => undefined },
        onGrantDeviceCreated: vi.fn(async () => {
          throw new Error("tenant linkage failed");
        }),
      });

      const response = await app.request("/folders/folder-1/grants/old-grant/regenerate", { method: "POST" });

      expect(response.status).toBe(500);
      expect(sqlite.prepare("SELECT grant_id FROM folder_grants WHERE grant_id = ?").get("old-grant")).toBeDefined();
      expect(
        (sqlite.prepare("SELECT token_hash FROM devices WHERE device_id = ?").get("old-device") as { token_hash: string })
          .token_hash,
      ).toBe("old-hash");
      expect((sqlite.prepare("SELECT COUNT(*) AS count FROM devices").get() as { count: number }).count).toBe(1);
      expect((sqlite.prepare("SELECT COUNT(*) AS count FROM folder_grants").get() as { count: number }).count).toBe(2);
    } finally {
      sqlite.close();
    }
  });

  it("commits the delete-then-insert atomically on success", async () => {
    const sqlite = new Database(":memory:");
    try {
      applySqliteMigrations(sqlite);
      sqlite.exec("PRAGMA foreign_keys = ON");
      seedRegenerateFixture(sqlite);

      const app = createMetadataRouter({ auth: makeAuth(sqlite), hub: { notify: () => undefined } });

      const response = await app.request("/folders/folder-1/grants/old-grant/regenerate", { method: "POST" });
      const body = await response.json();

      expect(response.status).toBe(200);
      expect(sqlite.prepare("SELECT grant_id FROM folder_grants WHERE grant_id = ?").get("old-grant")).toBeUndefined();
      expect(
        (sqlite.prepare("SELECT token_hash FROM devices WHERE device_id = ?").get("old-device") as { token_hash: string })
          .token_hash,
      ).toBe("revoked:old-grant");
      const replacement = sqlite
        .prepare("SELECT name, scope_node_id, can_write FROM folder_grants WHERE grant_id = ?")
        .get(body.grant_id) as { name: string; scope_node_id: string; can_write: number };
      expect(replacement).toMatchObject({ name: "build-01", scope_node_id: "root" });
    } finally {
      sqlite.close();
    }
  });
});

function makeAuth(sqlite: Database.Database): CoreAuth {
  const db = Object.assign(drizzle(sqlite, { schema: sqliteSchema }), { __valvSqlite: true }) as CoreDb;
  return {
    db,
    schema: sqliteSchema,
    api: { getSession: async () => ({ user: { id: "user-1" } }) },
  } as unknown as CoreAuth;
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

function seedRegenerateFixture(sqlite: Database.Database): void {
  sqlite.prepare("INSERT INTO shared_folders (folder_id, name, owner_user_id) VALUES (?, ?, ?)").run("folder-1", "Folder", "user-1");
  sqlite
    .prepare("INSERT INTO nodes (node_id, folder_id, parent_id, name, type, server_seq) VALUES (?, ?, ?, ?, ?, ?)")
    .run("root", "folder-1", null, "", "folder", 0);
  sqlite
    .prepare(
      "INSERT INTO folder_grants (grant_id, folder_id, scope_node_id, user_id, role, can_read, can_write) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .run("owner-grant", "folder-1", "root", "user-1", "owner", 1, 1);
  sqlite.prepare("INSERT INTO devices (device_id, name, token_hash) VALUES (?, ?, ?)").run("old-device", "Agent", "old-hash");
  sqlite
    .prepare(
      "INSERT INTO folder_grants (grant_id, folder_id, scope_node_id, device_id, name, role, can_read, can_write, created_by_user_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .run("old-grant", "folder-1", "root", "old-device", "build-01", "collaborator", 1, 1, "user-1");
}
