import { PgDialect } from "drizzle-orm/pg-core";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CoreDb } from "../auth/index.js";
import {
  DEFAULT_CHUNK_GC_BATCH_SIZE,
  UNBOUNDED_BATCH_SIZE,
  runChunkGcOnce,
  runOpLogTruncationOnce,
  runTombstonePurgeOnce,
  startGc,
  withLimit,
} from "./index.js";
import { sql } from "drizzle-orm";

const dialect = new PgDialect();

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("GC service - startGc (interval-based, standalone server)", () => {
  it("deletes the DB row before deleting the R2 object (design D8)", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-27T00:00:00.000Z"));
    const db = new GcTestDb({ chunks: { old: 0 } });
    const s3Client = s3For(db);

    startGc(db, s3Client, "bucket", undefined, intervalOpts());
    await vi.advanceTimersByTimeAsync(100);

    expect(db.events.indexOf("db:chunk-delete:old")).toBeLessThan(db.events.indexOf("s3:chunks/old"));
    expect(db.chunkDeletes).toEqual(["old"]);
  });

  it("leaves an orphaned R2 object (not the DB row) when R2 deletion fails after a successful DB delete", async () => {
    vi.useFakeTimers();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const db = new GcTestDb({ chunks: { old: 0 } });
    const s3Client = s3For(db, new Error("r2 unavailable"));

    startGc(db, s3Client, "bucket", undefined, intervalOpts());
    await vi.advanceTimersByTimeAsync(100);

    expect(db.chunkDeletes).toEqual(["old"]);
    expect(consoleError).toHaveBeenCalledWith(
      "Chunk GC failed",
      expect.objectContaining({ chunkHash: "old", key: "chunks/old" }),
    );
  });

  it("honors tombstone and op-log retention cutoffs", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-27T00:00:00.000Z"));
    const db = new GcTestDb({ tombstoneNodeIds: ["n1"], opLogIds: [1] });

    startGc(db, s3For(db), "bucket", undefined, {
      ...intervalOpts(),
      tombstoneRetentionMs: 10_000,
      opLogRetentionMs: 20_000,
    });
    await vi.advanceTimersByTimeAsync(100);

    expect(db.tombstoneCutoff?.toISOString()).toBe("2026-06-26T23:59:50.100Z");
    expect(db.opLogCutoff?.toISOString()).toBe("2026-06-26T23:59:40.100Z");
    expect(db.deletedTombstoneIds).toEqual(["n1"]);
    expect(db.deletedOpLogIds).toEqual([1]);
  });

  it("clears all timers when stopped", () => {
    vi.useFakeTimers();
    const stopGc = startGc(new GcTestDb(), s3For(new GcTestDb()), "bucket", undefined, intervalOpts());

    expect(vi.getTimerCount()).toBe(3);
    stopGc();

    expect(vi.getTimerCount()).toBe(0);
  });

  it("logs pass errors without rethrowing", async () => {
    vi.useFakeTimers();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const db = new GcTestDb({ failChunkSelect: true });

    startGc(db, s3For(db), "bucket", undefined, intervalOpts());
    await vi.advanceTimersByTimeAsync(100);

    expect(consoleError).toHaveBeenCalledWith("Chunk GC pass failed", expect.any(Error));
  });

  it("finds and deletes eligible chunks on a SQLite-shaped db without execute", async () => {
    vi.useFakeTimers();
    const db = new GcSqliteTestDb({ chunks: { old: 0 } });
    const s3Client = s3For(db);

    startGc(db, s3Client, "bucket", undefined, intervalOpts());
    await vi.advanceTimersByTimeAsync(100);

    expect(db.events).toContain("db:chunk-select");
    expect(db.events.indexOf("db:chunk-delete:old")).toBeLessThan(db.events.indexOf("s3:chunks/old"));
    expect(db.chunkDeletes).toEqual(["old"]);
  });

  it("with no resolveChunkDeletionTargets configured, a deleted chunk results in exactly one DeleteObject call for the flat key", async () => {
    vi.useFakeTimers();
    const db = new GcTestDb({ chunks: { old: 0 } });
    const s3Client = s3For(db);

    startGc(db, s3Client, "bucket", undefined, intervalOpts());
    await vi.advanceTimersByTimeAsync(100);

    expect(s3Client.fetch).toHaveBeenCalledTimes(1);
    expect(s3Client.fetch).toHaveBeenCalledWith("https://bucket.s3.amazonaws.com/bucket/chunks/old", expect.any(Object));
  });

  it("startGc's interval-driven call is unbounded, processing more rows than any one-shot default batch size", async () => {
    vi.useFakeTimers();
    const manyChunks = Object.fromEntries(
      Array.from({ length: DEFAULT_CHUNK_GC_BATCH_SIZE + 25 }, (_, i) => [`chunk-${i}`, 0]),
    );
    const db = new GcTestDb({ chunks: manyChunks });
    const s3Client = s3For(db);

    startGc(db, s3Client, "bucket", undefined, intervalOpts());
    await vi.advanceTimersByTimeAsync(100);

    expect(db.chunkDeletes).toHaveLength(DEFAULT_CHUNK_GC_BATCH_SIZE + 25);
  });
});

