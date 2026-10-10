import { defineConfig } from "@playwright/test";
export default defineConfig({ testDir: "./tests/e2e", testMatch: "web-feedback.spec.ts", workers: 1, timeout: 60_000 });
