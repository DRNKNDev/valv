import { existsSync, readFileSync } from "node:fs";

import { S3Client } from "@aws-sdk/client-s3";
import { serve } from "@hono/node-server";
import Database from "better-sqlite3";
import { drizzle as drizzleSqlite } from "drizzle-orm/better-sqlite3";
import { drizzle as drizzlePg } from "drizzle-orm/node-postgres";
import { Hono } from "hono";
import { Pool } from "pg";
import { WebSocketServer } from "ws";

import {
  createAuth,
  createBlobstoreRouter,
  createDeviceAuthRouter,
  createHub,
  createMetadataRouter,
  createRealtimeRouter,
  createSendInviteEmail,
  pgSchema,
  sqliteSchema,
  startGc,
  type CoreAuth,
} from "./index.js";

loadDotEnv(".env");

const databaseUrl = requiredEnv("VALV_DATABASE_URL");
const port = Number(process.env.VALV_PORT ?? 4747);
const appBaseUrl = process.env.VALV_BASE_URL ?? `http://localhost:${port}`;
const bucketName = requiredEnv("BUCKET_NAME");

const db = createDb(databaseUrl);
const provider = isSqliteUrl(databaseUrl) ? "sqlite" : "pg";
const schema = provider === "sqlite" ? sqliteSchema : pgSchema;
const auth = createAuth(db, {
  secret: requiredEnv("VALV_AUTH_SECRET"),
  baseURL: appBaseUrl,
  provider,
  schema,
});
const s3Client = new S3Client({
  endpoint: requiredEnv("BUCKET_ENDPOINT"),
  region: "auto",
  credentials: {
    accessKeyId: requiredEnv("BUCKET_ACCESS_KEY_ID"),
    secretAccessKey: requiredEnv("BUCKET_SECRET_ACCESS_KEY"),
  },
});
const hub = createHub();
const sendInviteEmail = maybeCreateSendInviteEmail();
const app = new Hono();

app.on(["POST", "GET"], "/api/auth/*", (ctx) => auth.handler(ctx.req.raw));
app.route("/auth", createDeviceAuthRouter(auth));
app.route("/api", createMetadataRouter({ auth, hub, sendInviteEmail }));
app.route("/api", createBlobstoreRouter({ auth, s3Client, bucketName }));
app.route("/ws", createRealtimeRouter({ auth, hub }));

startGc(auth.db, s3Client, bucketName);

serve(
  {
    fetch: app.fetch,
    port,
    websocket: { server: new WebSocketServer({ noServer: true }) },
  },
  (info) => {
    console.log(`valv core listening on ${info.address}:${info.port}`);
  },
);

function createDb(databaseUrl: string): CoreAuth["db"] {
  if (isSqliteUrl(databaseUrl)) {
    const sqlitePath = databaseUrl.startsWith("file:") ? databaseUrl.slice("file:".length) : databaseUrl;
    return drizzleSqlite(new Database(sqlitePath), { schema: sqliteSchema }) as CoreAuth["db"];
  }
  return drizzlePg(new Pool({ connectionString: databaseUrl }), { schema: pgSchema }) as CoreAuth["db"];
}

function isSqliteUrl(databaseUrl: string): boolean {
  return databaseUrl.startsWith("file:") || databaseUrl.endsWith(".db") || databaseUrl === ":memory:";
}

function maybeCreateSendInviteEmail() {
  const smtpPass = process.env.SMTP_PASS;
  const from = process.env.EMAIL_FROM;
  const smtpPort = process.env.SMTP_PORT === undefined ? undefined : Number(process.env.SMTP_PORT);
  if (!smtpPass || !from) {
    console.warn("Invite emails disabled: set SMTP_PASS and EMAIL_FROM to enable them.");
    return undefined;
  }
  return createSendInviteEmail({
    smtpHost: process.env.SMTP_HOST,
    smtpPort,
    smtpUser: process.env.SMTP_USER,
    smtpPass,
    from,
    appBaseUrl,
  });
}

function requiredEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`Missing required environment variable: ${name}`);
  }
  return value;
}

function loadDotEnv(path: string): void {
  if (!existsSync(path)) {
    return;
  }

  for (const line of readFileSync(path, "utf8").split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) {
      continue;
    }

    const separatorIndex = trimmed.indexOf("=");
    if (separatorIndex === -1) {
      continue;
    }

    const key = trimmed.slice(0, separatorIndex).trim();
    const rawValue = trimmed.slice(separatorIndex + 1).trim();
    process.env[key] ??= rawValue.replace(/^['"]|['"]$/g, "");
  }
}
