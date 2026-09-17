import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/e2e",
  testMatch: [
    "control-sizing.spec.ts",
    "model-management.spec.ts",
    "shell.spec.ts",
    "startup.spec.ts",
  ],
  globalSetup: "./tests/e2e/runtime-host.setup.ts",
  // 全套端到端用例共享 globalSetup 创建的同一隔离 Runtime，且会修改服务商、
  // 默认模型和工作空间等权威状态；串行执行才能保证用例之间不会并发污染。
  workers: 1,
  use: {
    baseURL: "http://localhost:1421",
  },
  webServer: {
    command: "npm run dev -- --host 127.0.0.1 --port 1421 --strictPort",
    url: "http://127.0.0.1:1421",
    reuseExistingServer: false,
  },
});
