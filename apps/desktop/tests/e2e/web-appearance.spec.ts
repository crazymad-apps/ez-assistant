import { compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test, type Page } from "@playwright/test";

const fixture = () => JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
async function login(page: Page) {
  const host = fixture();
  const response = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers: { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" }, body: JSON.stringify({ method: "desktop" }) });
  expect(response.ok).toBe(true);
  const { token } = await response.json() as { token: string };
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await expect(page.getByLabel("输入消息", { exact: true })).toBeVisible();
}
async function appearance(page: Page) {
  await page.getByRole("button", { name: "账号与 Runtime", exact: true }).click();
  await page.getByRole("menuitem", { name: "设置", exact: true }).click();
  await page.getByRole("button", { name: "外观", exact: true }).click();
}
const themes = [["Monokai", "monokai"], ["默认浅色", "default_light"], ["Solarized Light", "solarized_light"], ["Solarized Dark", "solarized_dark"]] as const;

test("Host Web preserves streamed Markdown, draft and PTY while all four themes update and survive reload", async ({ page }, info) => {
  test.setTimeout(60000);
  const errors: string[] = []; let sockets = 0;
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("websocket", () => { sockets += 1; });
  await login(page);
  const csp = (await page.request.get(fixture().base_url)).headers()["content-security-policy"];
  expect(csp).toContain("connect-src 'self'");
  await page.getByText("M2 临时会话", { exact: true }).first().click();
  await page.getByLabel("输入消息", { exact: true }).fill('F05_RESOURCES\n```typescript\nconst theme = "hello";\n```\n\n```mermaid\ngraph LR\nA[读取] --> B[应用]\n```');
  await page.getByRole("button", { name: "发送消息", exact: true }).click();
  await expect(page.locator('[data-streamdown="mermaid-block"] svg')).toBeVisible();
  await page.getByLabel("输入消息", { exact: true }).fill("未发送草稿保持原样");
  await page.getByRole("button", { name: "新建资源标签", exact: true }).click();
  await page.getByRole("menuitem", { name: "终端", exact: true }).click();
  const terminal = page.locator(".xterm-helper-textarea");
  await expect(terminal).toBeVisible();
  await expect(page.getByText("正在启动终端…", { exact: true })).toHaveCount(0);
  await terminal.focus(); await page.keyboard.insertText("echo THEME_HISTORY_OK"); await page.keyboard.press("Enter");
  await expect(page.locator(".xterm-rows")).toContainText("THEME_HISTORY_OK");
  const socket_count = sockets;
  for (const [name, id] of themes) {
    await appearance(page);
    await page.getByRole("radio", { name, exact: true }).check();
    await expect(page.locator("html")).toHaveAttribute("data-theme", id);
    await expect.poll(() => page.evaluate(() => localStorage.getItem("ez-assistant:theme"))).toBe(id);
    await page.getByRole("button", { name: "关闭设置", exact: true }).click();
    await expect(page.getByRole("dialog", { name: "设置", exact: true })).toBeHidden();
    await expect(page.getByLabel("输入消息", { exact: true })).toHaveValue("未发送草稿保持原样");
    await expect(page.locator(".xterm-rows")).toContainText("THEME_HISTORY_OK");
    await expect(page.locator('[data-streamdown="mermaid-block"] svg')).toBeVisible();
    const code = page.locator('[data-streamdown="code-block-body"] span[style]').first();
    await expect(code).toHaveText("const");
    expect(await code.evaluate((node) => getComputedStyle(node).color)).not.toBe(await code.evaluate((node) => getComputedStyle(node.parentElement!).color));
    expect(sockets).toBe(socket_count);
    await page.screenshot({ path: info.outputPath(`${id}.png`), animations: "disabled" });
  }
  await page.reload();
  await expect(page.getByLabel("输入消息", { exact: true })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "solarized_dark");
  expect(errors).toEqual([]);
});

test("save errors retain the selected theme, retry after reopening, and do not recolor another open tab", async ({ page, context }) => {
  await login(page);
  const second = await context.newPage(); await second.goto(fixture().base_url);
  await expect(second.getByLabel("输入消息", { exact: true })).toBeVisible();
  await page.evaluate(() => {
    const original = Storage.prototype.setItem;
    (window as Window & { rejectTheme?: boolean }).rejectTheme = true;
    Storage.prototype.setItem = function (key, value) {
      if (key === "ez-assistant:theme" && (window as Window & { rejectTheme?: boolean }).rejectTheme) throw new Error("isolated storage rejection");
      original.call(this, key, value);
    };
  });
  await appearance(page); await page.getByRole("radio", { name: "Monokai", exact: true }).check();
  await expect(page.getByRole("button", { name: "重试", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "关闭设置", exact: true }).click();
  await appearance(page); await expect(page.getByRole("radio", { name: "Monokai", exact: true })).toBeChecked();
  await page.evaluate(() => { (window as Window & { rejectTheme?: boolean }).rejectTheme = false; });
  await page.getByRole("button", { name: "重试", exact: true }).click();
  await expect(page.getByRole("button", { name: "重试", exact: true })).toHaveCount(0);
  await expect(second.locator("html")).toHaveAttribute("data-theme", "default_light");
  await second.reload(); await expect(second.locator("html")).toHaveAttribute("data-theme", "monokai");
  await second.close();
});

test("unknown saved preferences fall back without overwriting the stored value", async ({ page }) => {
  await page.addInitScript(() => { localStorage.setItem("ez-assistant:theme", "future-theme"); });
  await login(page);
  await expect(page.locator("html")).toHaveAttribute("data-theme", "default_light");
  expect(await page.evaluate(() => localStorage.getItem("ez-assistant:theme"))).toBe("future-theme");
  await appearance(page);
  await expect(page.getByRole("dialog", { name: "设置", exact: true }).getByText("已保存的配色不可用，已使用默认配色。")).toBeVisible();
  await page.getByRole("radio", { name: "Solarized Light", exact: true }).check();
  await expect.poll(() => page.evaluate(() => localStorage.getItem("ez-assistant:theme"))).toBe("solarized_light");
});
