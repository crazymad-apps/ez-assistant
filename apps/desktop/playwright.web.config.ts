import { defineConfig } from "@playwright/test";

/** 直接访问 Host 内嵌页面，不启动 Vite，也不模拟 Tauri bridge。 */
export default defineConfig({
  testDir: "./tests/e2e",
  testMatch: ["web-host.spec.ts", "web-files.spec.ts", "web-terminal.spec.ts", "web-model-settings.spec.ts", "web-manual-model.spec.ts"],
  globalSetup: "./tests/e2e/runtime-host.setup.ts",
  workers: 1,
});
