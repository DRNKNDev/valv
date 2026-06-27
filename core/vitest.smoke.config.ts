import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["tests/smoke/**/*.smoke.ts"],
    pool: "forks",
  },
});
