import { defineConfig } from '@playwright/test';

process.env.EZ_ASSISTANT_E2E_MCP_APPS = '1';

export default defineConfig({
  testDir: './tests/e2e',
  testMatch: ['web-mcp-apps.spec.ts', 'web-mcp-apps-csp.spec.ts', 'web-message-cards.spec.ts', 'web-app-workspace.spec.ts'],
  globalSetup: './tests/e2e/runtime-host.setup.ts',
  workers: 1,
});
