import { randomUUID } from "node:crypto";

import type { RequestApp } from "./types.js";

export async function requestJson<T>(
  app: RequestApp,
  path: string,
  opts: { bearerToken?: string; cookie?: string; body?: unknown; method?: string; expectedStatus?: number } = {},
): Promise<T> {
  const response = await app.request(path, {
    method: opts.method ?? "GET",
    body: opts.body === undefined ? undefined : JSON.stringify(opts.body),
    headers: {
      ...(opts.body === undefined ? {} : { "content-type": "application/json" }),
      ...(opts.cookie ? { cookie: opts.cookie } : {}),
      ...(opts.bearerToken ? { authorization: `Bearer ${opts.bearerToken}` } : {}),
    },
  });
  const text = await response.text();
  if (response.status !== (opts.expectedStatus ?? 200)) {
    throw new Error(`Expected ${opts.expectedStatus ?? 200} for ${path}, got ${response.status}: ${text}`);
  }
  return text ? (JSON.parse(text) as T) : (undefined as T);
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

export async function uploadChunk(presignedUrl: string, bytes: Buffer): Promise<void> {
  const response = await fetch(presignedUrl, {
    method: "PUT",
    body: bytes as unknown as BodyInit,
    headers: { "content-type": "application/octet-stream" },
  });
  if (!response.ok) {
    throw new Error(`chunk upload failed: ${response.status} ${await response.text()}`);
  }
}
