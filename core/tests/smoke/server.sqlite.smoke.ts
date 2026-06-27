import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import Database from "better-sqlite3";
import { drizzle as drizzleSqlite } from "drizzle-orm/better-sqlite3";
import { Hono } from "hono";
import { serializeSigned } from "hono/utils/cookie";
import { afterEach, describe, expect, it } from "vitest";

import {
  createAuth,
  createDeviceAuthRouter,
  createHub,
  createMetadataRouter,
  sqliteSchema,
  type CoreAuth,
} from "../../src/index.js";

const authSecret = "12345678901234567890123456789012";

describe("SQLite server smoke", () => {
  let cleanup: (() => void) | undefined;

  afterEach(() => {
    cleanup?.();
    cleanup = undefined;
  });

  it("creates a folder, submits a create op, and pulls it by delta over HTTP", async () => {
    const setup = await createSmokeApp();
    cleanup = setup.cleanup;

    const authSession = { cookie: setup.cookie };
    const folder = await requestJson<{ folder_id: string }>(setup.app, "/api/folders", {
      authSession,
      method: "POST",
      body: { name: "Smoke Folder" },
    });
    const grants = await requestJson<Array<{ folder_id: string; scope_node_id: string }>>(setup.app, "/api/grants", {
      authSession,
    });
    const rootNodeId = grants.find((grant) => grant.folder_id === folder.folder_id)?.scope_node_id;

    expect(rootNodeId).toBeTruthy();

    const agent = await requestJson<{ device_id: string; token: string }>(
      setup.app,
      `/api/folders/${folder.folder_id}/grants`,
      {
        authSession,
        method: "POST",
        body: { scope_node_id: rootNodeId, name: "Smoke Agent", can_read: true, can_write: true },
      },
    );
    const created = await requestJson<{ result: string; server_seq: number; node_id: string }>(
      setup.app,
      `/api/folders/${folder.folder_id}/ops`,
      {
        bearerToken: agent.token,
        method: "POST",
        body: {
          op_type: "create",
          payload: { parent_id: rootNodeId, name: "smoke.txt", type: "file" },
        },
      },
    );
    const delta = await requestJson<{ ops: Array<{ server_seq: number; op_type: string }> }>(
      setup.app,
      `/api/folders/${folder.folder_id}/ops?since=0`,
      { bearerToken: agent.token },
    );

    expect(created.result).toBe("applied");
    expect(delta.ops).toContainEqual(expect.objectContaining({ server_seq: created.server_seq, op_type: "create" }));
  });
});

async function createSmokeApp() {
  const dir = mkdtempSync(join(tmpdir(), "valv-core-smoke-"));
  const sqlite = new Database(join(dir, "smoke.db"));
  applyMigrations(sqlite);
  const db = drizzleSqlite(sqlite, { schema: sqliteSchema }) as CoreAuth["db"];
  const auth = createAuth(db, {
    secret: authSecret,
    baseURL: "http://localhost",
    provider: "sqlite",
    schema: sqliteSchema,
  });
  const app = new Hono();
  app.on(["POST", "GET"], "/api/auth/*", (ctx) => auth.handler(ctx.req.raw));
  app.route("/auth", createDeviceAuthRouter(auth));
  app.route("/api", createMetadataRouter({ auth, hub: createHub() }));
  const cookie = await seedSession(sqlite);

  return {
    app,
    cookie,
    cleanup: () => {
      sqlite.close();
      rmSync(dir, { force: true, recursive: true });
    },
  };
}

function applyMigrations(sqlite: Database.Database) {
  for (const migrationPath of [
    "src/db/migrations/sqlite/0000_orange_master_mold.sql",
    "src/db/migrations/sqlite/0001_tricky_vengeance.sql",
  ]) {
    const migration = readFileSync(migrationPath, "utf8");
    for (const statement of migration.split("--> statement-breakpoint")) {
      if (statement.trim()) {
        sqlite.exec(statement);
      }
    }
  }
}

async function seedSession(sqlite: Database.Database) {
  const now = Date.now();
  sqlite
    .prepare(
      `INSERT INTO "user" (id, name, email, emailVerified, image, createdAt, updatedAt) VALUES (?, ?, ?, ?, ?, ?, ?)`,
    )
    .run("user-1", "Smoke User", "smoke@example.com", 1, null, now, now);
  sqlite
    .prepare(
      `INSERT INTO "session" (id, token, userId, expiresAt, ipAddress, userAgent, createdAt, updatedAt) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
    )
    .run("session-1", "smoke-session-token", "user-1", now + 7 * 24 * 60 * 60 * 1000, "127.0.0.1", "smoke", now, now);

  const cookie = await serializeSigned("better-auth.session_token", "smoke-session-token", authSecret, {
    httpOnly: true,
    path: "/",
  });
  return cookie.split(";")[0];
}

async function requestJson<T>(
  app: Hono,
  path: string,
  opts: {
    authSession?: { cookie: string };
    bearerToken?: string;
    body?: unknown;
    method?: string;
  } = {},
): Promise<T> {
  const response = await app.request(path, {
    method: opts.method ?? "GET",
    body: opts.body ? JSON.stringify(opts.body) : undefined,
    headers: {
      ...(opts.body ? { "content-type": "application/json" } : {}),
      ...(opts.authSession ? { cookie: opts.authSession.cookie } : {}),
      ...(opts.bearerToken ? { authorization: `Bearer ${opts.bearerToken}` } : {}),
    },
  });
  const text = await response.text();
  expect(response.status, text).toBeLessThan(400);
  return text ? JSON.parse(text) as T : (undefined as T);
}