describe("GC service - one-shot pass functions", () => {
  it("a one-shot call performs exactly one bounded pass and returns without scheduling further work", async () => {
    const db = new GcTestDb({ chunks: { old: 0 } });
    const s3Client = s3For(db);

    const result = await runChunkGcOnce({ db, s3: s3Client, bucketName: "bucket", mode: "delete" });

    expect(result).toEqual({
      pass: "chunk_gc",
      mode: "delete",
      eligibleCount: 1,
      totalEligibleCount: 1,
      deletedCount: 1,
      errorCount: 0,
    });
    expect(db.events.filter((e) => e === "db:chunk-select")).toHaveLength(1);
  });

  it("audit mode makes zero R2/DB mutation calls while still reporting real eligible and total-eligible counts", async () => {
    const db = new GcTestDb({ chunks: { a: 0, b: 0 } });
    const s3Client = s3For(db);

    const result = await runChunkGcOnce({ db, s3: s3Client, bucketName: "bucket", mode: "audit" });

    expect(result).toEqual({
      pass: "chunk_gc",
      mode: "audit",
      eligibleCount: 2,
      totalEligibleCount: 2,
      deletedCount: 0,
      errorCount: 0,
    });
    expect(s3Client.fetch).not.toHaveBeenCalled();
    expect(db.chunkDeletes).toEqual([]);
  });

  it("audit mode reports eligible tombstones/op-log rows without deleting them", async () => {
    const db = new GcTestDb({ tombstoneNodeIds: ["n1", "n2"], opLogIds: [1, 2, 3] });

    const tombstoneResult = await runTombstonePurgeOnce({ db, mode: "audit" });
    const opLogResult = await runOpLogTruncationOnce({ db, mode: "audit" });

    expect(tombstoneResult).toEqual({
      pass: "tombstone_purge",
      mode: "audit",
      eligibleCount: 2,
      totalEligibleCount: 2,
      deletedCount: 0,
      errorCount: 0,
    });
    expect(opLogResult).toEqual({
      pass: "oplog_truncation",
      mode: "audit",
      eligibleCount: 3,
      totalEligibleCount: 3,
      deletedCount: 0,
      errorCount: 0,
    });
    expect(db.deletedTombstoneIds).toEqual([]);
    expect(db.deletedOpLogIds).toEqual([]);
  });

  it("a batch size smaller than the eligible set caps deletes; a second call makes further progress without reprocessing", async () => {
    const db = new GcTestDb({ chunks: { a: 0, b: 0, c: 0 } });
    const s3Client = s3For(db);

    const first = await runChunkGcOnce({ db, s3: s3Client, bucketName: "bucket", mode: "delete", batchSize: 2 });
    expect(first.eligibleCount).toBe(2);
    expect(first.totalEligibleCount).toBe(3);
    expect(first.deletedCount).toBe(2);
    expect(db.chunkDeletes).toHaveLength(2);

    const second = await runChunkGcOnce({ db, s3: s3Client, bucketName: "bucket", mode: "delete", batchSize: 2 });
    expect(second.eligibleCount).toBe(1);
    expect(second.totalEligibleCount).toBe(1);
    expect(second.deletedCount).toBe(1);
    expect(db.chunkDeletes).toHaveLength(3);
    expect(new Set(db.chunkDeletes)).toEqual(new Set(["a", "b", "c"]));
  });

  it("tombstone purge and op-log truncation are capped by batch size across multiple calls", async () => {
    const db = new GcTestDb({ tombstoneNodeIds: ["n1", "n2", "n3"], opLogIds: [1, 2, 3] });

    const first = await runTombstonePurgeOnce({ db, mode: "delete", batchSize: 2 });
    expect(first).toEqual({
      pass: "tombstone_purge",
      mode: "delete",
      eligibleCount: 2,
      totalEligibleCount: 3,
      deletedCount: 2,
      errorCount: 0,
    });
    const second = await runTombstonePurgeOnce({ db, mode: "delete", batchSize: 2 });
    expect(second).toEqual({
      pass: "tombstone_purge",
      mode: "delete",
      eligibleCount: 1,
      totalEligibleCount: 1,
      deletedCount: 1,
      errorCount: 0,
    });
    expect(db.deletedTombstoneIds).toEqual(["n1", "n2", "n3"]);

    const opFirst = await runOpLogTruncationOnce({ db, mode: "delete", batchSize: 2 });
    expect(opFirst).toEqual({
      pass: "oplog_truncation",
      mode: "delete",
      eligibleCount: 2,
      totalEligibleCount: 3,
      deletedCount: 2,
      errorCount: 0,
    });
    const opSecond = await runOpLogTruncationOnce({ db, mode: "delete", batchSize: 2 });
    expect(opSecond).toEqual({
      pass: "oplog_truncation",
      mode: "delete",
      eligibleCount: 1,
      totalEligibleCount: 1,
      deletedCount: 1,
      errorCount: 0,
    });
    expect(db.deletedOpLogIds).toEqual([1, 2, 3]);
  });

  it("uses the documented default batch size when no batchSize option is given", async () => {
    const manyChunks = Object.fromEntries(
      Array.from({ length: DEFAULT_CHUNK_GC_BATCH_SIZE + 10 }, (_, i) => [`chunk-${i}`, 0]),
    );
    const db = new GcTestDb({ chunks: manyChunks });
    const s3Client = s3For(db);

    const result = await runChunkGcOnce({ db, s3: s3Client, bucketName: "bucket", mode: "delete" });

    expect(result.eligibleCount).toBe(DEFAULT_CHUNK_GC_BATCH_SIZE);
    expect(result.totalEligibleCount).toBe(DEFAULT_CHUNK_GC_BATCH_SIZE + 10);
    expect(result.deletedCount).toBe(DEFAULT_CHUNK_GC_BATCH_SIZE);
  });

  it("a chunk whose refcount is concurrently bumped from 0 to 1 is not deleted from R2", async () => {
    const db = new GcTestDb({ chunks: { live: 0 } });
    const s3Client = s3For(db);
    db.onBeforeChunkDeleteAttempt = (chunkHash) => {
      if (chunkHash === "live") {
        db.chunks.set("live", { refcount: 1, createdAt: 0 });
      }
    };

    const result = await runChunkGcOnce({ db, s3: s3Client, bucketName: "bucket", mode: "delete" });

    expect(result).toMatchObject({ eligibleCount: 1, deletedCount: 0, errorCount: 0 });
    expect(s3Client.fetch).not.toHaveBeenCalled();
    expect(db.chunks.get("live")?.refcount).toBe(1);
  });

  it("R2 delete failure after a successful DB delete is counted as an error and does not un-delete the row", async () => {
    const db = new GcTestDb({ chunks: { old: 0 } });
    const s3Client = s3For(db, new Error("r2 unavailable"));

    const result = await runChunkGcOnce({ db, s3: s3Client, bucketName: "bucket", mode: "delete" });

    expect(result).toMatchObject({ eligibleCount: 1, deletedCount: 1, errorCount: 1 });
    expect(db.chunkDeletes).toEqual(["old"]);
  });

  it("with a test-supplied resolveChunkDeletionTargets returning two targets, both are deleted and their own onDeleted callbacks fire independently", async () => {
    const db = new GcTestDb({ chunks: { shared: 0 } });
    const s3Client = s3For(db);
    const onDeletedA = vi.fn(async () => undefined);
    const onDeletedB = vi.fn(async () => undefined);

    const result = await runChunkGcOnce({
      db,
      s3: s3Client,
      bucketName: "bucket",
      mode: "delete",
      resolveChunkDeletionTargets: () => [
        { key: "chunks/tenant-a/shared", onDeleted: onDeletedA },
        { key: "chunks/tenant-b/shared", onDeleted: onDeletedB },
      ],
    });

    expect(result).toMatchObject({ deletedCount: 1, errorCount: 0 });
    expect(s3Client.fetch).toHaveBeenCalledTimes(2);
    expect(s3Client.fetch).toHaveBeenCalledWith(
      "https://bucket.s3.amazonaws.com/bucket/chunks/tenant-a/shared",
      expect.objectContaining({ method: "DELETE" }),
    );
    expect(s3Client.fetch).toHaveBeenCalledWith(
      "https://bucket.s3.amazonaws.com/bucket/chunks/tenant-b/shared",
      expect.objectContaining({ method: "DELETE" }),
    );
    expect(onDeletedA).toHaveBeenCalledTimes(1);
    expect(onDeletedB).toHaveBeenCalledTimes(1);
  });

  it("a simulated failure on one target's delete still lets the other target's delete and callback proceed", async () => {
    const db = new GcTestDb({ chunks: { shared: 0 } });
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const onDeletedA = vi.fn(async () => undefined);
    const onDeletedB = vi.fn(async () => undefined);
    const s3Client = {
      fetch: vi.fn(async (url: string) => {
        if (url.includes("tenant-a")) {
          throw new Error("r2 unavailable for tenant-a");
        }
        return new Response(null, { status: 204 });
      }),
    } as any;

    const result = await runChunkGcOnce({
      db,
      s3: s3Client,
      bucketName: "bucket",
      mode: "delete",
      resolveChunkDeletionTargets: () => [
        { key: "chunks/tenant-a/shared", onDeleted: onDeletedA },
        { key: "chunks/tenant-b/shared", onDeleted: onDeletedB },
      ],
    });

    expect(result.deletedCount).toBe(1);
    expect(result.errorCount).toBe(1);
    expect(onDeletedA).not.toHaveBeenCalled();
    expect(onDeletedB).toHaveBeenCalledTimes(1);
    expect(consoleError).toHaveBeenCalledWith(
      "Chunk GC failed",
      expect.objectContaining({ chunkHash: "shared", key: "chunks/tenant-a/shared" }),
    );
  });

  it("excludes a chunk newer than the grace cutoff from eligibility and never deletes it", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-27T00:00:00.000Z"));
    const now = Date.now();
    const db = new GcTestDb({
      chunks: {
        old: { refcount: 0, createdAt: now - 48 * 60 * 60 * 1000 },
        fresh: { refcount: 0, createdAt: now },
      },
    });
    const s3Client = s3For(db);

    const result = await runChunkGcOnce({ db, s3: s3Client, bucketName: "bucket", mode: "delete" });

    expect(result.eligibleCount).toBe(1);
    expect(result.totalEligibleCount).toBe(1);
    expect(result.deletedCount).toBe(1);
    expect(db.chunkDeletes).toEqual(["old"]);
    expect(db.chunks.has("fresh")).toBe(true);
    expect(s3Client.fetch).toHaveBeenCalledTimes(1);
  });

  it("excludes a chunk that starts with refcount > 0 and never touches it", async () => {
    const db = new GcTestDb({ chunks: { live: 1, dead: 0 } });
    const s3Client = s3For(db);

    const result = await runChunkGcOnce({ db, s3: s3Client, bucketName: "bucket", mode: "delete" });

    expect(result.eligibleCount).toBe(1);
    expect(result.totalEligibleCount).toBe(1);
    expect(result.deletedCount).toBe(1);
    expect(db.chunkDeletes).toEqual(["dead"]);
    expect(db.chunks.has("live")).toBe(true);
    expect(s3Client.fetch).toHaveBeenCalledTimes(1);
  });

  it("processes the remaining chunks when one chunk's R2 delete throws, counting exactly the failures", async () => {
    const db = new GcTestDb({ chunks: { a: 0, b: 0, c: 0 } });
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const s3Client = {
      fetch: vi.fn(async (url: string) => {
        if (url.includes("chunks/b")) {
          throw new Error("r2 unavailable for b");
        }
        return new Response(null, { status: 204 });
      }),
    } as any;

    const result = await runChunkGcOnce({ db, s3: s3Client, bucketName: "bucket", mode: "delete" });

    expect(result.eligibleCount).toBe(3);
    expect(result.deletedCount).toBe(3);
    expect(result.errorCount).toBe(1);
    expect(new Set(db.chunkDeletes)).toEqual(new Set(["a", "b", "c"]));
    expect(s3Client.fetch).toHaveBeenCalledTimes(3);
  });

  it("treats a non-ok R2 response as an error: onDeleted is NOT called and errorCount is incremented", async () => {
    const db = new GcTestDb({ chunks: { shared: 0 } });
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const onDeleted = vi.fn(async () => undefined);
    const s3Client = s3ReturningStatus(db, 500);

    const result = await runChunkGcOnce({
      db,
      s3: s3Client,
      bucketName: "bucket",
      mode: "delete",
      resolveChunkDeletionTargets: () => [{ key: "chunks/tenant-a/shared", onDeleted }],
    });

    expect(result.deletedCount).toBe(1);
    expect(result.errorCount).toBe(1);
    expect(onDeleted).not.toHaveBeenCalled();
    expect(consoleError).toHaveBeenCalledWith(
      "Chunk GC failed",
      expect.objectContaining({ chunkHash: "shared", key: "chunks/tenant-a/shared", status: 500 }),
    );
  });

  it("signals an error when a just-deleted chunk resolves to zero deletion targets", async () => {
    const db = new GcTestDb({ chunks: { orphan: 0 } });
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const s3Client = s3For(db);

    const result = await runChunkGcOnce({
      db,
      s3: s3Client,
      bucketName: "bucket",
      mode: "delete",
      resolveChunkDeletionTargets: () => [],
    });

    expect(result.deletedCount).toBe(1);
    expect(result.errorCount).toBe(1);
    expect(s3Client.fetch).not.toHaveBeenCalled();
    expect(consoleError).toHaveBeenCalledWith(
      "gc_chunk_no_deletion_targets",
      expect.objectContaining({ chunkHash: "orphan" }),
    );
  });

  it("returns an all-zero, error-free result for empty eligible sets across all three passes", async () => {
    const db = new GcTestDb();
    const s3Client = s3For(db);

    const chunkResult = await runChunkGcOnce({ db, s3: s3Client, bucketName: "bucket", mode: "delete" });
    const tombstoneResult = await runTombstonePurgeOnce({ db, mode: "delete" });
    const opLogResult = await runOpLogTruncationOnce({ db, mode: "delete" });

    expect(chunkResult).toEqual({
      pass: "chunk_gc",
      mode: "delete",
      eligibleCount: 0,
      totalEligibleCount: 0,
      deletedCount: 0,
      errorCount: 0,
    });
    expect(tombstoneResult).toEqual({
      pass: "tombstone_purge",
      mode: "delete",
      eligibleCount: 0,
      totalEligibleCount: 0,
      deletedCount: 0,
      errorCount: 0,
    });
    expect(opLogResult).toEqual({
      pass: "oplog_truncation",
      mode: "delete",
      eligibleCount: 0,
      totalEligibleCount: 0,
      deletedCount: 0,
      errorCount: 0,
    });
    expect(s3Client.fetch).not.toHaveBeenCalled();
  });
});

