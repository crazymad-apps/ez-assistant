import { compatibilityHeaders } from "@ez-assistant/protocol";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import type { ApplicationSnapshot, RunSnapshot, SystemContextSnapshot } from "@ez-assistant/protocol";

type Fixture = Readonly<{ base_url: string; access_token: string }>;
const fixture = (): Fixture => JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP ?? "null") as Fixture;

async function expectWorkspaceReady(page: Page) {
  const transition = page.locator("[data-workspace-transition]");
  await expect(transition).toHaveAttribute("data-workspace-transition", "complete", { timeout: 8_000 });
  await expect(transition.locator("canvas")).toHaveCount(0);
}

async function access(command: object) {
  const host = fixture();
  const response = await fetch(`${host.base_url}/commands`, {
    method: "POST", headers: { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" },
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
  expect(anonymous.status()).toBe(200);
  const document = await page.goto(host.base_url);
  expect(document?.headers()["content-security-policy"]).toContain("connect-src 'self'");
  // Blob PDF 继承父页面策略；允许自身预览，仍禁止外站嵌入和 object 插件。
  const csp = document?.headers()["content-security-policy"]?.split("; ");
  expect(csp).toEqual(expect.arrayContaining(["frame-ancestors 'self'", "frame-src blob:", "object-src 'none'"]));
  await expect(page.getByLabel("Host 登录")).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath("web-login.png"), animations: "disabled" });
  await page.getByLabel("访问密码", { exact: true }).fill("isolated-web-password");
  await page.getByRole("button", { name: "进入项目" }).click();
  await expectWorkspaceReady(page);
  await page.screenshot({ path: testInfo.outputPath("web-workspace-reveal.png") });
  await expect(page.locator("[data-app-title-bar]")).toBeVisible();
  await expect(page.getByText("M2 临时会话", { exact: true }).first()).toBeVisible();
  await page.getByRole("button", { name: "账号与 Runtime" }).click();
  await page.getByRole("menuitem", { name: "设置" }).click();
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
  await page.getByRole("button", { name: "进入项目" }).click();
  await expectWorkspaceReady(page);
  await expect(page.locator("[data-app-title-bar]")).toBeVisible();
  await page.reload();
  await expectWorkspaceReady(page);
  await expect(page.locator("[data-app-title-bar]")).toBeVisible();
  await page.getByRole("button", { name: "个人用户", exact: true }).click();
  await page.getByRole("menuitem", { name: "退出登录", exact: true }).click();
  await page.getByRole("button", { name: "中断所有任务并退出", exact: true }).click();
  await expect(page.getByLabel("Host 登录")).toBeVisible();
  expect((await request.get(`${host.base_url}/commands-typo`)).status()).toBe(404);
  expect(errors).toEqual([]);
});

