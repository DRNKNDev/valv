import Database from "better-sqlite3";
import { gt } from "drizzle-orm";
import { drizzle as drizzleSqlite } from "drizzle-orm/better-sqlite3";
import { drizzle as drizzlePg } from "drizzle-orm/node-postgres";
import { Pool } from "pg";

import type { CoreDb, CoreSchema } from "../auth/index.js";
import { pgSchema, sqliteSchema } from "./schema.js";

type ManifestChunk = { chunk_hash?: string };

export type VersionChunksBackfillResult = {
  versionsScanned: number;
  rowsAttempted: number;
};

export async function backfillVersionChunks(
  db: CoreDb,
  schema: CoreSchema,
  opts: { pageSize?: number } = {},
): Promise<VersionChunksBackfillResult> {
  const pageSize = opts.pageSize ?? 500;
  let lastSeenVersionId: string | undefined;
  let versionsScanned = 0;
  let rowsAttempted = 0;

  for (;;) {
    const versions = await selectVersionPage(db, schema, pageSize, lastSeenVersionId);

    if (versions.length === 0) {
      break;
    }

    versionsScanned += versions.length;
    const values = versions.flatMap((version: { versionId: string; nodeId: string; manifest: unknown }) => {
      const manifest = Array.isArray(version.manifest) ? version.manifest as ManifestChunk[] : [];
      return [...new Set(manifest.map((chunk) => chunk.chunk_hash).filter((chunkHash): chunkHash is string => Boolean(chunkHash)))]
        .map((chunkHash) => ({
          versionId: version.versionId,
          nodeId: version.nodeId,
          chunkHash,
        }));
    });

    if (values.length > 0) {
      const insert = db.insert(schema.versionChunks).values(values) as any;
      if (typeof insert.onConflictDoNothing === "function") {
        await insert.onConflictDoNothing();
      } else {
        await insert;
      }
      rowsAttempted += values.length;
    }

    lastSeenVersionId = versions[versions.length - 1].versionId;
  }

  return { versionsScanned, rowsAttempted };
}

async function selectVersionPage(
  db: CoreDb,
  schema: CoreSchema,
  pageSize: number,
  lastSeenVersionId: string | undefined,
): Promise<Array<{ versionId: string; nodeId: string; manifest: unknown }>> {
  const selection = {
    versionId: schema.versions.versionId,
    nodeId: schema.versions.nodeId,
    manifest: schema.versions.manifest,
  };

  if (lastSeenVersionId) {
    return db
      .select(selection)
      .from(schema.versions)
      .where(gt(schema.versions.versionId, lastSeenVersionId))
      .orderBy(schema.versions.versionId)
      .limit(pageSize);
  }

  return db
    .select(selection)
    .from(schema.versions)
    .orderBy(schema.versions.versionId)
    .limit(pageSize);
}

export async function runVersionChunksBackfillFromEnv(
  env: NodeJS.ProcessEnv = process.env,
): Promise<VersionChunksBackfillResult> {
  const databaseUrl = env.VALV_DATABASE_URL ?? env.DATABASE_URL;
  if (!databaseUrl) {
    throw new Error("Missing VALV_DATABASE_URL or DATABASE_URL");
  }

  if (isSqliteUrl(databaseUrl)) {
    const sqlitePath = databaseUrl.startsWith("file:") ? databaseUrl.slice("file:".length) : databaseUrl;
    const sqlite = new Database(sqlitePath);
    try {
      sqlite.pragma("foreign_keys = ON");
      const db = Object.assign(drizzleSqlite(sqlite, { schema: sqliteSchema }), { __valvSqlite: true }) as CoreDb;
      return backfillVersionChunks(db, sqliteSchema);
    } finally {
      sqlite.close();
    }
  }

  const pool = new Pool({ connectionString: databaseUrl });
  try {
    const db = drizzlePg(pool, { schema: pgSchema }) as CoreDb;
    return backfillVersionChunks(db, pgSchema);
  } finally {
    await pool.end();
  }
}

function isSqliteUrl(databaseUrl: string): boolean {
  return databaseUrl === ":memory:" || databaseUrl.startsWith("file:") || databaseUrl.endsWith(".db");
}

if (process.argv[1]?.endsWith("version-chunks-backfill.ts") || process.argv[1]?.endsWith("version-chunks-backfill.js")) {
  runVersionChunksBackfillFromEnv()
    .then((result) => {
      console.log("version_chunks backfill complete", result);
    })
    .catch((error) => {
      console.error("version_chunks backfill failed", error);
      process.exitCode = 1;
    });
}
