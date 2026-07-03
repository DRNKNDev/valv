import { deviceAuthScenarios } from "./scenarios/01-device-auth.js";
import { createBareApp } from "../setup/api.js";

deviceAuthScenarios({ createApp: createBareApp });
