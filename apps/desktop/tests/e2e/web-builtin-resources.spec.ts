import { compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test } from "@playwright/test";
import { expectBuiltinMcpAcceptance } from "./mcp-acceptance";

for (const desktop of [false, true]) {
  test(`F05 ${desktop ? "Desktop UI" : "Host Web"} 配置内置 MCP 和 Skill`, async ({ page }) => {
    const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!);
    if (desktop) {
      // 只替换平台桥接，业务 HTTP／SSE 和页面使用同一个正式 Host。
      await page.addInitScript((bootstrap) => {
        Object.defineProperty(globalThis, "isTauri", { value: true });
        Object.defineProperty(globalThis, "__TAURI_EVENT_PLUGIN_INTERNALS__", { value: { unregisterListener() {} } });
        Object.defineProperty(globalThis, "__TAURI_INTERNALS__", { value: {
          transformCallback() { return 1; },
          async invoke(command: string) {
            if (["bootstrap_runtime", "refresh_runtime_connection"].includes(command)) return bootstrap;
            if (command === "connect_runtime_target") return { bootstrap, warning: null };
            if (command === "begin_runtime_connection") return "f05-binding";
            if (command === "desktop_platform") return "unsupported";
            if (command === "load_desktop_preferences") return {
              left_sidebar_open: true, right_sidebar_open: true, left_sidebar_width: 286, right_sidebar_width: 380,
              expanded_workspace_ids: null, close_behavior: "hide_to_tray", default_approval_mode: "auto", last_model_selection: null,
            };
            return null;
          },
        } });
      }, host);
      await page.goto(host.base_url);
      await page.getByRole("button", { name: "进入项目", exact: true }).click();
    } else {
      const response = await fetch(`${host.base_url}/auth/login`, {
        method: "POST",
        headers: { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" },
        body: JSON.stringify({ method: "desktop" }),
      });
      expect(response.ok).toBe(true);
      const { token } = await response.json();
      await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
    }
    await expect(page.getByRole("button", { name: "账号与 Runtime" })).toBeVisible();
    await expectBuiltinMcpAcceptance(page);
  });
}
