import { Hono } from "hono";
import { upgradeWebSocket, type WebSocketLike } from "@hono/node-server";
import type { WSContext } from "hono/ws";

import { authenticateRequest, type AuthVariables, type CoreAuth } from "../auth/index.js";
import { checkGrant } from "../metadata/authz.js";
import { getFolderRoot } from "../metadata/common.js";

export type RealtimeSocket = {
  readyState: number;
  send: (data: string) => void;
  addEventListener?: (type: "close", listener: () => void) => void;
  on?: (type: "close", listener: () => void) => void;
};

export type Hub = {
  subscribe: (folderId: string, ws: RealtimeSocket) => void;
  unsubscribe: (folderId: string, ws: RealtimeSocket) => void;
  notify: (folderId: string, serverSeq: number) => void;
};

const OPEN_READY_STATE = 1;

export function createHub(): Hub {
  const subscriptions = new Map<string, Set<RealtimeSocket>>();
  const socketFolders = new WeakMap<RealtimeSocket, Set<string>>();

  const unsubscribe = (folderId: string, ws: RealtimeSocket) => {
    subscriptions.get(folderId)?.delete(ws);
    socketFolders.get(ws)?.delete(folderId);
  };

  const unsubscribeAll = (ws: RealtimeSocket) => {
    const folders = socketFolders.get(ws);
    if (!folders) {
      return;
    }
    for (const folderId of folders) {
      subscriptions.get(folderId)?.delete(ws);
    }
    folders.clear();
  };

  return {
    subscribe(folderId, ws) {
      let sockets = subscriptions.get(folderId);
      if (!sockets) {
        sockets = new Set();
        subscriptions.set(folderId, sockets);
      }
      sockets.add(ws);
      let folders = socketFolders.get(ws);
      if (!folders) {
        folders = new Set();
        socketFolders.set(ws, folders);
        ws.addEventListener?.("close", () => unsubscribeAll(ws));
        ws.on?.("close", () => unsubscribeAll(ws));
      }
      folders.add(folderId);
    },
    unsubscribe,
    notify(folderId, serverSeq) {
      const payload = JSON.stringify({ folder_id: folderId, server_seq: serverSeq });
      for (const ws of subscriptions.get(folderId) ?? []) {
        if (ws.readyState === OPEN_READY_STATE) {
          ws.send(payload);
        } else {
          unsubscribe(folderId, ws);
        }
      }
    },
  };
}

export function createRealtimeRouter(opts: { hub: Hub; auth: CoreAuth }): Hono<{ Variables: AuthVariables }> {
  const router = new Hono<{ Variables: AuthVariables }>();
  router.get("/ws", async (ctx, next) => {
    const principal = await authenticateRequest(ctx, opts.auth);
    if (principal.type === "unauthenticated") {
      return ctx.json({ error: "unauthenticated" }, 401);
    }
    ctx.set("principal", principal);
    return upgradeWebSocket(() => {
      const folders = new Set<string>();
      return {
        onMessage(event, ws) {
          void (async () => {
            const message = parseMessage(event.data);
            if (message?.type !== "subscribe") {
              return;
            }
            const rootNodeId = await getFolderRoot(opts.auth, message.folder_id);
            if (!rootNodeId) {
              ws.send(JSON.stringify({ error: "folder_not_found" }));
              return;
            }
            const grant = await checkGrant(opts.auth.db, rootNodeId, principal, "read", opts.auth.schema);
            if (!grant.granted) {
              ws.send(JSON.stringify({ error: grant.reason }));
              return;
            }
            opts.hub.subscribe(message.folder_id, wsContextSocket(ws));
            folders.add(message.folder_id);
          })();
        },
        onClose(_, ws) {
          const socket = wsContextSocket(ws);
          for (const folderId of folders) {
            opts.hub.unsubscribe(folderId, socket);
          }
          folders.clear();
        },
      };
    })(ctx, next);
  });
  return router;
}

function parseMessage(data: unknown): { type: "subscribe"; folder_id: string } | undefined {
  if (typeof data !== "string") {
    return undefined;
  }
  try {
    const parsed = JSON.parse(data) as { type?: unknown; folder_id?: unknown };
    if (parsed.type === "subscribe" && typeof parsed.folder_id === "string") {
      return { type: "subscribe", folder_id: parsed.folder_id };
    }
  } catch {
    return undefined;
  }
  return undefined;
}

function wsContextSocket(ws: WSContext<WebSocketLike>): RealtimeSocket {
  return ws as unknown as RealtimeSocket;
}