describe("GC service - withLimit", () => {
  const base = sql`SELECT chunk_hash FROM chunks WHERE refcount = 0`;

  it("appends a LIMIT for a finite positive batch size", () => {
    const { sql: text, params } = dialect.sqlToQuery(withLimit(base, 5));
    expect(text).toContain("LIMIT");
    expect(params).toContain(5);
  });

  it("omits the LIMIT for the unbounded sentinel", () => {
    const { sql: text } = dialect.sqlToQuery(withLimit(base, UNBOUNDED_BATCH_SIZE));
    expect(text).not.toContain("LIMIT");
  });

  it("throws for a finite non-positive batch size", () => {
    expect(() => withLimit(base, 0)).toThrow(/positive number or the unbounded sentinel/);
    expect(() => withLimit(base, -5)).toThrow(/positive number or the unbounded sentinel/);
  });
});

class GcTestDb implements CoreDb {
  select: CoreDb["select"];
  select: CoreDb["select"];
  insert: CoreDb["insert"];
  update: CoreDb["update"];
  delete: CoreDb["delete"];
  events: string[] = [];
  chunkDeletes: string[] = [];
  deletedTombstoneIds: unknown[] = [];
  deletedOpLogIds: unknown[] = [];
  tombstoneCutoff?: Date;
  opLogCutoff?: Date;
  chunks: Map<string, { refcount: number; createdAt: number }>;
  onBeforeChunkDeleteAttempt?: (chunkHash: string) => void;
  private tombstoneNodeIds: string[];
  private opLogIds: unknown[];
  private failChunkSelect: boolean;

