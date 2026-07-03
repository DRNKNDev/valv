import { randomUUID } from "node:crypto";

import { createTestBucket, deleteTestBucket } from "./bucket.js";
import { requestJson, seedContext, type SeedContext } from "./helpers.js";
import { createSmokeApp } from "./server.js";
import type { RequestApp } from "../api/scenarios/types.js";

type SmokeApp = Awaited<ReturnType<typeof createSmokeApp>>;

export type AppContext = SmokeApp & {
  bucket: string;
  context: SeedContext;
  row<T = Record<string, unknown>>(sql: string, ...params: unknown[]): Promise<T | undefined>;
  rows<T = Record<string, unknown>>(sql: string, ...params: unknown[]): Promise<T[]>;
  exec(sql: string, ...params: unknown[]): Promise<void>;
};

export async function createAppContext(): Promise<AppContext> {
  const bootstrap = await createSmokeApp("bootstrap");
  const bucket = await createTestBucket(bootstrap.s3);
  bootstrap.cleanup();

  const app = await createSmokeApp(bucket);
  const context = await seedContext(app.db, app.sqlite);
  return {
    ...app,
    bucket,
    context,
    row: async <T = Record<string, unknown>>(sql: string, ...params: unknown[]) => row<T>(app.sqlite, sql, ...params),
    rows: async <T = Record<string, unknown>>(sql: string, ...params: unknown[]) => rows<T>(app.sqlite, sql, ...params),
    exec: async (sql: string, ...params: unknown[]) => {
      app.sqlite.prepare(sql).run(...params);
    },
  };
}

export async function createBareApp(): Promise<{ app: RequestApp; cleanup: () => Promise<void> }> {
  const bootstrap = await createSmokeApp("bootstrap");
  const bucket = await createTestBucket(bootstrap.s3);
  bootstrap.cleanup();

  const setup = await createSmokeApp(bucket);
  return {
    app: setup.app,
    cleanup: async () => {
      try {
        await deleteTestBucket(setup.s3, bucket);
      } finally {
        setup.cleanup();
      }
    },
  };
}

export async function cleanupAppContext(ctx: AppContext | undefined): Promise<void> {
  if (!ctx) {
    return;
  }
  try {
    await deleteTestBucket(ctx.s3, ctx.bucket);
  } finally {
    ctx.cleanup();
  }
}

export async function submitOp<T>(
  app: RequestApp,
  folderId: string,
  token: string,
  body: unknown,
  expectedStatus = 200,
): Promise<T> {
  return requestJson<T>(app, `/api/folders/${folderId}/ops`, {
    bearerToken: token,
    method: "POST",
    body,
    expectedStatus,
  });
}

export async function createNode(app: RequestApp, folderId: string, token: string, parentId: string, name: string, type: "file" | "folder") {
  const nodeId = randomUUID();
  const created = await submitOp<{ result: string; server_seq: number; node_id: string }>(app, folderId, token, {
    op_type: "create",
    payload: { node_id: nodeId, parent_id: parentId, name, type },
  });
  return { ...created, nodeId };
}

export function row<T = Record<string, unknown>>(sqlite: AppContext["sqlite"], sql: string, ...params: unknown[]): T | undefined {
  return sqlite.prepare(sql).get(...params) as T | undefined;
}

export function rows<T = Record<string, unknown>>(sqlite: AppContext["sqlite"], sql: string, ...params: unknown[]): T[] {
  return sqlite.prepare(sql).all(...params) as T[];
}
