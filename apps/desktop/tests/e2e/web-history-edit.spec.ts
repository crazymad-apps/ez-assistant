import { type RuntimeCommand, type RuntimeCommandResult, compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test } from "@playwright/test";

test.use({ viewport: { width: 1440, height: 900 } });

// 只在 globalSetup 新建的 Runtime Home 内创建、改写并读取合成会话。
test("历史消息编辑保留普通草稿，丢失回执后同键核实并刷新正文", async ({ page }) => {
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" };
  async function command<T extends RuntimeCommand["type"]>(type: T, payload: object = {}): Promise<Extract<RuntimeCommandResult, { type: T }>["payload"]> {
    const response = await fetch(`${host.base_url}/commands`, {
      method: "POST", headers,
      body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }),
    });
    const body = await response.json();
    expect(response.status, JSON.stringify(body)).toBe(200);
    return body.result.payload.payload;
  }
  const app = (await command("get_application_snapshot")).snapshot.value;
  expect(app.capabilities.user_message_edit).toBe(true);
  const title = "F02 编辑验收";
  const { session } = await command("create_session", { title, workspace_id: app.workspaces[0].workspace_id });
  const session_id = session.session_id;
  const view = async () => (await command("get_session_view", { session_id })).snapshot.value;
  for (const message of ["FIRST_CASE 原消息", "QUEUED_CASE 待裁尾消息"]) {
    const { input_id } = await command("submit_input", { session_id, message, mode: "normal", variant: "build", attachment_ids: [], quotes: [] });
    await expect.poll(async () => (await view()).runs.find(run => run.input_id === input_id)?.status).toBe("completed");
  }
  const before = await view();
  const first = before.conversation.items.find(item => item.type === "user");
  expect(first).toBeDefined();

  const login = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  expect(login.ok).toBe(true);
  const { token } = await login.json() as { token: string };
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.addInitScript(() => { localStorage.setItem("ez-assistant:theme", "solarized_light"); });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await expect(page.locator("html")).toHaveAttribute("data-theme", "solarized_light");
  const sidebar = page.getByRole("complementary", { name: "会话导航" });
  await sidebar.getByRole("button", { name: title, exact: true }).click();
  await expect(page.getByText("离线回复：QUEUED_CASE", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "收起资源栏" }).click();
  const ordinary = page.getByRole("textbox", { name: "输入消息" });
  await ordinary.fill("应保留的普通草稿");
  const row = page.locator(`article[data-message-id="${first!.message_id}"]`);
  async function openEdit() {
    await row.hover();
    await row.getByRole("button", { name: "编辑用户消息" }).click();
    await expect(page.getByRole("textbox", { name: "编辑消息" })).toHaveValue("FIRST_CASE 原消息");
  }
  await openEdit();
  const editor = page.getByRole("textbox", { name: "编辑消息" });
  const drawer = page.getByRole("region", { name: "正在编辑历史消息" });
  await expect(editor).toBeFocused();
  await expect(page.getByRole("button", { name: "发送修改" })).toBeEnabled();
  const edit_attachments = page.getByRole("button", { name: "添加输入内容" });
  await expect(edit_attachments).toHaveCSS("border-top-width", "0px");
  await edit_attachments.click();
  await expect(page.getByRole("menuitem", { name: "添加附件", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(sidebar).toBeVisible();
  await expect(drawer.getByRole("button", { name: "返回原位置" })).toBeVisible();
  await expect(drawer.getByRole("button", { name: "取消", exact: true })).toBeVisible();
  if (process.env.EZ_ASSISTANT_E2E_CAPTURE_HISTORY_EDIT === "1") {
    await editor.focus();
    await page.screenshot({ path: test.info().outputPath("history-edit-desktop.png") });
  }
  await drawer.getByRole("button", { name: "返回原位置" }).click();
  await expect(editor).toHaveValue("FIRST_CASE 原消息");
  await editor.fill("取消时放弃的修改");
  await drawer.getByRole("button", { name: "取消", exact: true }).click();
  await expect(ordinary).toHaveValue("应保留的普通草稿");
  expect((await view()).conversation_generation).toBe(before.conversation_generation);

  const attempts: unknown[] = [];
  await page.route(`${host.base_url}/commands`, async route => {
    const payload = route.request().postDataJSON()?.command?.payload;
    if (payload?.type !== "reenter_from_user_message") { await route.continue(); return; }
    attempts.push(payload.payload);
    if (attempts.length === 1) {
      const response = await route.fetch();
      expect(response.status()).toBe(200);
      await route.abort();
    } else {
      await route.continue();
    }
  });
  await openEdit();
  await editor.fill("F02_STREAM 修改后新轮");
  const concurrent = await command("submit_input", {
    session_id, message: "QUEUED_CASE 并行追加", mode: "normal", variant: "build", attachment_ids: [], quotes: [],
  });
  await expect.poll(async () => (await view()).runs.find(run => run.input_id === concurrent.input_id)?.status).toBe("completed");
  await expect(page.getByText("历史已更新，请重新核对后发送。")).toBeVisible();
  await expect(page.getByRole("button", { name: "发送修改" })).toBeDisabled();
  await page.getByRole("button", { name: "重新核对" }).click();
  await expect(editor).toHaveValue("F02_STREAM 修改后新轮");
  await expect(page.getByRole("button", { name: "发送修改" })).toBeEnabled();
  expect(attempts).toHaveLength(0);
  const execution_settings = page.getByRole("button", { name: "执行设置" });
  await execution_settings.click();
  await page.getByRole("menuitem", { name: /执行模式/ }).click();
  await page.getByRole("menuitemradio", { name: /规划/ }).click();
  await expect(execution_settings).toContainText("规划");
  expect((await view()).session.current_variant).toBe("build");
  await editor.press("Control+Enter");
  expect(attempts).toHaveLength(0);
  await editor.press("Enter");
  await expect(page.getByText("发送结果尚未确认。")).toBeVisible();
  await expect(drawer.getByRole("button", { name: "取消", exact: true })).toBeDisabled();
  await page.getByRole("button", { name: "重试原发送" }).click();
  await expect(ordinary).toHaveValue("应保留的普通草稿");
  await expect(page.getByText("F02_STREAM 修改后新轮", { exact: true })).toBeVisible();
  await expect(page.getByText("QUEUED_CASE 待裁尾消息", { exact: true })).toHaveCount(0);
  await expect.poll(async () => (await view()).runs.map(run => run.status), { timeout: 12_000 }).toEqual(["completed"]);
  expect(attempts).toHaveLength(2);
  expect(attempts[1]).toEqual(attempts[0]);
  const after = await view();
  expect(after.session.current_variant).toBe("build");
  expect(attempts[0]).toMatchObject({ variant: "plan" });
  expect(after.conversation_generation).toBeGreaterThan(before.conversation_generation);
  expect(after.conversation.items.filter(item => item.type === "user").map(item => item.text)).toEqual(["F02_STREAM 修改后新轮"]);
  const latest_user = after.conversation.items.find(item => item.type === "user")!;
  const latest_row = page.locator(`article[data-message-id="${latest_user.message_id}"]`);
  await latest_row.hover();
  await latest_row.getByRole("button", { name: "编辑用户消息" }).click();
  await expect(editor).toHaveValue(latest_user.text);
  await page.getByRole("button", { name: "发送修改" }).click();
  await expect(ordinary).toHaveValue("应保留的普通草稿");
  // F02_STREAM 本身连续发送 120 个 SSE 帧；Composer 回执已由草稿恢复单独验证。
  await expect.poll(async () => (await view()).runs.map(run => run.status), { timeout: 12_000 }).toEqual(["completed"]);
  const resent = await view();
  const resent_user = resent.conversation.items.find(item => item.type === "user")!;
  expect(resent.conversation_generation).toBeGreaterThan(after.conversation_generation);
  expect(resent_user.text).toBe(latest_user.text);
  expect(resent_user.message_id).not.toBe(latest_user.message_id);
  expect(resent.runs[0].run_id).not.toBe(after.runs[0].run_id);
  expect(attempts).toHaveLength(3);
  expect(attempts[2]).toMatchObject({ message: latest_user.text, variant: "plan" });
  const resent_row = page.locator(`article[data-message-id="${resent_user.message_id}"]`);
  await command("submit_input", {
    session_id, message: "BLOCK_FOR_QUEUE", mode: "normal", variant: "build", attachment_ids: [], quotes: [],
  });
  await expect(resent_row.getByRole("button", { name: "编辑用户消息" })).toHaveCount(0);
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
  await resent_row.hover();
  await resent_row.getByRole("button", { name: "复制用户消息" }).click();
  await expect(resent_row.getByRole("status")).toHaveText("已复制");
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("F02_STREAM 修改后新轮");
});
