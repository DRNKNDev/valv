import { DeleteObjectCommand, type S3Client } from "@aws-sdk/client-s3";
import { sql } from "drizzle-orm";

import type { CoreDb } from "../auth/index.js";
import { chunkKey } from "../blobstore/index.js";

export const DEFAULT_CHUNK_GC_INTERVAL_MS = 60 * 60 * 1000;
export const DEFAULT_CHUNK_GRACE_PERIOD_MS = 24 * 60 * 60 * 1000;
export const DEFAULT_TOMBSTONE_PURGE_INTERVAL_MS = 6 * 60 * 60 * 1000;
export const DEFAULT_TOMBSTONE_RETENTION_MS = 30 * 24 * 60 * 60 * 1000;
export const DEFAULT_OPLOG_TRUNCATION_INTERVAL_MS = 6 * 60 * 60 * 1000;
export const DEFAULT_OPLOG_RETENTION_MS = 90 * 24 * 60 * 60 * 1000;

export type GcOptions = {
  chunkGcIntervalMs?: number;
  chunkGracePeriodMs?: number;
  tombstonePurgeIntervalMs?: number;
  tombstoneRetentionMs?: number;
  opLogTruncationIntervalMs?: number;
  opLogRetentionMs?: number;
};

export function startGc(
  db: CoreDb,
  s3Client: S3Client,
  bucketName: string,
  opts: GcOptions = {},
): () => void {
  const config = {
    chunkGcIntervalMs: opts.chunkGcIntervalMs ?? DEFAULT_CHUNK_GC_INTERVAL_MS,
    chunkGracePeriodMs: opts.chunkGracePeriodMs ?? DEFAULT_CHUNK_GRACE_PERIOD_MS,
    tombstonePurgeIntervalMs: opts.tombstonePurgeIntervalMs ?? DEFAULT_TOMBSTONE_PURGE_INTERVAL_MS,
    tombstoneRetentionMs: opts.tombstoneRetentionMs ?? DEFAULT_TOMBSTONE_RETENTION_MS,
    opLogTruncationIntervalMs: opts.opLogTruncationIntervalMs ?? DEFAULT_OPLOG_TRUNCATION_INTERVAL_MS,
    opLogRetentionMs: opts.opLogRetentionMs ?? DEFAULT_OPLOG_RETENTION_MS,
  };

  const timers = [
    setInterval(() => void runChunkGc(db, s3Client, bucketName, config.chunkGracePeriodMs), config.chunkGcIntervalMs),
    setInterval(() => void runTombstonePurge(db, config.tombstoneRetentionMs), config.tombstonePurgeIntervalMs),
    setInterval(() => void runOpLogTruncation(db, config.opLogRetentionMs), config.opLogTruncationIntervalMs),
  ];

  return () => {
    for (const timer of timers) {
      clearInterval(timer);
    }
  };
}

async function runChunkGc(
  db: CoreDb,
  s3Client: S3Client,
  bucketName: string,
  gracePeriodMs: number,
): Promise<void> {
  try {
    const cutoff = cutoffForDb(db, gracePeriodMs);
    const chunks = await executeRows(db, sql`
      SELECT chunk_hash FROM chunks WHERE refcount = 0 AND created_at < ${cutoff}
    `);
    for (const chunk of chunks) {
      const chunkHash = String(chunk.chunk_hash);
      try {
        await s3Client.send(new DeleteObjectCommand({ Bucket: bucketName, Key: chunkKey(chunkHash) }));
        await executeMutation(db, sql`DELETE FROM chunks WHERE chunk_hash = ${chunkHash} AND refcount = 0`);
      } catch (error) {
        console.error("Chunk GC failed", { chunkHash, error });
      }
    }
  } catch (error) {
    console.error("Chunk GC pass failed", error);
  }
}

async function runTombstonePurge(db: CoreDb, retentionMs: number): Promise<void> {
  try {
    const cutoff = cutoffForDb(db, retentionMs);
    await executeMutation(db, sql`DELETE FROM nodes WHERE deleted_at IS NOT NULL AND deleted_at < ${cutoff}`);
  } catch (error) {
    console.error("Tombstone purge failed", error);
  }
}

async function runOpLogTruncation(db: CoreDb, retentionMs: number): Promise<void> {
  try {
    const cutoff = cutoffForDb(db, retentionMs);
    await executeMutation(db, sql`DELETE FROM op_log WHERE applied_at < ${cutoff}`);
  } catch (error) {
    console.error("Op-log truncation failed", error);
  }
}

function cutoffForDb(db: CoreDb, retentionMs: number): Date | number {
  const cutoff = Date.now() - retentionMs;
  return (db as CoreDb & { __valvSqlite?: boolean }).__valvSqlite ? cutoff : new Date(cutoff);
}

async function executeRows(db: CoreDb, query: unknown): Promise<any[]> {
  if (typeof db.execute !== "function") {
    return [];
  }
  const result = await db.execute(query);
  if (Array.isArray(result)) {
    return result;
  }
  if (Array.isArray(result.rows)) {
    return result.rows;
  }
  return [];
}

async function executeMutation(db: CoreDb, query: unknown): Promise<unknown> {
  const maybeRun = (db as CoreDb & { run?: (query: unknown) => unknown }).run;
  if (typeof maybeRun === "function") {
    return maybeRun.call(db, query);
  }
  return db.execute?.(query);
}
