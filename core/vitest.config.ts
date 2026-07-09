import { resolve } from "node:path";

import { defineConfig } from "vitest/config";

export default defineConfig({
  resolve: {
    alias: {
      "@valv/contracts-sync": resolve(__dirname, "../contracts/sync/src/index.ts"),
    },
  },
});
