import { describe, expect, it } from "vitest";
import { PROTOCOL_HEADER } from "@valv/contracts-sync";

import { LifecycleDb, metadataAppFor } from "../../tests/support.js";

describe("metadata protocol version middleware", () => {
  it("rejects a gated request below the configured floor", async () => {
    const app = metadataAppFor(new LifecycleDb(), { type: "device", deviceId: "device-1" }, { minProtocolVersion: 2 });

    const response = await app.request("/folders/folder-1/ops?since=0", {
      headers: { [PROTOCOL_HEADER]: "1" },
    });

    expect(response.status).toBe(426);
    await expect(response.json()).resolves.toMatchObject({
      error: "protocol_too_old",
      min_protocol: 2,
      message: expect.any(String),
    });
  });

  it("rejects a gated request with no header once a floor is configured", async () => {
    const app = metadataAppFor(new LifecycleDb(), { type: "device", deviceId: "device-1" }, { minProtocolVersion: 1 });

    const response = await app.request("/folders/folder-1/tree");

    expect(response.status).toBe(426);
  });

  it("passes gated requests through at or above the configured floor", async () => {
    const app = metadataAppFor(new LifecycleDb(), { type: "device", deviceId: "device-1" }, { minProtocolVersion: 2 });

    const response = await app.request("/folders/folder-1/ops?since=0", {
      headers: { [PROTOCOL_HEADER]: "2" },
    });

    expect(response.status).not.toBe(426);
  });

  it("does not gate requests when no floor is configured", async () => {
    const app = metadataAppFor(new LifecycleDb(), { type: "device", deviceId: "device-1" });

    const response = await app.request("/folders/folder-1/ops?since=0");

    expect(response.status).not.toBe(426);
  });

  it("does not gate unrelated grant routes", async () => {
    const db = new LifecycleDb();
    db.authorizedScopes.add("root");
    const app = metadataAppFor(db, { type: "user", userId: "user-1" }, { minProtocolVersion: 99 });

    const response = await app.request("/folders/folder-1/grants", {
      method: "POST",
      body: JSON.stringify({ name: "Agent" }),
      headers: { "content-type": "application/json", [PROTOCOL_HEADER]: "1" },
    });

    expect(response.status).toBe(200);
    expect(db.folderGrants[0]).toMatchObject({
      folderId: "folder-1",
      scopeNodeId: "root",
      deviceId: expect.any(String),
    });
  });
});
