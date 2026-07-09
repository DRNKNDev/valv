import type { AwsClient } from "aws4fetch";
import { sql, type SQL } from "drizzle-orm";

import type { CoreDb } from "../auth/index.js";
import { chunkKey, objectUrl } from "../blobstore/index.js";

export const DEFAULT_CHUNK_GC_INTERVAL_MS = 60 * 60 * 1000;
export const DEFAULT_CHUNK_GRACE_PERIOD_MS = 24 * 60 * 60 * 1000;
export const DEFAULT_TOMBSTONE_PURGE_INTERVAL_MS = 6 * 60 * 60 * 1000;
export const DEFAULT_TOMBSTONE_RETENTION_MS = 30 * 24 * 60 * 60 * 1000;
export const DEFAULT_OPLOG_TRUNCATION_INTERVAL_MS = 6 * 60 * 60 * 1000;
export const DEFAULT_OPLOG_RETENTION_MS = 90 * 24 * 60 * 60 * 1000;

export const DEFAULT_CHUNK_GC_BATCH_SIZE = 50;
export const DEFAULT_TOMBSTONE_BATCH_SIZE = 500;
export const DEFAULT_OPLOG_BATCH_SIZE = 500;

export const UNBOUNDED_BATCH_SIZE = Number.POSITIVE_INFINITY;

export type GcMode = "audit" | "delete";
export type GcPassName = "chunk_gc" | "tombstone_purge" | "oplog_truncation";

export type GcPassResult = {
  pass: GcPassName;
  mode: GcMode;
  eligibleCount: number;
  totalEligibleCount: number;
  deletedCount: number;
  errorCount: number;
};

export type ChunkDeletionTarget = {
  key: string;
  onDeleted?: () => Promise<void>;
};

export type GcOptions = {
  chunkGcIntervalMs?: number;
  chunkGracePeriodMs?: number;
  tombstonePurgeIntervalMs?: number;
  tombstoneRetentionMs?: number;
  opLogTruncationIntervalMs?: number;
  opLogRetentionMs?: number;
  resolveChunkDeletionTargets?: (chunkHash: string) => Promise<ChunkDeletionTarget[]> | ChunkDeletionTarget[];
};

export function startGc(
  db: CoreDb,
  s3: AwsClient,
  bucketName: string,
  bucketEndpoint?: string,
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
    setInterval(
      () =>
        void runChunkGcOnce({
          db,
          s3,
          bucketName,
          bucketEndpoint,
          gracePeriodMs: config.chunkGracePeriodMs,
          mode: "delete",
          batchSize: UNBOUNDED_BATCH_SIZE,
        }),
      config.chunkGcIntervalMs,
    ),
    setInterval(
      () =>
        void runTombstonePurgeOnce({
          db,
          retentionMs: config.tombstoneRetentionMs,
          mode: "delete",
          batchSize: UNBOUNDED_BATCH_SIZE,
        }),
      config.tombstonePurgeIntervalMs,
    ),
    setInterval(
      () =>
        void runOpLogTruncationOnce({
          db,
          retentionMs: config.opLogRetentionMs,
          mode: "delete",
          batchSize: UNBOUNDED_BATCH_SIZE,
        }),
      config.opLogTruncationIntervalMs,
    ),
  ];

  return () => {
    for (const timer of timers) {
      clearInterval(timer);
    }
  };
}

export type RunChunkGcOnceArgs = {
  db: CoreDb;
  s3: AwsClient;
  bucketName: string;
  bucketEndpoint?: string;
  gracePeriodMs?: number;
  mode?: GcMode;
  batchSize?: number;
  resolveChunkDeletionTargets?: GcOptions["resolveChunkDeletionTargets"];
};

