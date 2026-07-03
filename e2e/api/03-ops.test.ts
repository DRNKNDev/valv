import { opScenarios } from "./scenarios/03-ops.js";
import { cleanupAppContext, createAppContext } from "../setup/api.js";

opScenarios({
  createApp: async () => {
    const ctx = await createAppContext();
    return { ...ctx, cleanup: () => cleanupAppContext(ctx) };
  },
});
