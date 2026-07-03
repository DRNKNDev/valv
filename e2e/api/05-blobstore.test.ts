import { blobstoreScenarios } from "./scenarios/05-blobstore.js";
import { cleanupAppContext, createAppContext } from "../setup/api.js";

blobstoreScenarios({
  createApp: async () => {
    const ctx = await createAppContext();
    return { ...ctx, cleanup: () => cleanupAppContext(ctx) };
  },
});
