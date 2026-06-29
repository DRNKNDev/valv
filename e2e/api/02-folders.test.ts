import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { cleanupAppContext, createAppContext } from "../setup/api.js";
import { requestJson } from "../setup/helpers.js";

describe("folder API", () => {
  let ctx: Awaited<ReturnType<typeof createAppContext>>;

  beforeAll(async () => {
    ctx = await createAppContext();
  });

  afterAll(async () => cleanupAppContext(ctx));

  it("creates folders and lists owner grants rooted at the folder root", async () => {
    const first = await requestJson<{ folder_id: string }>(ctx.app, "/api/folders", {
      method: "POST",
      cookie: ctx.context.cookie,
      body: { name: "First Folder" },
    });
    const second = await requestJson<{ folder_id: string }>(ctx.app, "/api/folders", {
      method: "POST",
      cookie: ctx.context.cookie,
      body: { name: "Second Folder" },
    });
    expect(first.folder_id).not.toBe(second.folder_id);

    const grants = await requestJson<Array<{ folder_id: string; scope_node_id: string; role: string }>>(ctx.app, "/api/grants", {
      cookie: ctx.context.cookie,
    });
    const grant = grants.find((item) => item.folder_id === first.folder_id);
    expect(grant).toMatchObject({ role: "owner" });

    const root = ctx.sqlite
      .prepare("SELECT node_id FROM nodes WHERE folder_id = ? AND parent_id IS NULL")
      .get(first.folder_id) as { node_id: string };
    expect(grant?.scope_node_id).toBe(root.node_id);
  });

  it("rejects unauthenticated folder creation", async () => {
    const response = await ctx.app.request("/api/folders", {
      method: "POST",
      body: JSON.stringify({ name: "No Auth" }),
      headers: { "content-type": "application/json" },
    });
    expect(response.status).toBe(401);
  });
});
