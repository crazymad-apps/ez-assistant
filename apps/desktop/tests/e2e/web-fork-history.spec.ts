import { type RuntimeCommand, type RuntimeCommandResult, compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test } from "@playwright/test";

test("repeated forks remain readable after source deletion and accept new input", async ({ page }) => {
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
  const source = (await command("create_session", {
    title: "F02 分支来源", workspace_id: app.workspaces[0].workspace_id,
  })).session;
  const accepted = await command("submit_input", {
    session_id: source.session_id, message: "FIRST_CASE", mode: "normal", variant: "build", attachment_ids: [], quotes: [],
  });
  await expect.poll(async () => (await command("get_run", {
    session_id: source.session_id, run_id: accepted.run.run_id,
  })).run.status).toBe("completed");
  const original = (await command("get_session_view", { session_id: source.session_id })).snapshot.value;
  const issued = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  expect(issued.ok).toBe(true);
  const { token } = await issued.json() as { token: string };
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  const navigation = page.getByRole("complementary", { name: "会话导航" });
  await navigation.getByRole("button", { name: source.title, exact: true }).click();
  await expect(page.getByText("离线回复：FIRST_CASE", { exact: true })).toBeVisible();
  let title = source.title;
  const copies = [];
  for (let index = 0; index < 2; index++) {
    await page.getByRole("button", { name: "从此消息分叉" }).last().click();
    await page.getByRole("dialog", { name: "从这条回复创建分支？" }).getByRole("button", { name: "创建分支" }).click();
    title = `${title}（分支）`;
    await expect(page.getByLabel("会话标题栏").getByRole("button", { name: title, exact: true })).toBeVisible();
    await expect(page.getByRole("status").filter({ hasText: "继承 · 已完成" }).first()).toBeVisible();
    const copy = (await command("get_application_snapshot")).snapshot.value.active_sessions.find((session) => session.title === title)!;
    expect(copy).toBeDefined();
    copies.push(copy);
    const view = (await command("get_session_view", { session_id: copy.session_id })).snapshot.value;
    expect(view.runs).toHaveLength(0);
    expect(view.conversation.items.map((item) => item.message_id)).toEqual(original.conversation.items.map((item) => item.message_id));
    expect(view.conversation.items.filter((item) => item.type === "user" || item.type === "assistant").every((item) => item.inherited)).toBe(true);
  }
  // 此场景仅删除 globalSetup 新建隔离 Home 中本测试创建的会话。
  for (const session of [source, copies[0]]) {
    const prepared = await command("prepare_delete_session", { session_id: session.session_id });
    await command("delete_session", { session_id: session.session_id, confirmation_token: prepared.confirmation_token });
  }
  await page.reload();
  await navigation.getByRole("button", { name: title, exact: true }).click();
  await expect(page.getByText("离线回复：FIRST_CASE", { exact: true })).toBeVisible();
  await expect(page.getByRole("status").filter({ hasText: "继承 · 已完成" }).first()).toBeVisible();
  const composer = page.getByRole("textbox", { name: "输入消息" });
  await composer.fill("QUEUED_CASE");
  await composer.press("Enter");
  await expect(page.getByText("离线回复：QUEUED_CASE", { exact: true })).toBeVisible();
  await expect.poll(async () => {
    const view = (await command("get_session_view", { session_id: copies[1].session_id })).snapshot.value;
    return view.runs.map((run) => run.status);
  }).toEqual(["completed"]);
});
