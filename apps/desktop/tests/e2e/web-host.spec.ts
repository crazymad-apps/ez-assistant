import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import type { ApplicationSnapshot, RunSnapshot, SystemContextSnapshot } from "../../src/generated/assistant-protocol";

type Fixture = Readonly<{ base_url: string; access_token: string }>;
const fixture = (): Fixture => JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP ?? "null") as Fixture;

async function expectLaunch(page: Page) {
  const transition = page.locator("[data-workspace-transition]");
  await expect(transition).toHaveAttribute("data-workspace-transition", "complete", { timeout: 8_000 });
  expect(Number(await transition.getAttribute("data-launch-frame"))).toBeGreaterThanOrEqual(110);
  await expect(transition.locator("canvas")).toHaveCount(0);
}

async function access(command: object) {
  const host = fixture();
  const response = await fetch(`${host.base_url}/commands`, {
    method: "POST", headers: { Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" },
    body: JSON.stringify({ request_id: "web-access-test", command: { scope: "host_access", payload: command } }),
  });
  expect(response.status).toBe(200);
  return (await response.json() as { result: { payload: { revision: string | null } } }).result.payload;
}

test.beforeEach(async () => {
  const current = await access({ type: "get_status" });
  await access({ type: "set_password", payload: { expected_revision: current.revision, password: "isolated-web-password" } });
});

test("Host 内嵌 Web 支持登录、设置、改密撤销和退出", async ({ page, request }, testInfo) => {
  const host = fixture();
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const anonymous = await request.get(`${host.base_url}/capabilities`);
  expect(anonymous.status()).toBe(401);
  const document = await page.goto(host.base_url);
  expect(document?.headers()["content-security-policy"]).toContain("connect-src 'self'");
  // Blob PDF 继承父页面策略；允许自身预览，仍禁止外站嵌入和 object 插件。
  const csp = document?.headers()["content-security-policy"]?.split("; ");
  expect(csp).toEqual(expect.arrayContaining(["frame-ancestors 'self'", "frame-src blob:", "object-src 'none'"]));
  await expect(page.getByLabel("Host 登录")).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath("web-login.png"), animations: "disabled" });
  await page.getByLabel("访问密码", { exact: true }).fill("isolated-web-password");
  await page.getByRole("button", { name: "进入工作空间" }).click();
  await page.waitForFunction(() => Number(document.querySelector("[data-launch-frame]")?.getAttribute("data-launch-frame")) >= 80);
  await page.screenshot({ path: testInfo.outputPath("web-workspace-reveal.png") });
  await expectLaunch(page);
  await expect(page.locator("[data-app-title-bar]")).toBeVisible();
  await expect(page.getByText("M2 临时会话", { exact: true }).first()).toBeVisible();
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await expect(page.getByRole("button", { name: /^本机与客户端/ })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "切换", exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: /^访问设置/ }).click();
  await expect(page.getByRole("checkbox", { name: "允许其他设备连接" })).toBeEnabled();
  await page.screenshot({ path: testInfo.outputPath("host-access-settings.png"), animations: "disabled" });
  await page.getByLabel("新密码", { exact: true }).fill("changed-web-password");
  await page.getByRole("button", { name: "保存密码", exact: true }).click();
  await expect(page.getByLabel("Host 登录")).toBeVisible();
  await expect(page.locator("[data-app-title-bar]")).toHaveCount(0);
  await page.getByLabel("访问密码", { exact: true }).fill("changed-web-password");
  await page.getByRole("button", { name: "进入工作空间" }).click();
  await expectLaunch(page);
  await expect(page.locator("[data-app-title-bar]")).toBeVisible();
  await page.reload();
  await expectLaunch(page);
  await expect(page.locator("[data-app-title-bar]")).toBeVisible();
  await page.getByRole("button", { name: "退出登录", exact: true }).click();
  await expect(page.getByLabel("Host 登录")).toBeVisible();
  expect((await request.get(`${host.base_url}/commands-typo`)).status()).toBe(404);
  expect(errors).toEqual([]);
});

test("普通 token fragment 快速登录后立即移除，并在失效时回到密码页", async ({ page }) => {
  const host = fixture();
  const issued = await fetch(`${host.base_url}/auth/login`, {
    method: "POST", headers: { Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" },
    body: JSON.stringify({ method: "desktop" }),
  });
  expect(issued.status).toBe(200);
  const { token } = await issued.json() as { token: string };
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await expectLaunch(page);
  await expect(page.locator("[data-app-title-bar]")).toBeVisible();
  expect(new URL(page.url()).hash).toBe("");
  const current = await access({ type: "get_status" });
  await access({ type: "set_password", payload: { expected_revision: current.revision, password: "revoked-web-password" } });
  await expect(page.getByLabel("Host 登录")).toBeVisible();
  await page.goto(`${host.base_url}/#token=expired-test-token`);
  await expect(page.getByLabel("访问密码", { exact: true })).toBeVisible();
  expect(new URL(page.url()).hash).toBe("");
  expect(await page.evaluate(() => sessionStorage.length)).toBe(0);
});

for (const fallback of ["reduced-motion", "webgl-and-storage-unavailable"] as const) {
  test(`窄屏深色环境下 ${fallback} 不阻断 Web 登录`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.emulateMedia({ colorScheme: "dark", reducedMotion: fallback === "reduced-motion" ? "reduce" : "no-preference" });
    if (fallback === "webgl-and-storage-unavailable") {
      await page.addInitScript(() => {
        const getContext = HTMLCanvasElement.prototype.getContext;
        HTMLCanvasElement.prototype.getContext = function (type: string, ...args: unknown[]) {
          if (type.includes("webgl")) return null;
          return Reflect.apply(getContext, this, [type, ...args]);
        } as typeof getContext;
        for (const method of ["getItem", "setItem", "removeItem"] as const) {
          Storage.prototype[method] = () => { throw new DOMException("Fixture storage unavailable", "SecurityError"); };
        }
      });
    }
    await page.goto(fixture().base_url);
    await expect(page.getByLabel("Host 登录")).toBeVisible();
    await expect(page.locator("[data-dots-state='static']")).toHaveCount(1);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath(`${fallback}.png`) });
    await page.getByLabel("访问密码", { exact: true }).fill("isolated-web-password");
    await page.getByLabel("访问密码", { exact: true }).press("Enter");
    await expect(page.locator("[data-app-title-bar]")).toBeVisible();
    await expect(page.getByRole("button", { name: "退出登录", exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.getByRole("button", { name: "退出登录", exact: true }).click();
    await expect(page.getByLabel("Host 登录")).toBeVisible();
  });
}


async function runtimeCommand<T>(type: string, payload: object): Promise<T> {
  const host = fixture();
  const response = await fetch(`${host.base_url}/commands`, {
    method: "POST", headers: { Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" },
    body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }),
  });
  expect(response.status).toBe(200);
  return (await response.json() as { result: { payload: { payload: T } } }).result.payload.payload;
}

