import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import Database from "better-sqlite3";
import { drizzle as drizzleSqlite } from "drizzle-orm/better-sqlite3";
import { Hono } from "hono";

import {
  createAuth,
  createBlobstoreRouter,
  createDeviceAuthRouter,
  createHub,
  createMetadataRouter,
  createRealtimeRouter,
  sqliteSchema,
  type CoreAuth,
} from "@valv/core";

import { authSecret } from "./helpers.js";
import { createTestS3Client } from "./bucket.js";

const currentDir = dirname(fileURLToPath(import.meta.url));
const migrationDir = join(currentDir, "../../core/src/db/migrations/sqlite");

export async function createSmokeApp(bucketName: string) {
  const sqlite = new Database(":memory:");
  applyMigrations(sqlite);
  const db = Object.assign(drizzleSqlite(sqlite, { schema: sqliteSchema }), { __valvSqlite: true }) as CoreAuth["db"];
  const auth = createAuth(db, {
    secret: authSecret,
    baseURL: "http://localhost",
    provider: "sqlite",
    schema: sqliteSchema,
  });
  const s3 = createTestS3Client();
  const hub = createHub();
  const app = new Hono();

  app.on(["POST", "GET"], "/api/auth/*", (ctx: any) => auth.handler(ctx.req.raw));
  app.route("/auth", createDeviceAuthRouter(auth));
  app.route("/api", createMetadataRouter({ auth, hub }));
  app.route("/api", createBlobstoreRouter({ auth, s3Client: s3, bucketName }));
  app.route("/ws", createRealtimeRouter({ auth, hub }));

  return {
    app,
    db,
    sqlite,
    s3,
    cleanup: () => sqlite.close(),
  };
}

function applyMigrations(sqlite: Database.Database): void {
  const files = readdirSync(migrationDir).filter((file) => file.endsWith(".sql")).sort();
  for (const file of files) {
    const migration = readFileSync(join(migrationDir, file), "utf8");
    for (const statement of migration.split("--> statement-breakpoint")) {
      if (statement.trim()) {
        sqlite.exec(statement);
      }
    }
  }
}
