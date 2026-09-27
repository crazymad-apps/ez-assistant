import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests/e2e',
  testMatch: 'web-notifications.spec.ts',
  globalSetup: './tests/e2e/runtime-host.setup.ts',
  workers: 1,
  use: { channel: 'chrome' },
});