export async function runChunkGcOnce(args: RunChunkGcOnceArgs): Promise<GcPassResult> {
  const mode: GcMode = args.mode ?? "delete";
  const batchSize = args.batchSize ?? DEFAULT_CHUNK_GC_BATCH_SIZE;
  const result: GcPassResult = {
    pass: "chunk_gc",
    mode,
    eligibleCount: 0,
    totalEligibleCount: 0,
    deletedCount: 0,
    errorCount: 0,
  };

  try {
    const cutoff = cutoffForDb(args.db, args.gracePeriodMs ?? DEFAULT_CHUNK_GRACE_PERIOD_MS);
    const eligibility = sql`SELECT chunk_hash FROM chunks WHERE refcount = 0 AND created_at < ${cutoff}`;
    const chunks = await executeRows(args.db, withLimit(eligibility, batchSize));
    result.eligibleCount = chunks.length;
    result.totalEligibleCount = await countRows(
      args.db,
      sql`SELECT COUNT(*) AS count FROM chunks WHERE refcount = 0 AND created_at < ${cutoff}`,
      result.eligibleCount,
    );

    if (mode === "audit") {
      return result;
    }

    for (const chunk of chunks) {
      const chunkHash = String(chunk.chunk_hash);
      try {
        const deletedRows = await executeRows(
          args.db,
          sql`DELETE FROM chunks WHERE chunk_hash = ${chunkHash} AND refcount = 0 RETURNING chunk_hash`,
        );
        if (deletedRows.length === 0) {
          continue;
        }
        result.deletedCount += 1;
        await deleteChunkTargets(args, chunkHash, result);
      } catch (error) {
        result.errorCount += 1;
        console.error("Chunk GC failed", { chunkHash, error });
      }
    }
  } catch (error) {
    result.errorCount += 1;
    console.error("Chunk GC pass failed", error);
  }

  return result;
}

async function deleteChunkTargets(
  args: RunChunkGcOnceArgs,
  chunkHash: string,
  result: GcPassResult,
): Promise<void> {
  const targets = await (args.resolveChunkDeletionTargets?.(chunkHash) ?? [{ key: chunkKey(chunkHash) }]);

  if (targets.length === 0) {
    result.errorCount += 1;
    console.error("gc_chunk_no_deletion_targets", { chunkHash });
    return;
  }

  for (const target of targets) {
    let deleted = false;
    try {
      const response = await args.s3.fetch(
        objectUrl({ bucketEndpoint: args.bucketEndpoint, bucketName: args.bucketName }, target.key),
        { method: "DELETE" },
      );
      // R2 fetch resolves on 4xx/5xx, so check response.ok.
      deleted = response.ok;
      if (!deleted) {
        result.errorCount += 1;
        console.error("Chunk GC failed", { chunkHash, key: target.key, status: response.status });
      }
    } catch (error) {
      result.errorCount += 1;
      console.error("Chunk GC failed", { chunkHash, key: target.key, error });
    }
    if (!deleted || !target.onDeleted) {
      continue;
    }
    try {
      await target.onDeleted();
    } catch (error) {
      result.errorCount += 1;
      console.error("Chunk GC deletion-target cleanup failed", { chunkHash, key: target.key, error });
    }
  }
}

export type RunTombstonePurgeOnceArgs = {
  db: CoreDb;
  retentionMs?: number;
  mode?: GcMode;
  batchSize?: number;
};

export async function runTombstonePurgeOnce(args: RunTombstonePurgeOnceArgs): Promise<GcPassResult> {
  const cutoff = cutoffForDb(args.db, args.retentionMs ?? DEFAULT_TOMBSTONE_RETENTION_MS);
  return runIdBatchPass(args.db, {
    pass: "tombstone_purge",
    mode: args.mode ?? "delete",
    batchSize: args.batchSize ?? DEFAULT_TOMBSTONE_BATCH_SIZE,
    eligibilitySql: () => sql`SELECT node_id AS id FROM nodes WHERE deleted_at IS NOT NULL AND deleted_at < ${cutoff}`,
    countSql: () => sql`SELECT COUNT(*) AS count FROM nodes WHERE deleted_at IS NOT NULL AND deleted_at < ${cutoff}`,
    deleteByIdsSql: (ids) => sql`DELETE FROM nodes WHERE node_id IN (${sql.join(ids.map((id) => sql`${id}`), sql`, `)})`,
    failureLogLabel: "Tombstone purge failed",
  });
}

