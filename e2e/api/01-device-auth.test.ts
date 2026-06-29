import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { createTestBucket, deleteTestBucket } from "../setup/bucket.js";
import { requestJson } from "../setup/helpers.js";
import { createSmokeApp } from "../setup/server.js";

describe("device auth API", () => {
  let setup: Awaited<ReturnType<typeof createSmokeApp>>;
  let bucket = "";

  beforeAll(async () => {
    setup = await createSmokeApp("bootstrap");
    bucket = await createTestBucket(setup.s3);
    setup.cleanup();
    setup = await createSmokeApp(bucket);
  });

  afterAll(async () => {
    await deleteTestBucket(setup.s3, bucket);
    setup.cleanup();
  });

  it("signs up, registers a device, accepts the token, and rejects bad tokens", async () => {
    const signup = await setup.app.request("/api/auth/sign-up/email", {
      method: "POST",
      body: JSON.stringify({ name: "E2E User", email: "device-auth@example.com", password: "password1234" }),
      headers: { "content-type": "application/json" },
    });
    expect(signup.status).toBeLessThan(400);
    const cookie = signup.headers.get("set-cookie")?.split(";")[0];
    expect(cookie).toContain("better-auth.session_token");

    const device = await requestJson<{ device_id: string; token: string }>(setup.app, "/auth/device", {
      method: "POST",
      cookie,
      body: { name: "Test Mac" },
    });
    expect(device.device_id).toEqual(expect.any(String));
    expect(device.token).toEqual(expect.any(String));

    const grants = await requestJson<unknown[]>(setup.app, "/api/grants", { bearerToken: device.token });
    expect(grants).toEqual([]);

    const bad = await setup.app.request("/api/grants", { headers: { authorization: "Bearer bad-token" } });
    expect(bad.status).toBe(401);
  });
});
