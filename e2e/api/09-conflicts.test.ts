import { conflictScenarios } from "./scenarios/09-conflicts.js";
import { cleanupAppContext, createAppContext } from "../setup/api.js";

conflictScenarios({
  createApp: async () => {
    const ctx = await createAppContext();
    return { ...ctx, cleanup: () => cleanupAppContext(ctx) };
  },
});
