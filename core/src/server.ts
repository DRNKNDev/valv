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

const databaseUrl = requiredEnv("DATABASE_URL");
const bucketName = requiredEnv("R2_BUCKET");

const db = createDb(databaseUrl);
const provider = isSqliteUrl(databaseUrl) ? "sqlite" : "pg";
const schema = provider === "sqlite" ? sqliteSchema : pgSchema;
const auth = createAuth(db, {
  secret: requiredEnv("AUTH_SECRET"),
  baseURL: process.env.APP_BASE_URL,
  provider,
  schema,
});
const s3Client = new S3Client({
  endpoint: requiredEnv("R2_ENDPOINT"),
  region: "auto",
  credentials: {
    accessKeyId: requiredEnv("R2_ACCESS_KEY_ID"),
    secretAccessKey: requiredEnv("R2_SECRET_ACCESS_KEY"),
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
    port: Number(process.env.PORT ?? 3000),
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
  const apiToken = process.env.CF_EMAIL_API_TOKEN;
  const from = process.env.CF_EMAIL_FROM;
  const appBaseUrl = process.env.APP_BASE_URL;
  if (!apiToken || !from || !appBaseUrl) {
    console.warn("Invite emails disabled: set CF_EMAIL_API_TOKEN, CF_EMAIL_FROM, and APP_BASE_URL to enable them.");
    return undefined;
  }
  return createSendInviteEmail({ apiToken, from, appBaseUrl });
}

function requiredEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`Missing required environment variable: ${name}`);
  }
  return value;
}
