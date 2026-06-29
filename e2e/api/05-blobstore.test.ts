import { randomUUID } from "node:crypto";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { cleanupAppContext, createAppContext, createNode, row, submitOp } from "../setup/api.js";
import { requestJson, uploadChunk } from "../setup/helpers.js";

describe("blobstore API", () => {
  let ctx: Awaited<ReturnType<typeof createAppContext>>;

  beforeAll(async () => {
    ctx = await createAppContext();
  });

  afterAll(async () => cleanupAppContext(ctx));

  it("coordinates chunk upload, dedupes referenced chunks, and accepts uploaded versions", async () => {
    const empty = await requestJson<{ objects: unknown[] }>(ctx.app, "/api/objects/batch", {
      bearerToken: ctx.context.token,
      method: "POST",
      body: { operation: "upload", objects: [] },
    });
    expect(empty.objects).toEqual([]);

    const hash = `chunk-${randomUUID()}`;
    const staged = await requestJson<{ objects: Array<{ already_exists: boolean; actions?: { upload?: { href: string } } }> }>(ctx.app, "/api/objects/batch", {
      bearerToken: ctx.context.token,
      method: "POST",
      body: { operation: "upload", objects: [{ oid: hash, size: 11 }] },
    });
    expect(staged.objects[0]?.already_exists).toBe(false);
    expect(staged.objects[0]?.actions?.upload?.href).toEqual(expect.any(String));
    await uploadChunk(staged.objects[0]!.actions!.upload!.href, Buffer.from("hello world"));

    const file = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "blob.txt", "file");
    const version = await submitOp<{ result: string }>(ctx.app, ctx.context.folderId, ctx.context.token, {
      op_type: "new_version",
      node_id: file.nodeId,
      based_on_seq: file.server_seq,
      payload: {
        version_id: randomUUID(),
        content_hash: hash,
        size_bytes: 11,
        manifest: [{ chunk_hash: hash, offset: 0, length: 11 }],
      },
    });
    expect(version.result).toBe("applied");
    expect(row<{ refcount: number }>(ctx.sqlite, "SELECT refcount FROM chunks WHERE chunk_hash = ?", hash)?.refcount).toBe(1);

    const duplicate = await requestJson<{ objects: Array<{ already_exists: boolean; actions?: unknown }> }>(ctx.app, "/api/objects/batch", {
      bearerToken: ctx.context.token,
      method: "POST",
      body: { operation: "upload", objects: [{ oid: hash, size: 11 }] },
    });
    expect(duplicate.objects[0]).toMatchObject({ already_exists: true });
    expect(duplicate.objects[0]?.actions).toBeUndefined();
  });
});
