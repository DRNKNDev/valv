import { describe, expect, it, vi } from "vitest";

import type { CoreAuth, CoreDb, Principal } from "../auth/index.js";
import { pgSchema } from "../db/schema.js";
import { createHub, createRealtimeRouter, type RealtimeSocket } from "./index.js";

const wsState = vi.hoisted(() => ({ handlers: undefined as any }));

vi.mock("@hono/node-server", () => ({
  upgradeWebSocket: vi.fn((factory: any) => (ctx: any) => {
    wsState.handlers = factory(ctx);
    return ctx.text("upgraded");
  }),
}));

describe("realtime hub", () => {
  it("fans out exact notifications only to open sockets in the target folder", () => {
    const hub = createHub();
    const target = socket();
    const sibling = socket();
    const closed = socket({ readyState: 3 });

    hub.subscribe("folder-1", target);
    hub.subscribe("folder-2", sibling);
    hub.subscribe("folder-1", closed);
    hub.notify("folder-1", 42);

    expect(target.send).toHaveBeenCalledWith(JSON.stringify({ folder_id: "folder-1", server_seq: 42 }));
    expect(sibling.send).not.toHaveBeenCalled();
    expect(closed.send).not.toHaveBeenCalled();

    closed.readyState = 1;
    hub.notify("folder-1", 43);
    expect(closed.send).not.toHaveBeenCalled();
  });

  it("unsubscribes sockets from every folder when the connection closes", () => {
    const hub = createHub();
    const ws = socket();

    hub.subscribe("folder-1", ws);
    hub.subscribe("folder-2", ws);
    ws.close();
    hub.notify("folder-1", 1);
    hub.notify("folder-2", 2);

    expect(ws.send).not.toHaveBeenCalled();
  });
});

describe("realtime router", () => {
  it("rejects unauthenticated upgrade requests", async () => {
    const response = await createRealtimeRouter({ hub: createHub(), auth: authFor(null) }).request("/ws");

    expect(response.status).toBe(401);
    await expect(response.json()).resolves.toEqual({ error: "unauthenticated" });
  });

  it("rejects unauthorized subscribe messages", async () => {
    const hub = { subscribe: vi.fn(), unsubscribe: vi.fn(), notify: vi.fn() };
    const app = createRealtimeRouter({ hub, auth: authFor({ type: "user", userId: "user-1" }, false) });
    const response = await app.request("/ws");
    const ws = socket();

    expect(response.status).toBe(200);
    wsState.handlers.onMessage({ data: JSON.stringify({ type: "subscribe", folder_id: "folder-1" }) }, ws);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(ws.send).toHaveBeenCalledWith(JSON.stringify({ error: "no_grant" }));
    expect(hub.subscribe).not.toHaveBeenCalled();
  });
});

function socket(opts: { readyState?: number } = {}) {
  let closeListener: (() => void) | undefined;
  const send = vi.fn((_data: string) => undefined);
  return {
    readyState: opts.readyState ?? 1,
    send,
    addEventListener: (_type, listener) => {
      closeListener = listener;
    },
    close: () => {
      closeListener?.();
    },
  } as RealtimeSocket & { send: typeof send; close: () => void };
}

class RealtimeTestDb implements CoreDb {
  select: CoreDb["select"];
  insert: CoreDb["insert"];
  update: CoreDb["update"];
  delete: CoreDb["delete"];

  constructor(private authorized: boolean) {}

  async getFolderRootForAuthz(folderId: string): Promise<string | undefined> {
    return folderId === "folder-1" ? "root-1" : undefined;
  }

  async getNodeForAuthz(nodeId: string): Promise<{ nodeId: string; folderId: string; parentId: string | null } | undefined> {
    return nodeId === "root-1" ? { nodeId, folderId: "folder-1", parentId: null } : undefined;
  }

  async getGrantForAuthz(): Promise<{ grantId: string; scopeNodeId: string; canRead: boolean; canWrite: boolean } | undefined> {
    return this.authorized ? { grantId: "grant-1", scopeNodeId: "root-1", canRead: true, canWrite: false } : undefined;
  }
}

function authFor(principal: Principal | null, authorized = true): CoreAuth {
  return {
    db: new RealtimeTestDb(authorized),
    schema: pgSchema,
    api: {
      getSession: async () => (principal?.type === "user" ? { user: { id: principal.userId } } : null),
    },
  } as unknown as CoreAuth;
}
