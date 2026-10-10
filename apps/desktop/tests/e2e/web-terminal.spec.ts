import { compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test, type Page } from "@playwright/test";
import { mkdir, readFile } from "node:fs/promises";
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
  await expect(page.getByRole("button", { name: "账号与 Runtime", exact: true })).toBeVisible();
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
  await expect(second.getByRole("button", { name: "账号与 Runtime", exact: true })).toBeVisible();
  await expect(second.getByRole("tab", { name: "终端", exact: true })).toHaveCount(0);
  await expect(second.locator(".xterm-helper-textarea")).toHaveCount(0);
  await gone(secondPid);
  await second.close();
  expect(errors).toEqual([]);
});

test("F02 default and explicit shells keep short prompts through confirmed switching", async ({ page }, info) => {
  test.skip(process.platform === "win32", "Unix interactive Shell matrix; Windows has its own fixture");
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const response = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers: { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" }, body: JSON.stringify({ method: "desktop" }) });
  expect(response.ok).toBe(true);
  const { token } = await response.json() as { token: string };
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await openTerminal(page);
  const select = page.getByRole("button", { name: "终端 Shell 类型", exact: true });
  await expect(select).toBeEnabled();
  await expect(select).toContainText("系统默认 Shell");
  const directory = join(process.env.EZ_ASSISTANT_E2E_NEW_WORKSPACE!, "long-parent", "F02-中文目录");
  await mkdir(directory, { recursive: true });
  const rows = page.locator(".xterm-rows");
  let previous_pid: number | null = null;
  for (const [index, label] of [null, "Bash", "/bin/sh", "系统默认 Shell"].entries()) {
    if (label) {
      await select.click();
      await page.getByRole("option", { name: label, exact: label !== "系统默认 Shell" }).click();
      const dialog = page.getByRole("dialog", { name: "切换终端 Shell" });
      await expect(dialog).toBeVisible();
      if (index === 1) {
        await dialog.getByRole("button", { name: "取消", exact: true }).click();
        await expect(select).toContainText("系统默认 Shell");
        expect(() => process.kill(previous_pid!, 0)).not.toThrow();
        await select.click();
        await page.getByRole("option", { name: label, exact: true }).click();
      }
      await dialog.getByRole("button", { name: "切换 Shell", exact: true }).click();
      await expect(select).toBeEnabled();
      await expect(select).toContainText(label);
      await gone(previous_pid!);
    }
    await expect(rows).toContainText(/[%$#]\s*$/);
    const marker = join(directory, `shell-${index}.pid`);
    await command(page, `cd '${directory.replaceAll("'", "'\\''")}'; echo $$ > '${marker.replaceAll("'", "'\\''")}'; printf 'READY-%s\\n' ${index}`);
    const pid = await terminalPid(marker);
    previous_pid = pid;
    await expect(rows).toContainText(`READY-${index}`);
    await expect.poll(async () => (await rows.innerText()).replaceAll("\n", "")).toMatch(/F02-中文目录 [%$#]\s*$/);
    await page.screenshot({ path: info.outputPath(`f02-shell-${index}.png`), animations: "disabled" });
    if (index === 0) {
      await page.keyboard.insertText("printf 'EDIT-%s\\n' wrong");
      for (let character = 0; character < 5; character++) await page.keyboard.press("Backspace");
      await page.keyboard.insertText("ok");
      await page.keyboard.press("Enter");
      await expect(rows).toContainText("EDIT-ok");
      await page.keyboard.press("ArrowUp");
      await page.keyboard.press("Enter");
      await expect.poll(async () => (await rows.innerText()).match(/EDIT-ok/g)?.length ?? 0).toBe(2);
      await command(page, "printf 'WAIT-%s\\n' started; sleep 60");
      await expect(rows).toContainText("WAIT-started");
      await page.keyboard.press("Control+c");
      await command(page, "printf 'AFTER-%s\\n' interrupt");
      await expect(rows).toContainText("AFTER-interrupt");
    }
    if (index === 3) {
      await page.getByRole("button", { name: "关闭 终端", exact: true }).click();
      await page.getByRole("dialog", { name: "关闭 终端" }).getByRole("button", { name: "关闭终端", exact: true }).click();
      await gone(pid);
    }
  }
  await expect(page.locator(".xterm-helper-textarea")).toHaveCount(0);
});