  constructor(
    opts: {
      chunks?: Record<string, number | { refcount?: number; createdAt?: number }>;
      tombstoneNodeIds?: string[];
      opLogIds?: unknown[];
      failChunkSelect?: boolean;
    } = {},
  ) {
    this.chunks = new Map(
      Object.entries(opts.chunks ?? {}).map(([hash, value]) => [
        hash,
        typeof value === "number"
          ? { refcount: value, createdAt: 0 }
          : { refcount: value.refcount ?? 0, createdAt: value.createdAt ?? 0 },
      ]),
    );
    this.tombstoneNodeIds = [...(opts.tombstoneNodeIds ?? [])];
    this.opLogIds = [...(opts.opLogIds ?? [])];
    this.failChunkSelect = opts.failChunkSelect ?? false;
  }

  private eligibleChunkHashes(cutoff: Date): string[] {
    const cutoffMs = cutoff.getTime();
    return [...this.chunks.entries()]
      .filter(([, chunk]) => chunk.refcount === 0 && chunk.createdAt < cutoffMs)
      .map(([hash]) => hash);
  }

  async execute(query: unknown): Promise<Array<Record<string, unknown>>> {
    const { sql: text, params } = dialect.sqlToQuery(query as any);

    if (text.startsWith("SELECT chunk_hash FROM chunks")) {
      this.events.push("db:chunk-select");
      if (this.failChunkSelect) {
        throw new Error("select failed");
      }
      const limit = text.includes("LIMIT") ? Number(params[1]) : Number.POSITIVE_INFINITY;
      const eligible = this.eligibleChunkHashes(params[0] as Date);
      return eligible.slice(0, limit).map((chunkHash) => ({ chunk_hash: chunkHash }));
    }
    if (text.startsWith("SELECT COUNT(*) AS count FROM chunks")) {
      return [{ count: this.eligibleChunkHashes(params[0] as Date).length }];
    }
    if (text.startsWith("DELETE FROM chunks") && text.includes("RETURNING chunk_hash")) {
      const chunkHash = String(params[0]);
      this.onBeforeChunkDeleteAttempt?.(chunkHash);
      const chunk = this.chunks.get(chunkHash);
      if (!chunk || chunk.refcount !== 0) {
        return [];
      }
      this.chunks.delete(chunkHash);
      this.chunkDeletes.push(chunkHash);
      this.events.push(`db:chunk-delete:${chunkHash}`);
      return [{ chunk_hash: chunkHash }];
    }
    if (text.startsWith("SELECT node_id AS id FROM nodes")) {
      this.events.push("db:tombstone-select");
      this.tombstoneCutoff = params[0] as Date;
      const limit = text.includes("LIMIT") ? Number(params[1]) : Number.POSITIVE_INFINITY;
      return this.tombstoneNodeIds.slice(0, limit).map((id) => ({ id }));
    }
    if (text.startsWith("SELECT COUNT(*) AS count FROM nodes")) {
      return [{ count: this.tombstoneNodeIds.length }];
    }
    if (text.startsWith("DELETE FROM nodes WHERE node_id IN")) {
      this.events.push("db:tombstone-delete");
      this.deletedTombstoneIds.push(...params);
      const deletedSet = new Set(params);
      this.tombstoneNodeIds = this.tombstoneNodeIds.filter((id) => !deletedSet.has(id));
      return [];
    }
    if (text.startsWith("SELECT server_seq AS id FROM op_log")) {
      this.events.push("db:oplog-select");
      this.opLogCutoff = params[0] as Date;
      const limit = text.includes("LIMIT") ? Number(params[1]) : Number.POSITIVE_INFINITY;
      return this.opLogIds.slice(0, limit).map((id) => ({ id }));
    }
    if (text.startsWith("SELECT COUNT(*) AS count FROM op_log")) {
      return [{ count: this.opLogIds.length }];
    }
    if (text.startsWith("DELETE FROM op_log WHERE server_seq IN")) {
      this.events.push("db:oplog-delete");
      this.deletedOpLogIds.push(...params);
      const deletedSet = new Set(params);
      this.opLogIds = this.opLogIds.filter((id) => !deletedSet.has(id));
      return [];
    }
    return [];
  }
}

