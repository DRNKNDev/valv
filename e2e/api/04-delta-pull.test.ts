import { deltaPullScenarios } from "./scenarios/04-delta-pull.js";
import { cleanupAppContext, createAppContext } from "../setup/api.js";

deltaPullScenarios({
  createApp: async () => {
    const ctx = await createAppContext();
    return { ...ctx, cleanup: () => cleanupAppContext(ctx) };
  },
});
