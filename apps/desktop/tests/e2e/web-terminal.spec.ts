import { compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test, type Page } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

async function openTerminal(page: Page) {
  await page.getByText("M2 临时会话", { exact: true }).first().click();
  await page.getByRole("button", { name: "新建资源标签", exact: true }).click();
  await page.getByRole("menuitem", { name: "终端", exact: true }).click();
  await expect(page.locator(".xterm-helper-textarea")).toBeVisible();
  await expect(page.getByText("正在启动终端…", { exact: true })).toHaveCount(0);
}
async function command(page: Page, text: string) {
  await page.locator(".xterm-helper-textarea").focus();
  await page.keyboard.insertText(text);
  await page.keyboard.press("Enter");
}
async function terminalPid(file: string): Promise<number> {
  let pid = 0;
  await expect.poll(async () => {
    try { pid = Number(await readFile(file, "utf8")); return pid > 1; } catch { return false; }
  }).toBe(true);
  return pid;
}
async function gone(pid: number) {
  await expect.poll(() => { try { process.kill(pid, 0); return false; } catch { return true; } }, { timeout: 5000 }).toBe(true);
}

test("Web terminal renders Host ANSI/UTF-8; closing and refresh reap only their own PTYs", async ({ page, context }, info) => {
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const response = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers: { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" }, body: JSON.stringify({ method: "desktop" }) });
  expect(response.ok).toBe(true);
  const { token } = await response.json() as { token: string };
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await expect(page.locator("[data-app-title-bar]")).toBeVisible();
  const second = await context.newPage();
  await second.goto(host.base_url);
  const errors: string[] = [];
  for (const view of [page, second]) { view.on("pageerror", (e) => errors.push(e.message)); await openTerminal(view); }
  const directory = process.env.EZ_ASSISTANT_E2E_NEW_WORKSPACE!;
  const firstFile = join(directory, "web-terminal-first.pid");
  const secondFile = join(directory, "web-terminal-second.pid");
  await command(page, String.raw`echo $$ > '${firstFile}'; printf '\033[32m终端中文输出\033[0m\n'`);
  const firstPid = await terminalPid(firstFile);
  await expect(page.locator(".xterm-rows")).toContainText("终端中文输出");
  await command(second, `echo $$ > '${secondFile}'; printf 'SECOND_HOST_TERMINAL\n'`);
  const secondPid = await terminalPid(secondFile);
  await expect(second.locator(".xterm-rows")).toContainText("SECOND_HOST_TERMINAL");
  await page.screenshot({ path: info.outputPath("web-terminal.png") });
  await page.getByRole("button", { name: "关闭 终端", exact: true }).click();
  await page.getByRole("dialog", { name: "关闭 终端" }).getByRole("button", { name: "关闭终端", exact: true }).click();
  await expect(page.getByRole("tab", { name: "终端", exact: true })).toHaveCount(0);
  await gone(firstPid);
  expect(() => process.kill(secondPid, 0)).not.toThrow();
  await command(second, "printf 'STILL_CONNECTED_AFTER_OTHER_CLOSE\n'");
  await expect(second.locator(".xterm-rows")).toContainText("STILL_CONNECTED_AFTER_OTHER_CLOSE");
  await second.reload();
  await expect(second.locator("[data-app-title-bar]")).toBeVisible();
  await expect(second.getByRole("tab", { name: "终端", exact: true })).toHaveCount(0);
  await expect(second.locator(".xterm-helper-textarea")).toHaveCount(0);
  await gone(secondPid);
  await second.close();
  expect(errors).toEqual([]);
});
