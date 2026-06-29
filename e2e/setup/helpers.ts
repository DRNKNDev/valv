import { randomUUID } from "node:crypto";

import type Database from "better-sqlite3";
import { generateSignedCookie } from "hono/cookie";

import { generateDeviceToken, sha256Hex, type CoreAuth } from "@valv/core";

export const authSecret = "12345678901234567890123456789012";

export type SeedContext = {
  cookie: string;
  userId: string;
  deviceId: string;
  token: string;
  folderId: string;
  rootNodeId: string;
};

type RequestApp = {
  request: (path: string, init?: RequestInit) => Response | Promise<Response>;
};

export async function seedContext(_db: CoreAuth["db"], sqlite: Database.Database): Promise<SeedContext> {
  const now = Date.now();
  const userId = randomUUID();
  const sessionToken = randomUUID();
  const deviceId = randomUUID();
  const token = generateDeviceToken();
  const folderId = randomUUID();
  const rootNodeId = randomUUID();
  const grantId = randomUUID();

  sqlite
    .prepare(`INSERT INTO "user" (id, name, email, emailVerified, image, createdAt, updatedAt) VALUES (?, ?, ?, ?, ?, ?, ?)`) 
    .run(userId, "E2E User", `${userId}@example.com`, 1, null, now, now);
  sqlite
    .prepare(`INSERT INTO "session" (id, token, userId, expiresAt, ipAddress, userAgent, createdAt, updatedAt) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`) 
    .run(randomUUID(), sessionToken, userId, now + 7 * 24 * 60 * 60 * 1000, "127.0.0.1", "e2e", now, now);
  sqlite
    .prepare(`INSERT INTO devices (device_id, user_id, name, token_hash, created_at) VALUES (?, ?, ?, ?, ?)`)
    .run(deviceId, userId, "E2E Device", sha256Hex(token), now);
  sqlite
    .prepare(`INSERT INTO shared_folders (folder_id, name, owner_user_id, created_at) VALUES (?, ?, ?, ?)`)
    .run(folderId, "E2E Folder", userId, now);
  sqlite
    .prepare(`INSERT INTO nodes (node_id, folder_id, parent_id, name, type, current_version_id, deleted_at, server_seq) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`)
    .run(rootNodeId, folderId, null, "", "folder", null, null, 0);
  sqlite
    .prepare(`INSERT INTO folder_grants (grant_id, folder_id, scope_node_id, user_id, device_id, role, can_read, can_write, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`)
    .run(grantId, folderId, rootNodeId, userId, null, "owner", 1, 1, now);

  const cookie = await generateSignedCookie("better-auth.session_token", sessionToken, authSecret, {
    httpOnly: true,
    path: "/",
  });

  return { cookie: cookie.split(";")[0] ?? cookie, userId, deviceId, token, folderId, rootNodeId };
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