test("普通 token fragment 快速登录后立即移除，并在失效时回到密码页", async ({ page }) => {
  const host = fixture();
  const issued = await fetch(`${host.base_url}/auth/login`, {
    method: "POST", headers: { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" },
    body: JSON.stringify({ method: "desktop" }),
  });
  expect(issued.status).toBe(200);
  const { token } = await issued.json() as { token: string };
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await expectWorkspaceReady(page);
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
    await expect(page.getByRole("button", { name: "个人用户", exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.getByRole("button", { name: "个人用户", exact: true }).click();
    await page.getByRole("menuitem", { name: "退出登录", exact: true }).click();
    await page.getByRole("button", { name: "中断所有任务并退出", exact: true }).click();
    await expect(page.getByLabel("Host 登录")).toBeVisible();
  });
}


async function runtimeCommand<T>(type: string, payload: object): Promise<T> {
  const host = fixture();
  const response = await fetch(`${host.base_url}/commands`, {
    method: "POST", headers: { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" },
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
  await page.getByRole("button", { name: "进入项目" }).click();
  await expect(page.locator("[data-workspace-transition]")).toHaveAttribute("data-workspace-transition", "complete");
  const input = page.getByRole("textbox", { name: "输入消息" });
  for (const controller of [true, false]) {
    if (controller) await page.getByRole("button", { name: "助理会话" }).click();
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

test("Web 新会话使用底部抽屉并可从私有工作空间首次发送", async ({ page }) => {
  const host = fixture();
  await page.goto(host.base_url);
  await page.getByLabel("访问密码", { exact: true }).fill("isolated-web-password");
  await page.getByRole("button", { name: "进入项目" }).click();
  await expectWorkspaceReady(page);
  const before = await runtimeCommand<{ snapshot: { value: ApplicationSnapshot } }>("get_application_snapshot", {});
  await page.getByRole("button", { name: "新建会话" }).click();
  const header = page.locator('header[aria-label="会话标题栏"]');
  await expect(header.getByRole("button", { name: "新对话" })).toHaveCount(0);
  await expect(page.getByRole("region", { name: "新会话项目选项" })).toBeVisible();
  await page.getByRole("button", { name: "添加项目" }).click();
  const editor = page.getByRole("dialog", { name: "新建项目" });
  await expect(editor.getByText(/仅使用私有目录/)).toBeVisible();
  await editor.getByRole("textbox", { name: /项目名称/ }).fill("Web M4 私有工作空间");
  await editor.getByRole("button", { name: "创建" }).click();
  await expect(editor).toBeHidden();
  await expect(page.getByRole("button", { name: "新会话项目" })).toContainText("Web M4 私有工作空间");
  const after_create = await runtimeCommand<{ snapshot: { value: ApplicationSnapshot } }>("get_application_snapshot", {});
  expect(after_create.snapshot.value.active_sessions.length).toBe(before.snapshot.value.active_sessions.length);
  const workspace = after_create.snapshot.value.workspaces.find((item) => item.label === "Web M4 私有工作空间");
  expect(workspace?.user_directory).toBeNull();
  const input = page.getByRole("textbox", { name: "输入消息" });
  await page.getByRole("button", { name: "整理文档" }).click();
  await expect(input).toHaveValue(/请整理我提供的材料/);
  await input.fill("PRIVATE_CASE");
  await input.press("Enter");
  await expect(page.getByText("离线回复：PRIVATE_CASE", { exact: true })).toBeVisible();
  const after_send = await runtimeCommand<{ snapshot: { value: ApplicationSnapshot } }>("get_application_snapshot", {});
  expect(after_send.snapshot.value.active_sessions.length).toBe(before.snapshot.value.active_sessions.length + 1);
});

test("C03 Web 可创建个人智能体、选择并冻结到新会话", async ({ page }) => {
  const host = fixture();
  await page.goto(host.base_url);
  await page.getByLabel("访问密码", { exact: true }).fill("isolated-web-password");
  await page.getByRole("button", { name: "进入项目" }).click();
  await expectWorkspaceReady(page);
  await page.getByRole("button", { name: "账号与 Runtime" }).click();
  await page.getByRole("menuitem", { name: "设置" }).click();
  await page.getByRole("button", { name: "智能体", exact: true }).click();
  await page.getByRole("button", { name: "创建", exact: true }).click();
  await page.getByRole("textbox", { name: "名称" }).fill("Web C03 助手");
  await page.getByRole("textbox", { name: "说明" }).fill("隔离验收用个人智能体");
  await page.getByRole("textbox", { name: "完整提示词" }).fill("请简明回答。");
  await page.getByRole("button", { name: "保存智能体" }).click();
  await expect(page.getByRole("button", { name: /Web C03 助手/ })).toBeVisible();
  await page.getByRole("button", { name: "关闭设置" }).click();
  const before = await runtimeCommand<{ snapshot: { value: ApplicationSnapshot } }>("get_application_snapshot", {});
  await page.getByRole("button", { name: "新建会话" }).click();
  await page.getByRole("button", { name: "选择智能体" }).click();
  await page.getByRole("dialog", { name: "选择智能体" }).getByRole("option", { name: /Web C03 助手/ }).click();
  await expect(page.getByRole("button", { name: "选择智能体" })).toHaveText(/Web C03 助手/);
  const after_select = await runtimeCommand<{ snapshot: { value: ApplicationSnapshot } }>("get_application_snapshot", {});
  expect(after_select.snapshot.value.active_sessions.length).toBe(before.snapshot.value.active_sessions.length);
  await page.getByRole("textbox", { name: "输入消息" }).fill("FIRST_CASE");
  await page.getByRole("button", { name: "发送消息" }).click();
  await expect(page.getByText("离线回复：FIRST_CASE", { exact: true })).toBeVisible();
  const after_send = await runtimeCommand<{ snapshot: { value: ApplicationSnapshot } }>("get_application_snapshot", {});
  expect(after_send.snapshot.value.active_sessions.length).toBe(before.snapshot.value.active_sessions.length + 1);
  expect(after_send.snapshot.value.active_sessions.some((session) => session.custom_agent?.name === "Web C03 助手" && session.custom_agent.source.type === "personal")).toBe(true);
});

test("C03 缺少插件候选时保留草稿并提供处理入口", async ({ page }) => {
  const host = fixture();
  await page.goto(host.base_url);
  await page.getByLabel("访问密码", { exact: true }).fill("isolated-web-password");
  await page.getByRole("button", { name: "进入项目" }).click();
  await expectWorkspaceReady(page);
  await page.getByRole("button", { name: "账号与 Runtime" }).click();
  await page.getByRole("menuitem", { name: "设置" }).click();
  await page.getByRole("button", { name: "智能体", exact: true }).click();
  await page.getByRole("button", { name: "创建", exact: true }).click();
  await page.getByRole("textbox", { name: "名称" }).fill("缺包测试智能体");
  await page.getByRole("textbox", { name: "完整提示词" }).fill("按要求回答。");
  await page.getByRole("button", { name: "添加默认技能" }).click();
  await page.getByRole("textbox", { name: "默认技能名称 1" }).fill("missing-skill");
  await page.getByRole("textbox", { name: "默认技能所属插件 1" }).fill("missing-plugin");
  await page.getByRole("button", { name: "保存智能体" }).click();
  await expect(page.getByRole("button", { name: /缺包测试智能体/ })).toBeVisible();
  await page.getByRole("button", { name: "关闭设置" }).click();
  await page.getByRole("button", { name: "新建会话" }).click();
  await page.getByRole("button", { name: "选择智能体" }).click();
  await page.getByRole("dialog", { name: "选择智能体" }).getByRole("option", { name: /缺包测试智能体/ }).click();
  await expect(page.getByText(/缺少插件/)).toBeVisible();
  await page.getByRole("textbox", { name: "输入消息" }).fill("MISSING_PLUGIN_CASE");
  const before = await runtimeCommand<{ snapshot: { value: ApplicationSnapshot } }>("get_application_snapshot", {});
  await page.getByRole("button", { name: "发送消息" }).click();
  await expect(page.getByRole("button", { name: "管理插件" })).toBeVisible();
  const after = await runtimeCommand<{ snapshot: { value: ApplicationSnapshot } }>("get_application_snapshot", {});
  expect(after.snapshot.value.active_sessions.length).toBe(before.snapshot.value.active_sessions.length);
  await page.getByRole("button", { name: "管理插件" }).click();
  await expect(page.getByRole("button", { name: "上传插件" })).toBeVisible();
  await page.getByRole("button", { name: "关闭设置" }).click();
  await expect(page.getByRole("textbox", { name: "输入消息" })).toHaveValue("MISSING_PLUGIN_CASE");
});

test("C03 Web 上传真实插件包并显示安装终态", async ({ page }) => {
  test.setTimeout(90_000);
  const archive = process.env.C03_SAMPLE_ARCHIVE;
  test.skip(!archive, "requires an explicit preserved C03 sample archive");
  const host = fixture();
  await page.goto(host.base_url);
  await page.getByLabel("访问密码", { exact: true }).fill("isolated-web-password");
  await page.getByRole("button", { name: "进入项目" }).click();
  await expectWorkspaceReady(page);
  await page.getByRole("button", { name: "账号与 Runtime" }).click();
  await page.getByRole("menuitem", { name: "设置" }).click();
  await page.getByRole("button", { name: "插件", exact: true }).click();
  await page.getByLabel("选择插件包").setInputFiles(archive!);
  const progress = page.getByRole("dialog", { name: "安装插件" });
  await expect(progress.getByText("warehouse-cad-guidance", { exact: true })).toHaveCount(2);
  await expect(progress.getByText(/0\.1\.0/)).toBeVisible();
  await progress.getByRole("button", { name: "确认并开始安装" }).click();
  await expect(progress.getByText("插件操作已完成")).toBeVisible({ timeout: 60_000 });
  await progress.getByRole("button", { name: "关闭" }).click();
  await expect(page.getByRole("button", { name: /warehouse-cad-guidance/ })).toBeVisible();
  await page.getByRole("button", { name: /warehouse-cad-guidance/ }).click();
  await expect(page.getByText("compact-shelving-cad")).toBeVisible();
});

test("released old Web assets cannot log in to the new Host", async ({ page, request }, info) => {
  const old = process.env.EZ_ASSISTANT_TEST_OLD_WEB_ORIGIN;
  test.skip(!old, "requires an explicitly started isolated released Host");
  expect(old).toMatch(/^http:\/\/127\.0\.0\.1:\d+$/);
  const host = fixture();
  // Simulate an already cached released page; all application requests still reach the new Host.
  await page.route(`${host.base_url}/**`, async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/" || path.startsWith("/assets/")) {
      const response = await request.get(`${old}${path}`);
      expect(response.ok()).toBe(true);
      await route.fulfill({ response });
    } else await route.continue();
  });
  await page.goto(host.base_url);
  await expect(page.getByLabel("访问密码", { exact: true })).toBeVisible();
  await page.getByLabel("访问密码", { exact: true }).fill("isolated-web-password");
  const rejected = page.waitForResponse((response) => response.url() === `${host.base_url}/auth/login` && response.request().method() === "POST");
  await page.getByRole("button", { name: "进入项目" }).click();
  const response = await rejected;
  expect(response.status()).toBe(409);
  expect((await response.json()).error.code).toBe("missing_declaration");
  await expect(page.locator("[data-app-title-bar]")).toHaveCount(0);
  await page.screenshot({ path: info.outputPath("released-web-rejected.png") });
  const health = await request.get(`${host.base_url}/health`, { headers: { Authorization: `Bearer ${host.access_token}` } });
  expect((await health.json()).status).toBe("ready");
});
