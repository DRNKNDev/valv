import { inviteScenarios } from "./scenarios/07-invites.js";
import { cleanupAppContext, createAppContext } from "../setup/api.js";

inviteScenarios({
  createApp: async () => {
    const ctx = await createAppContext();
    return { ...ctx, cleanup: () => cleanupAppContext(ctx) };
  },
});
