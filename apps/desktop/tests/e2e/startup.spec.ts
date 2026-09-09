import { expect, test } from "@playwright/test";

for (const desktop of [true, false]) {
  test(`${desktop ? "Desktop" : "Web"} observes startup and shows failure without business requests`, async ({ page }) => {
    const fixture = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!);
    let status = "starting", health_reads = 0, business_reads = 0;
    await page.route(`${fixture.base_url}/health`, async (route) => {
      health_reads++;
      await route.fulfill({ json: { status, stage: "database_migration", error: status === "unavailable" ? "migration_failed" : null, database_version: null, target_version: "0.25.1" }, headers: { "Access-Control-Allow-Origin": "http://localhost:1420", "Access-Control-Allow-Credentials": "true" } });
    });
    page.on("request", (request) => {
      if (request.method() !== "OPTIONS" && request.url().startsWith(fixture.base_url) && /\/(commands|events|session-materializations)(?:\?|$)/.test(request.url())) business_reads++;
    });
    await page.emulateMedia({ reducedMotion: "reduce" });
    if (desktop) {
      await page.addInitScript((bootstrap) => {
        Object.defineProperty(globalThis, "isTauri", { value: true });
        Object.defineProperty(globalThis, "__TAURI_EVENT_PLUGIN_INTERNALS__", { value: { unregisterListener() {} } });
        Object.defineProperty(globalThis, "__TAURI_INTERNALS__", { value: {
          transformCallback() { return 1; },
          async invoke(command: string) {
            if (command === "bootstrap_runtime" || command === "refresh_runtime_connection") return bootstrap;
            if (command === "connect_runtime_target") return { bootstrap, warning: null };
            if (command === "begin_runtime_connection") return "startup-binding";
            if (command === "load_desktop_preferences") return { left_sidebar_open: true, right_sidebar_open: true, expanded_workspace_ids: null };
            if (command === "desktop_platform") return "unsupported";
            return null;
          },
        } });
      }, fixture);
      await page.goto("/");
      await page.getByRole("button", { name: "进入工作空间" }).click();
    } else {
      const issued = await fetch(`${fixture.base_url}/auth/login`, { method: "POST", headers: { Authorization: `Bearer ${fixture.access_token}`, "Content-Type": "application/json" }, body: JSON.stringify({ method: "desktop" }) });
      expect(issued.ok).toBe(true);
      const { token } = await issued.json() as { token: string };
      await page.goto(`${fixture.base_url}/#token=${encodeURIComponent(token)}`);
    }
    await expect(desktop ? page.getByRole("button", { name: "正在升级数据库，请稍候…", exact: true }) : page.getByText("正在升级数据库，请稍候…", { exact: true })).toBeVisible();
    expect(business_reads).toBe(0);
    await page.screenshot({ path: test.info().outputPath(`${desktop ? "desktop" : "web"}-starting.png`) });
    status = "unavailable";
    await expect(page.getByText(/数据库升级失败，已停止后续升级/)).toBeVisible();
    const finished_reads = health_reads;
    await page.waitForTimeout(1600);
    expect(health_reads).toBe(finished_reads);
    expect(business_reads).toBe(0);
    await page.screenshot({ path: test.info().outputPath(`${desktop ? "desktop" : "web"}-unavailable.png`) });
  });
}