test("主控与普通会话都能 refresh，Skill 新目录即时更新且不创建 Run", async ({ page }, info) => {
  const before = await runtimeCommand<{ snapshot: { value: ApplicationSnapshot } }>("get_application_snapshot", {});
  const prompts = new Map<string, string[]>();
  const existing_runs = new Map<string, string[]>();
  for (const session of before.snapshot.value.active_sessions) {
    const context = await runtimeCommand<{ snapshot: SystemContextSnapshot }>("get_system_context", { session_id: session.session_id });
    prompts.set(session.session_id, context.snapshot.parts);
    const result = await runtimeCommand<{ runs: RunSnapshot[] }>("list_runs", { session_id: session.session_id });
    existing_runs.set(session.session_id, result.runs.map((run) => run.run_id).sort());
  }
  const directory = join(process.env.EZ_ASSISTANT_E2E_SKILL_ROOT!, "refresh-check");
  await mkdir(directory, { recursive: true });
  await writeFile(join(directory, "SKILL.md"), "---\nname: refresh-check\ndescription: Refresh fixture\n---\nFollow this isolated fixture instruction.\n");
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(fixture().base_url);
  await page.getByLabel("访问密码", { exact: true }).fill("isolated-web-password");
  await page.getByRole("button", { name: "进入工作空间" }).click();
  await expect(page.locator("[data-workspace-transition]")).toHaveAttribute("data-workspace-transition", "complete");
  const input = page.getByRole("textbox", { name: "输入消息" });
  for (const controller of [true, false]) {
    if (controller) await page.getByRole("region", { name: "主控会话", exact: true }).getByRole("button").click();
    else await page.getByText("M2 临时会话", { exact: true }).first().click();
    // 文件在会话创建后新增：无需 refresh 就能从两个角色的选择器读取。
    await input.fill("/skill");
    await input.press("Enter");
    await expect(page.getByRole("option", { name: /refresh-check/ })).toBeVisible();
    await page.getByRole("combobox", { name: "搜索技能" }).press("Escape");
    const description = controller ? "Changed before controller refresh" : "Changed before session refresh";
    await writeFile(join(directory, "SKILL.md"), `---
name: refresh-check
description: ${description}
---
Current fixture instructions.
`);
    await input.fill("/skill");
    await input.press("Enter");
    await expect(page.getByRole("option", { name: /refresh-check/ })).toContainText(description);
    await page.getByRole("combobox", { name: "搜索技能" }).press("Escape");
    const skills = page.getByRole("button", { name: "技能", exact: true });
    await skills.click();
    await skills.click();
    await expect(page.getByText("refresh-check", { exact: true })).toBeVisible();
    await input.fill("/skill refresh");
    await input.press("Enter");
    await expect(page.getByRole("article", { name: "技能刷新完成", exact: true })).toContainText("1 个技能");
    await input.fill("/skill");
    await input.press("Enter");
    await expect(page.getByRole("option", { name: /refresh-check/ })).toBeVisible();
    await page.getByRole("combobox", { name: "搜索技能" }).press("Escape");
    await input.fill("/mcp refresh");
    await input.press("Enter");
    await expect(page.getByRole("article", { name: "MCP 刷新完成", exact: true })).toBeVisible();
    await page.screenshot({ path: info.outputPath(controller ? "controller-refresh.png" : "session-refresh.png") });
  }
  const snapshot = await runtimeCommand<{ snapshot: { value: ApplicationSnapshot } }>("get_application_snapshot", {});
  for (const session of snapshot.snapshot.value.active_sessions) {
    const result = await runtimeCommand<{ runs: RunSnapshot[] }>("list_runs", { session_id: session.session_id });
    // 同组文件用例可能已产生合法历史 Run；refresh 必须保留历史且不能新增 Run。
    expect(result.runs.map((run) => run.run_id).sort()).toEqual(existing_runs.get(session.session_id));
    const context = await runtimeCommand<{ snapshot: SystemContextSnapshot }>("get_system_context", { session_id: session.session_id });
    expect(context.snapshot.parts).toEqual(prompts.get(session.session_id));
    const view = await runtimeCommand<{ snapshot: { value: Record<string, unknown> } }>("get_session_view", { session_id: session.session_id });
    expect(view.snapshot.value).not.toHaveProperty("skill_catalog");
  }
});
