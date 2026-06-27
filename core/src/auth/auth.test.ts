import { describe, expect, it, vi } from "vitest";

import { pgSchema } from "../db/schema.js";
import { authenticateRequest, sha256Hex, type CoreAuth, type CoreDb } from "./index.js";

describe("auth helpers", () => {
  it("hashes device tokens with SHA-256 hex", () => {
    expect(sha256Hex("raw-token")).toBe("34d328009b123fbbb0dc93f18b3e6de1ecf7b1a5783c33dff7ffe1926f09e943");
  });

  it("resolves a bearer device token before Better Auth session fallback", async () => {
    const db = new AuthTestDb([{ deviceId: "device-1" }]);
    const auth = authFor(db, { userId: "user-1" });
    const ctx = contextFor({ authorization: "Bearer raw-token" });

    const result = await authenticateRequest(ctx as any, auth);

    expect(result).toEqual({ type: "device", deviceId: "device-1" });
    expect(db.selectCount).toBe(1);
    expect(auth.api.getSession).not.toHaveBeenCalled();
  });

  it("returns unauthenticated when no auth dependency is available", async () => {
    const result = await authenticateRequest(contextFor({}) as any, undefined);

    expect(result).toEqual({ type: "unauthenticated" });
  });

});

class AuthTestDb implements CoreDb {
  update: CoreDb["update"];
  delete: CoreDb["delete"];
  execute: CoreDb["execute"];
  selectCount = 0;
  insertedDevices: Array<Record<string, string>> = [];

  constructor(private readonly deviceRows: Array<{ deviceId: string }>) {}

  select(): any {
    this.selectCount += 1;
    return {
      from: () => ({
        where: () => ({
          limit: async () => this.deviceRows,
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

function authFor(db: AuthTestDb, session: { userId: string } | undefined): CoreAuth {
  const getSession = vi.fn(async () => (session ? { user: { id: session.userId } } : null));
  return {
    db,
    schema: pgSchema,
    api: { getSession },
  } as unknown as CoreAuth;
}

function contextFor(headers: Record<string, string>) {
  const stored = new Map<string, unknown>();
  return {
    var: {},
    set: (key: string, value: unknown) => stored.set(key, value),
    req: {
      header: (name: string) => headers[name.toLowerCase()],
      query: () => undefined,
      raw: { headers: new Headers(headers) },
    },
  };
}
