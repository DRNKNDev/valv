import { folderScenarios } from "./scenarios/02-folders.js";
import { cleanupAppContext, createAppContext } from "../setup/api.js";

folderScenarios({
  createApp: async () => {
    const ctx = await createAppContext();
    return { ...ctx, cleanup: () => cleanupAppContext(ctx) };
  },
});
