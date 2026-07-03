import { gcScenarios } from "./scenarios/10-gc.js";
import { cleanupAppContext, createAppContext } from "../setup/api.js";

gcScenarios({
  createApp: async () => {
    const ctx = await createAppContext();
    return { ...ctx, cleanup: () => cleanupAppContext(ctx) };
  },
});