export type RunOpLogTruncationOnceArgs = {
  db: CoreDb;
  retentionMs?: number;
  mode?: GcMode;
  batchSize?: number;
};

export async function runOpLogTruncationOnce(args: RunOpLogTruncationOnceArgs): Promise<GcPassResult> {
  const cutoff = cutoffForDb(args.db, args.retentionMs ?? DEFAULT_OPLOG_RETENTION_MS);
  return runIdBatchPass(args.db, {
    pass: "oplog_truncation",
    mode: args.mode ?? "delete",
    batchSize: args.batchSize ?? DEFAULT_OPLOG_BATCH_SIZE,
    eligibilitySql: () => sql`SELECT server_seq AS id FROM op_log WHERE applied_at < ${cutoff}`,
    countSql: () => sql`SELECT COUNT(*) AS count FROM op_log WHERE applied_at < ${cutoff}`,
    deleteByIdsSql: (ids) => sql`DELETE FROM op_log WHERE server_seq IN (${sql.join(ids.map((id) => sql`${id}`), sql`, `)})`,
    failureLogLabel: "Op-log truncation failed",
  });
}

type IdBatchPassConfig = {
  pass: GcPassName;
  mode: GcMode;
  batchSize: number;
  eligibilitySql: () => SQL;
  countSql: () => SQL;
  deleteByIdsSql: (ids: unknown[]) => SQL;
  failureLogLabel: string;
};

async function runIdBatchPass(db: CoreDb, config: IdBatchPassConfig): Promise<GcPassResult> {
  const result: GcPassResult = {
    pass: config.pass,
    mode: config.mode,
    eligibleCount: 0,
    totalEligibleCount: 0,
    deletedCount: 0,
    errorCount: 0,
  };

  try {
    const rows = await executeRows(db, withLimit(config.eligibilitySql(), config.batchSize));
    const ids = rows.map((row) => row.id);
    result.eligibleCount = ids.length;
    result.totalEligibleCount = await countRows(db, config.countSql(), result.eligibleCount);

    if (config.mode === "audit" || ids.length === 0) {
      return result;
    }

    await executeMutation(db, config.deleteByIdsSql(ids));
    result.deletedCount = ids.length;
  } catch (error) {
    result.errorCount += 1;
    console.error(config.failureLogLabel, error);
  }

  return result;
}

export function withLimit(query: SQL, batchSize: number): SQL {
  if (!Number.isFinite(batchSize)) {
    return query;
  }
  if (batchSize <= 0) {
    throw new Error(`GC batchSize must be a positive number or the unbounded sentinel; got ${batchSize}`);
  }
  return sql`${query} LIMIT ${batchSize}`;
}

async function countRows(db: CoreDb, query: SQL, fallback: number): Promise<number> {
  const rows = await executeRows(db, query);
  const raw = rows[0]?.count;
  const parsed = typeof raw === "string" ? Number.parseInt(raw, 10) : Number(raw);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function cutoffForDb(db: CoreDb, retentionMs: number): Date | number {
  const cutoff = Date.now() - retentionMs;
  return (db as CoreDb & { __valvSqlite?: boolean }).__valvSqlite ? cutoff : new Date(cutoff);
}

async function executeRows(db: CoreDb, query: unknown): Promise<any[]> {
  const maybeAll = (db as CoreDb & { all?: (query: unknown) => Promise<any[]> | any[] }).all;
  if (typeof maybeAll === "function") {
    return maybeAll.call(db, query);
  }
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
