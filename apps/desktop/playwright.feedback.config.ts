import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: './tests/e2e', testMatch: 'web-feedback.spec.ts',
  globalSetup: './tests/e2e/runtime-host.setup.ts', workers: 1,
  use: { channel: 'chrome', viewport: { width: 1280, height: 900 }, screenshot: 'only-on-failure' },
});
