import path from "node:path";

import { defineConfig } from "vitest/config";

export default defineConfig({
  resolve: {
    alias: [
      { find: "@valv/core", replacement: path.resolve("../core/src/index.ts") },
      { find: "@valv/contracts-sync", replacement: path.resolve("../contracts/sync/src/index.ts") },
      { find: /^@hono\/(.*)$/, replacement: path.resolve("../core/node_modules/@hono") + "/$1" },
      { find: "aws4fetch", replacement: path.resolve("../core/node_modules/aws4fetch/dist/aws4fetch.esm.mjs") },
      { find: "better-auth/adapters/drizzle", replacement: path.resolve("../core/node_modules/better-auth/dist/adapters/drizzle-adapter/index.mjs") },
      { find: "better-auth", replacement: path.resolve("../core/node_modules/better-auth/dist/index.mjs") },
      { find: "better-sqlite3", replacement: path.resolve("../core/node_modules/better-sqlite3") },
      { find: /^drizzle-orm(\/.*)?$/, replacement: path.resolve("../core/node_modules/drizzle-orm") + "$1" },
      { find: "hono/cookie", replacement: path.resolve("../core/node_modules/hono/dist/helper/cookie/index.js") },
      { find: "hono/factory", replacement: path.resolve("../core/node_modules/hono/dist/helper/factory/index.js") },
      { find: "hono/ws", replacement: path.resolve("../core/node_modules/hono/dist/helper/websocket/index.js") },
      { find: "hono", replacement: path.resolve("../core/node_modules/hono/dist/index.js") },
      { find: "nodemailer", replacement: path.resolve("../core/node_modules/nodemailer") },
      { find: "pg", replacement: path.resolve("../core/node_modules/pg") },
      { find: "ws", replacement: path.resolve("../core/node_modules/ws") },
      { find: "zod", replacement: path.resolve("../core/node_modules/zod") },
    ],
  },
  test: {
    pool: "forks",
  },
});
