import { defineConfig } from "vitest/config";
import { version as app_version } from "./package.json" with { type: "json" };
import quick_actions_fixture from "./tests/fixtures/quick-actions.json" with { type: "json" };

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify(app_version),
    __QUICK_ACTIONS_CONFIG__: JSON.stringify(quick_actions_fixture),
  },
  test: {
    environment: "jsdom",
    include: ["tests/unit/**/*.test.ts", "tests/unit/**/*.test.tsx"],
    setupFiles: ["./tests/support/setup.ts"],
  },
});
