import { afterEach, describe, expect, it, vi } from "vitest";

import type { CoreDb } from "../auth/index.js";
import { startGc } from "./index.js";

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("GC service", () => {
  it("deletes the R2 object before deleting the chunk row", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-27T00:00:00.000Z"));
    const db = new GcTestDb({ chunks: ["old"] });
    const s3Client = s3For(db);

    startGc(db, s3Client, "bucket", intervalOpts());
    await vi.advanceTimersByTimeAsync(100);

    expect(db.events.indexOf("s3:chunks/old")).toBeLessThan(db.events.indexOf("db:chunk-delete:old"));
    expect(db.chunkDeletes).toEqual(["old"]);
  });

  it("leaves the chunk row when R2 deletion fails and logs the error", async () => {
    vi.useFakeTimers();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const db = new GcTestDb({ chunks: ["old"] });
    const s3Client = s3For(db, new Error("r2 unavailable"));

    startGc(db, s3Client, "bucket", intervalOpts());
    await vi.advanceTimersByTimeAsync(100);

    expect(db.chunkDeletes).toEqual([]);
    expect(consoleError).toHaveBeenCalledWith("Chunk GC failed", expect.objectContaining({ chunkHash: "old" }));
  });

  it("honors tombstone and op-log retention cutoffs", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-06-27T00:00:00.000Z"));
    const db = new GcTestDb();

    startGc(db, s3For(db), "bucket", {
      ...intervalOpts(),
      tombstoneRetentionMs: 10_000,
      opLogRetentionMs: 20_000,
    });
    await vi.advanceTimersByTimeAsync(100);

    expect(db.tombstoneCutoff?.toISOString()).toBe("2026-06-26T23:59:50.100Z");
    expect(db.opLogCutoff?.toISOString()).toBe("2026-06-26T23:59:40.100Z");
  });

  it("clears all timers when stopped", () => {
    vi.useFakeTimers();
    const stopGc = startGc(new GcTestDb(), s3For(new GcTestDb()), "bucket", intervalOpts());

    expect(vi.getTimerCount()).toBe(3);
    stopGc();

    expect(vi.getTimerCount()).toBe(0);
  });

  it("logs pass errors without rethrowing", async () => {
    vi.useFakeTimers();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const db = new GcTestDb({ failChunkSelect: true });

    startGc(db, s3For(db), "bucket", intervalOpts());
    await vi.advanceTimersByTimeAsync(100);

    expect(consoleError).toHaveBeenCalledWith("Chunk GC pass failed", expect.any(Error));
  });
});

class GcTestDb implements CoreDb {
  select: CoreDb["select"];
  insert: CoreDb["insert"];
  update: CoreDb["update"];
  delete: CoreDb["delete"];
  events: string[] = [];
  chunkDeletes: string[] = [];
  tombstoneCutoff?: Date;
  opLogCutoff?: Date;
  private chunks: string[];
  private failChunkSelect: boolean;

  constructor(opts: { chunks?: string[]; failChunkSelect?: boolean } = {}) {
    this.chunks = opts.chunks ?? [];
    this.failChunkSelect = opts.failChunkSelect ?? false;
  }

  async execute(query: any): Promise<Array<{ chunk_hash: string }>> {
    const text = query.queryChunks?.[0]?.value?.[0] as string | undefined;
    if (text?.includes("SELECT chunk_hash")) {
      this.events.push("db:chunk-select");
      if (this.failChunkSelect) {
        throw new Error("select failed");
      }
      return this.chunks.map((chunkHash) => ({ chunk_hash: chunkHash }));
    }
    if (text?.includes("DELETE FROM chunks")) {
      const chunkHash = String(query.queryChunks[1]);
      this.events.push(`db:chunk-delete:${chunkHash}`);
      this.chunkDeletes.push(chunkHash);
      return [];
    }
    if (text?.includes("DELETE FROM nodes")) {
      this.tombstoneCutoff = query.queryChunks[1];
      this.events.push("db:tombstone-delete");
      return [];
    }
    if (text?.includes("DELETE FROM op_log")) {
      this.opLogCutoff = query.queryChunks[1];
      this.events.push("db:oplog-delete");
      return [];
    }
    return [];
  }
}

function s3For(db: GcTestDb, error?: Error) {
  return {
    send: vi.fn(async (command: { input: { Key?: string } }) => {
      db.events.push(`s3:${command.input.Key}`);
      if (error) {
        throw error;
      }
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
