import { compatibilityHeaders, type AgentShellSettings } from "@ez-assistant/protocol";
import { expect, test } from "@playwright/test";
import { shellLabels } from "../../src/runtime-client/shellPresentation";

test.use({ channel: process.platform === "win32" ? "msedge" : undefined, screenshot: "only-on-failure" });

test("Windows Shell settings, session switch and terminal types use the real Host", async ({ page }, info) => {
  test.skip(process.platform !== "win32", "Windows Shell matrix");
  test.setTimeout(120_000);
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" };
  const response = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  expect(response.ok).toBe(true);
  const { token } = await response.json() as { token: string };
  const catalog_response = await fetch(`${host.base_url}/commands`, { method: "POST", headers, body: JSON.stringify({
    request_id: "shell-catalog", command: { scope: "runtime", payload: { type: "get_agent_shell_settings", payload: {} } },
  }) });
  expect(catalog_response.ok).toBe(true);
  const catalog = (await catalog_response.json() as { result: { payload: { payload: AgentShellSettings } } }).result.payload.payload;
  const errors: string[] = [];
  page.on("pageerror", (error) => { errors.push(error.message); console.error("Shell page error:", error.message); });
  page.on("response", (response) => { if (response.status() >= 400) console.error("Shell page HTTP:", response.status(), new URL(response.url()).pathname); });
  await page.setViewportSize({ width: 1280, height: 900 });
  const document = await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  expect(document?.status()).toBe(200);
  await expect(page.locator("[data-app-title-bar]")).toBeVisible();
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await page.getByRole("button", { name: /^状态与诊断/ }).click();
  await expect(page.getByText("Windows · x64", { exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "平台依赖", exact: true })).toBeVisible();
  await expect(page.getByText("rg", { exact: true })).toBeVisible();
  await expect(page.getByText("Git Bash", { exact: true })).toBeVisible();
  await page.screenshot({ path: info.outputPath("windows-dependencies.png"), animations: "disabled" });
  await page.getByRole("button", { name: "返回 Runtime", exact: true }).click();
  const setting = page.getByRole("button", { name: "默认 Agent Shell", exact: true });
  await expect(setting).toHaveText("Windows PowerShell 5.1");
  await setting.click();
  await page.getByRole("option", { name: "CMD", exact: true }).click();
  await expect(setting).toHaveText("CMD");
  await page.screenshot({ path: info.outputPath("shell-default-settings.png"), animations: "disabled" });
  await page.keyboard.press("Escape");
  await page.getByText("M2 临时会话", { exact: true }).first().click();
  const binding = page.getByRole("button", { name: "切换会话 Agent Shell" });
  await expect(binding).toHaveText("Windows PowerShell 5.1");
  const refresh = page.getByRole("button", { name: "刷新 Shell 环境", exact: true });
  await expect(refresh).toBeVisible();
  await refresh.click();
  await expect(page.getByRole("article", { name: "已刷新 Agent Shell：Windows PowerShell 5.1" })).toBeVisible();
  await expect(binding).toHaveText("Windows PowerShell 5.1");
  await page.screenshot({ path: info.outputPath("shell-explicit-refresh.png"), animations: "disabled" });
  const select_box = await binding.boundingBox();
  const refresh_box = await refresh.boundingBox();
  expect(select_box).not.toBeNull();
  expect(refresh_box).not.toBeNull();
  expect(refresh_box!.x).toBeGreaterThan(select_box!.x + select_box!.width);
  expect(Math.abs(refresh_box!.y + refresh_box!.height / 2 - select_box!.y - select_box!.height / 2)).toBeLessThan(2);
  await binding.click();
  await page.getByRole("option", { name: "CMD", exact: true }).click();
  await expect(binding).toHaveText("CMD");
  await expect(page.getByRole("article", { name: "已切换 Agent Shell：Windows PowerShell 5.1 → CMD" })).toBeVisible();
  await page.screenshot({ path: info.outputPath("shell-session-switch.png"), animations: "disabled" });
  await expect(page.locator('[data-quote-role="assistant"]')).toHaveCount(0);
  await expect(page.getByText(/离线回复/)).toHaveCount(0);

  for (const entry of catalog.catalog.filter((entry) => entry.kind !== "posix_sh")) {
    await page.getByRole("button", { name: "新建资源标签", exact: true }).click();
    await page.getByRole("menuitem", { name: "终端", exact: true }).click();
    const create = page.getByRole("toolbar", { name: "终端工具栏", exact: true });
    await expect(page.locator(".xterm-helper-textarea")).toBeVisible();
    await expect(page.getByRole("button", { name: "终端 Shell 类型", exact: true })).toBeEnabled();
    await expect(create).toBeVisible();
    await create.getByRole("button", { name: "终端 Shell 类型" }).click();
    const option = page.getByRole("option", { name: shellLabels[entry.kind], exact: false });
    if (!entry.available) {
      await expect(option).toHaveAttribute("aria-disabled", "true");
      await page.keyboard.press("Escape");
      await page.getByRole("button", { name: "关闭 终端", exact: true }).click();
      await page.getByRole("dialog", { name: "关闭 终端" }).getByRole("button", { name: "关闭终端", exact: true }).click();
      info.annotations.push({ type: "unavailable-shell", description: entry.kind });
      continue;
    }
    await option.click();
    await page.screenshot({ path: info.outputPath(`terminal-${entry.kind}-choice.png`), animations: "disabled" });

    await expect(page.locator(".xterm-helper-textarea")).toBeVisible();
    await expect(page.getByText("正在启动终端…", { exact: true })).toHaveCount(0);
    const marker = `终端中文-${entry.kind}`;
    // 输入回显不含完整 marker；必须等解释器真正执行后才能通过输出断言。
    const command = entry.kind === "cmd" ? "echo 终端中文-c^md & echo CWD_CHECK=%CD%" : entry.kind === "git_bash"
      ? `printf '\\033[32m终端中文-%s\\033[0m\\n' '${entry.kind}'`
      : `Write-Output ('{0}[32m终端中文-{1}{0}[0m' -f [char]27,'${entry.kind}')`;
    await page.locator(".xterm-helper-textarea").focus();
    await page.keyboard.insertText(command);
    await page.keyboard.press("Enter");
    await expect(page.locator(".xterm-rows")).toContainText(marker);
    if (entry.kind === "cmd") await expect(page.locator(".xterm-rows")).toContainText(/CWD_CHECK=[A-Za-z]:\\.*ez-assistant-e2e-workspace-/);
    await expect(page.getByTitle(shellLabels[entry.kind])).toBeVisible();
    await page.screenshot({ path: info.outputPath(`terminal-${entry.kind}-running.png`), animations: "disabled" });
    await page.getByRole("button", { name: "关闭 终端", exact: true }).click();
    await page.getByRole("dialog", { name: "关闭 终端" }).getByRole("button", { name: "关闭终端", exact: true }).click();
    await expect(page.locator(".xterm-helper-textarea")).toHaveCount(0);
  }
  expect(errors).toEqual([]);
});