class GcSqliteTestDb implements CoreDb {
  select: CoreDb["select"];
  insert: CoreDb["insert"];
  update: CoreDb["update"];
  delete: CoreDb["delete"];
  events: string[] = [];
  chunkDeletes: string[] = [];
  chunks: Map<string, { refcount: number; createdAt: number }>;

  constructor(opts: { chunks?: Record<string, number> } = {}) {
    this.chunks = new Map(
      Object.entries(opts.chunks ?? {}).map(([hash, refcount]) => [hash, { refcount, createdAt: 0 }]),
    );
  }

  async all(query: unknown): Promise<Array<Record<string, unknown>>> {
    const { sql: text, params } = dialect.sqlToQuery(query as any);

    if (text.startsWith("SELECT chunk_hash FROM chunks")) {
      this.events.push("db:chunk-select");
      const limit = text.includes("LIMIT") ? Number(params[1]) : Number.POSITIVE_INFINITY;
      const cutoffMs = (params[0] as Date).getTime();
      const eligible = [...this.chunks.entries()]
        .filter(([, chunk]) => chunk.refcount === 0 && chunk.createdAt < cutoffMs)
        .map(([hash]) => hash);
      return eligible.slice(0, limit).map((chunkHash) => ({ chunk_hash: chunkHash }));
    }
    if (text.startsWith("SELECT COUNT(*) AS count FROM chunks")) {
      const cutoffMs = (params[0] as Date).getTime();
      const eligible = [...this.chunks.entries()].filter(
        ([, chunk]) => chunk.refcount === 0 && chunk.createdAt < cutoffMs,
      );
      return [{ count: eligible.length }];
    }
    if (text.startsWith("DELETE FROM chunks") && text.includes("RETURNING chunk_hash")) {
      const chunkHash = String(params[0]);
      const chunk = this.chunks.get(chunkHash);
      if (!chunk || chunk.refcount !== 0) {
        return [];
      }
      this.chunks.delete(chunkHash);
      this.chunkDeletes.push(chunkHash);
      this.events.push(`db:chunk-delete:${chunkHash}`);
      return [{ chunk_hash: chunkHash }];
    }
    if (text.startsWith("SELECT node_id AS id FROM nodes") || text.startsWith("SELECT server_seq AS id FROM op_log")) {
      return [];
    }
    if (text.startsWith("SELECT COUNT(*) AS count FROM nodes") || text.startsWith("SELECT COUNT(*) AS count FROM op_log")) {
      return [{ count: 0 }];
    }
    return [];
  }
}

function s3For(db: { events: string[] }, error?: Error) {
  return {
    fetch: vi.fn(async (url: string) => {
      const key = new URL(url).pathname.split("/").slice(-2).join("/");
      db.events.push(`s3:${key}`);
      if (error) {
        throw error;
      }
      return new Response(null, { status: 204 });
    }),
  } as any;
}

function s3ReturningStatus(db: { events: string[] }, status: number) {
  return {
    fetch: vi.fn(async (url: string) => {
      const key = new URL(url).pathname.split("/").slice(-2).join("/");
      db.events.push(`s3:${key}`);
      return new Response(null, { status });
    }),
  } as any;
}

function intervalOpts() {
  return {
    chunkGcIntervalMs: 100,
    chunkGracePeriodMs: 1_000,
    tombstonePurgeIntervalMs: 100,
    tombstoneRetentionMs: 1_000,
    opLogTruncationIntervalMs: 100,
    opLogRetentionMs: 1_000,
  };
}
