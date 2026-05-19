import { defineConfig } from "@playwright/test";

/**
 * Playwright configuration for the PokeDumpster intents UI testing framework.
 *
 * The intents runner discovers `intents/*.yaml` and runs each scenario either
 * via deterministic replay (an `implementations/*.ts` module exists) or via the
 * Claude Vision harness. Browser launch options mirror DeckDumpster's conftest:
 * headless Chromium that accepts self-signed certs, 1280x900 viewport.
 */
export default defineConfig({
  testDir: ".",
  testMatch: ["test_scenarios.ts"],
  fullyParallel: false,
  workers: 1,
  timeout: 120_000,
  reporter: [["list"]],
  use: {
    viewport: { width: 1280, height: 900 },
    ignoreHTTPSErrors: true,
    launchOptions: {
      args: ["--ignore-certificate-errors"],
    },
  },
  projects: [
    {
      name: "chromium",
      use: { browserName: "chromium" },
    },
  ],
});
