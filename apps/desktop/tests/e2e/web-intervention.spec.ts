import { compatibilityHeaders, type RuntimeCommand, type RuntimeCommandResult } from "@ez-assistant/protocol";
import { expect, test, type Page } from "@playwright/test";

async function openSession(page: Page, title: string) {
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
  expect(app.capabilities.input_intervention).toBe(true);
  const { session } = await command("create_session", { title, workspace_id: app.workspaces[0].workspace_id });
  const session_id = session.session_id;
  const login = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  expect(login.ok).toBe(true);
  const { token } = await login.json() as { token: string };
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await page.getByRole("complementary", { name: "会话导航" }).getByRole("button", { name: title, exact: true }).click();
  const input = page.getByRole("textbox", { name: "输入消息" });
  await expect(input).toBeEditable();
  return { command, host, input, session_id, view: async () => (await command("get_session_view", { session_id })).snapshot.value };
}

test("直发同键核实后先显示可靠消息，再介入原队列项", async ({ page }) => {
  const { command, host, input, session_id, view } = await openSession(page, "F03 介入验收");
  const ordinary = await command("submit_input", { session_id, message: "C05_STREAM 原轮", variant: "build" });
  await expect.poll(async () => (await view()).active_run?.input_id).toBe(ordinary.input_id);
  await input.fill("QUEUED_CASE 待发消息");
  await input.press("Enter");
  await expect(input).toHaveValue("");
  const before = await view();
  expect(before.queue.items).toHaveLength(1);
  const queued_id = before.queue.items[0].payload.input_id;

  const attempts: unknown[] = [];
  let release_snapshot: () => void = () => {};
  const snapshot_gate = new Promise<void>(resolve => { release_snapshot = resolve; });
  let release_response: () => void = () => {};
  const response_gate = new Promise<void>(resolve => { release_response = resolve; });
  await page.route(`${host.base_url}/commands`, async route => {
    const command = route.request().postDataJSON()?.command?.payload;
    if (command?.type === "get_session_view" && attempts.length) await snapshot_gate;
    if (command?.type !== "submit_input" || command.payload?.delivery !== "intervention") {
      await route.continue();
      return;
    }
    attempts.push(command.payload);
    if (attempts.length === 1) {
      const response = await route.fetch();
      expect(response.status()).toBe(200);
      await response_gate;
      await route.abort();
    } else await route.continue();
  });

  try {
    await input.fill("C05_STREAM 介入新轮");
    await input.press("Control+Enter");
    await expect(page.getByText("正在介入…", { exact: true })).toBeVisible();
    await expect(input).toHaveValue("C05_STREAM 介入新轮");
    await expect(page.getByRole("button", { name: "发送消息" })).toBeDisabled();
    release_response();
    await expect(page.getByText("正在确认…", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "核实", exact: true }).click();
    const message = page.getByRole("article").filter({ hasText: "C05_STREAM 介入新轮" });
    await expect(message).toHaveCount(1);
    await expect(input).toHaveValue("");
    expect(attempts).toHaveLength(2);
    expect(attempts[1]).toEqual(attempts[0]);
  } finally {
    release_response();
    release_snapshot();
  }

  await page.getByRole("button", { name: "介入", exact: true }).click();
  await expect(page.getByRole("article").filter({ hasText: "QUEUED_CASE 待发消息" })).toHaveCount(1);
  await expect.poll(async () => (await command("list_runs", { session_id, input_id: queued_id, page: { limit: 1 } })).runs[0]?.status).toBe("completed");
  const after = await view();
  const messages = after.conversation.items.filter(item => item.type === "user");
  expect(messages.map(item => item.text)).toEqual(["C05_STREAM 原轮", "C05_STREAM 介入新轮", "QUEUED_CASE 待发消息"]);
  expect(messages[2].input_id).toBe(queued_id);
  for (const input_id of [ordinary.input_id, messages[1].input_id]) {
    const runs = await command("list_runs", { session_id, input_id, page: { limit: 1 } });
    expect(runs.runs[0]?.status).toBe("cancelled");
  }
  expect(after.queue.items).toHaveLength(0);
  await expect(page.getByRole("article").filter({ hasText: "C05_STREAM 介入新轮" })).toHaveCount(1);
});

test("问答保留专用入口，普通草稿仍可通过 macOS 快捷键介入", async ({ page }) => {
  await page.addInitScript(() => Object.defineProperty(navigator, "platform", { get: () => "MacIntel" }));
  const { command, input, session_id } = await openSession(page, "F03 待回答问题");
  await command("submit_input", { session_id, message: "F04_END", variant: "build" });
  const feedback = page.getByRole("region", { name: "等待你回答" });
  await expect(feedback).toBeVisible();
  await input.fill("FIRST_CASE 改成新任务");
  await expect(page.getByRole("button", { name: "发送消息" })).toHaveAttribute("title", "介入（Command + Enter）");
  await input.press("Enter");
  await expect(feedback).toBeVisible();
  await expect(input).toHaveValue("FIRST_CASE 改成新任务");
  await input.press("Meta+Enter");
  await expect(page.getByRole("article").filter({ hasText: "FIRST_CASE 改成新任务" })).toHaveCount(1);
  await expect(feedback).toHaveCount(0);
  expect((await command("list_feedback_requests", { session_id }))[0]?.status).toBe("cancelled");
});

test("新会话快捷键沿首次发送创建会话", async ({ page }) => {
  const { command, input } = await openSession(page, "F03 首次发送验收");
  const before = (await command("list_sessions", { limit: 10000 })).sessions.length;
  await page.getByRole("button", { name: "新建会话" }).click();
  await expect(page.getByRole("region", { name: "新会话项目选项" })).toBeVisible();
  await input.fill("FIRST_CASE 发送首条消息");
  await input.press("Control+Enter");
  await expect(page.getByText("离线回复：FIRST_CASE", { exact: true })).toBeVisible();
  expect((await command("list_sessions", { limit: 10000 })).sessions).toHaveLength(before + 1);
});
