import { describe, expect, it, vi } from "vitest";

import { pgSchema } from "../db/schema.js";
import { createDeviceAuthRouter } from "./device.js";
import { sha256Hex, type CoreAuth, type CoreDb } from "./index.js";

describe("device auth routes", () => {
  it("registers a human user's device and persists only the token hash", async () => {
    const db = new DeviceTestDb();
    const app = createDeviceAuthRouter(authFor(db, { userId: "user-1" }));

    const response = await app.request("/device", {
      method: "POST",
      body: JSON.stringify({ name: "MacBook" }),
      headers: { "content-type": "application/json" },
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body.device_id).toEqual(expect.any(String));
    expect(body.token).toEqual(expect.any(String));
    expect(db.insertedDevices).toHaveLength(1);
    expect(db.insertedDevices[0]).toMatchObject({ userId: "user-1", name: "MacBook" });
    expect(db.insertedDevices[0]?.tokenHash).toBe(sha256Hex(body.token));
    expect(db.insertedDevices[0]?.tokenHash).not.toBe(body.token);
  });

  it("uses a default device name when none is provided", async () => {
    const db = new DeviceTestDb();
    const app = createDeviceAuthRouter(authFor(db, { userId: "user-1" }));

    const response = await app.request("/device", { method: "POST" });

    expect(response.status).toBe(200);
    expect(db.insertedDevices[0]).toMatchObject({ name: "Device" });
  });

  it("rejects device principals", async () => {
    const response = await createDeviceAuthRouter(authFor(new DeviceTestDb("device-1"), undefined)).request("/device", {
      method: "POST",
      headers: { authorization: "Bearer device-token" },
    });

    expect(response.status).toBe(403);
    await expect(response.json()).resolves.toEqual({ error: "forbidden" });
  });
});

class DeviceTestDb implements CoreDb {
  update: CoreDb["update"];
  delete: CoreDb["delete"];
  execute: CoreDb["execute"];
  insertedDevices: Array<Record<string, string>> = [];

  constructor(private readonly deviceId?: string) {}

  select(): any {
    return {
      from: () => ({
        where: () => ({
          limit: async () => (this.deviceId ? [{ deviceId: this.deviceId }] : []),
        }),
      }),
    };
  }

  insert(): any {
    return {
      values: async (value: Record<string, string>) => {
        this.insertedDevices.push(value);
      },
    };
  }
}

function authFor(db: DeviceTestDb, session?: { userId: string }): CoreAuth {
  return {
    db,
    schema: pgSchema,
    api: {
      getSession: vi.fn(async () => (session ? { user: { id: session.userId } } : null)),
    },
  } as unknown as CoreAuth;
}
