import { grantScenarios } from "./scenarios/06-grants.js";
import { cleanupAppContext, createAppContext } from "../setup/api.js";

grantScenarios({
  createApp: async () => {
    const ctx = await createAppContext();
    return { ...ctx, cleanup: () => cleanupAppContext(ctx) };
  },
});
