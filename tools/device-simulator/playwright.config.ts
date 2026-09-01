import { defineConfig } from "@playwright/test";

const port = 57_679;

export default defineConfig({
  testDir: "tests/browser",
  timeout: 15_000,
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    trace: "retain-on-failure",
  },
  webServer: {
    command: `node dist/node/main.js --home /tmp/ez-assistant-device-simulator-playwright --port ${port}`,
    url: `http://127.0.0.1:${port}`,
    reuseExistingServer: false,
    timeout: 15_000,
  },
});
