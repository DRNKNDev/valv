import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { serve } from "@hono/node-server";
import { WebSocketServer } from "ws";
import WebSocket from "ws";

import { cleanupAppContext, createAppContext, createNode } from "../setup/api.js";
import { requestJson } from "../setup/helpers.js";

describe("realtime API", () => {
  let ctx: Awaited<ReturnType<typeof createAppContext>>;
  let server: { close: () => void } | undefined;
  let port = 0;

  beforeAll(async () => {
    ctx = await createAppContext();
    await new Promise<void>((resolve) => {
      let listeningServer: { close: () => void };
      listeningServer = serve(
        { fetch: ctx.app.fetch, port: 0, websocket: { server: new WebSocketServer({ noServer: true }) } },
        (info: { port: number }) => {
          port = info.port;
          server = listeningServer;
          resolve();
        },
      );
    });
  });

  afterAll(async () => {
    server?.close();
    await cleanupAppContext(ctx);
  });

  it("pushes committed ops to subscribed devices within 2s", async () => {
    const ws = await connect(ctx.context.token);
    ws.send(JSON.stringify({ type: "subscribe", folder_id: ctx.context.folderId }));
    await new Promise((resolve) => setTimeout(resolve, 50));

    const message = nextMessage(ws, 2000);
    const created = await createNode(ctx.app, ctx.context.folderId, ctx.context.token, ctx.context.rootNodeId, "push.txt", "file");
    await expect(message).resolves.toMatchObject({ folder_id: ctx.context.folderId, server_seq: created.server_seq });
    ws.close();
  });

  it("does not push sibling folder ops to a different subscription", async () => {
    const otherFolder = await requestJson<{ folder_id: string }>(ctx.app, "/api/folders", {
      method: "POST",
      cookie: ctx.context.cookie,
      body: { name: "Other Realtime Folder" },
    });
    const otherRoot = ctx.sqlite
      .prepare("SELECT node_id FROM nodes WHERE folder_id = ? AND parent_id IS NULL")
      .get(otherFolder.folder_id) as { node_id: string };
    const ws = await connect(ctx.context.token);
    ws.send(JSON.stringify({ type: "subscribe", folder_id: ctx.context.folderId }));
    await new Promise((resolve) => setTimeout(resolve, 50));

    await createNode(ctx.app, otherFolder.folder_id, ctx.context.token, otherRoot.node_id, "nested.txt", "file");
    await expect(noMessage(ws, 300)).resolves.toBe(true);
    ws.close();
  });

  async function connect(token: string): Promise<WebSocket> {
    const ws = new WebSocket(`ws://127.0.0.1:${port}/ws?token=${encodeURIComponent(token)}`);
    await new Promise<void>((resolve, reject) => {
      ws.once("open", resolve);
      ws.once("error", reject);
    });
    return ws;
  }
});

function nextMessage(ws: WebSocket, timeoutMs: number): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("timed out waiting for websocket message")), timeoutMs);
    ws.once("message", (data) => {
      clearTimeout(timer);
      resolve(JSON.parse(data.toString()) as Record<string, unknown>);
    });
  });
}

function noMessage(ws: WebSocket, timeoutMs: number): Promise<boolean> {
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve(true), timeoutMs);
    ws.once("message", () => {
      clearTimeout(timer);
      resolve(false);
    });
  });
}
