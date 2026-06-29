import { existsSync, readFileSync } from "node:fs";

import { defineConfig } from "drizzle-kit";

const databaseUrl = process.env.VALV_DATABASE_URL ?? readDotEnv(".env").VALV_DATABASE_URL;
if (!databaseUrl) {
  throw new Error("Missing VALV_DATABASE_URL. Set it in the environment or oss/core/.env.");
}

export default defineConfig({
  dialect: "sqlite",
  schema: "./src/db/schema.sqlite.ts",
  out: "./src/db/migrations/sqlite",
  dbCredentials: { url: databaseUrl },
});

function readDotEnv(path: string): Record<string, string> {
  if (!existsSync(path)) {
    return {};
  }

  return Object.fromEntries(
    readFileSync(path, "utf8")
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line && !line.startsWith("#") && line.includes("="))
      .map((line) => {
        const separatorIndex = line.indexOf("=");
        const key = line.slice(0, separatorIndex).trim();
        const rawValue = line.slice(separatorIndex + 1).trim();
        return [key, rawValue.replace(/^['"]|['"]$/g, "")];
      }),
  );
}
