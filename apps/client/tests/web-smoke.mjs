import { createRequire } from "node:module";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
const require = createRequire(new URL("../../desktop/package.json", import.meta.url));
const { chromium, expect } = require("@playwright/test");
const home = process.env.EZ_ASSISTANT_TEST_RUNTIME_HOME;
if (!home || !home.includes("ez-client-web-")) throw new Error("isolated Web fixture required");
const discovery = JSON.parse(await readFile(join(home, "run/runtime.json"), "utf8"));
const browser = await chromium.launch({ headless: true });
try {
  const context = await browser.newContext({ viewport: { width: 1200, height: 840 }, reducedMotion: "reduce" });
  const page = await context.newPage(); const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(discovery.address);
  await expect(page.getByLabel("Host 登录")).toBeVisible();
  await page.getByLabel("访问密码", { exact: true }).fill("isolated-client-password");
  await page.getByRole("button", { name: "进入工作空间" }).click();
  await expect(page.locator("[data-app-title-bar]")).toBeVisible({ timeout: 15000 });
  await page.getByText("Client M4 临时会话", { exact: true }).first().click();
  const composer = page.getByRole("textbox", { name: "输入消息" });
  await composer.fill("验证 Client 启动的 Host"); await composer.press("Enter");
  await expect(page.getByText("Client 启动的 Host 已完成 Agent 回复。", { exact: true }).first()).toBeVisible({ timeout: 20000 });
  await page.screenshot({ path: process.env.EZ_ASSISTANT_TEST_SCREENSHOT, animations: "disabled" });
  expect(errors).toEqual([]);
  await page.getByRole("button", { name: "退出登录", exact: true }).click();
  await expect(page.getByLabel("Host 登录")).toBeVisible();
  await context.close();
  console.log("PASS real Chromium: ordinary Web login, Agent reply, logout; no native token URL");
} finally { await browser.close(); }
