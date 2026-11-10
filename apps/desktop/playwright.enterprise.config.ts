import { defineConfig } from "@playwright/test";
export default defineConfig({ testDir: "./tests/e2e", testMatch: ["web-enterprise.spec.ts", "web-enterprise-network.spec.ts"], workers: 1, timeout: 45_000 });
