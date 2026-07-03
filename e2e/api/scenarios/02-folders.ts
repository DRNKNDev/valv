import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { requestJson } from "./helpers.js";
import type { SeededHarness } from "./types.js";

export function folderScenarios(harness: SeededHarness): void {
  describe("folder API", () => {
    let ctx: Awaited<ReturnType<SeededHarness["createApp"]>>;

    beforeAll(async () => {
      ctx = await harness.createApp();
    });

    afterAll(async () => ctx?.cleanup());

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

      const root = await ctx.row<{ node_id: string }>("SELECT node_id FROM nodes WHERE folder_id = ? AND parent_id IS NULL", first.folder_id);
      expect(grant?.scope_node_id).toBe(root?.node_id);
    });

    it("rejects unauthenticated folder creation", async () => {
      const response = await ctx.app.request("/api/folders", {
        method: "POST",
        body: JSON.stringify({ name: "No Auth" }),
        headers: { "content-type": "application/json" },
      });
      expect(response.status).toBe(401);
    });

    it("GET /api/folders/:id returns the folder's name for an authorized principal", async () => {
      const created = await requestJson<{ folder_id: string }>(ctx.app, "/api/folders", {
        method: "POST",
        cookie: ctx.context.cookie,
        body: { name: "Design Docs" },
      });

      const folder = await requestJson<{ folder_id: string; name: string }>(ctx.app, `/api/folders/${created.folder_id}`, {
        cookie: ctx.context.cookie,
      });

      expect(folder).toEqual({ folder_id: created.folder_id, name: "Design Docs" });
    });

    it("GET /api/folders/:id returns 403 for a principal with no covering grant", async () => {
      const signup = await ctx.app.request("/api/auth/sign-up/email", {
        method: "POST",
        body: JSON.stringify({ name: "Outsider", email: "outsider-folders@example.com", password: "password1234" }),
        headers: { "content-type": "application/json" },
      });
      const outsiderCookie = signup.headers.get("set-cookie")?.split(";")[0];

      const response = await ctx.app.request(`/api/folders/${ctx.context.folderId}`, {
        headers: { cookie: outsiderCookie ?? "" },
      });

      expect(response.status).toBe(403);
    });

    it("GET /api/folders/:id returns 404 for an unknown folder_id", async () => {
      const response = await ctx.app.request("/api/folders/00000000-0000-0000-0000-000000000000", {
        headers: { cookie: ctx.context.cookie },
      });

      expect(response.status).toBe(404);
    });
  });
}
