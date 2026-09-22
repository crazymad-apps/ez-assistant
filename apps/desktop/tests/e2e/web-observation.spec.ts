import { type RuntimeCommand, type RuntimeCommandResult, compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test, type Locator } from "@playwright/test";

test("long output keeps reading intent, pages under load, and shows child context", async ({ page }, testInfo) => {
  test.setTimeout(90_000);
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" };
  async function command<T extends RuntimeCommand["type"]>(type: T, payload: object = {}): Promise<Extract<RuntimeCommandResult, { type: T }>["payload"]> {
    const response = await fetch(`${host.base_url}/commands`, { method: "POST", headers,
      body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }) });
    const body = await response.json();
    expect(response.status, JSON.stringify(body)).toBe(200);
    return body.result.payload.payload;
  }
  const app = (await command("get_application_snapshot")).snapshot.value;
  const provider = await command("create_provider", { connection: { ...app.providers[0].connection, display_name: "C05 reasoning", provider_type: "vllm", discovery_format: "vllm" }, credential: { mode: "replace", value: "isolated-fixture" } });
  await command("refresh_provider_models", { provider_instance_id: provider.provider_instance_id });
  const selection = { provider_instance_id: provider.provider_instance_id, model_id: "offline-model" };
  await command("save_model_fixed_config", { selection, parameters: {
    context_window_tokens: { state: "known", value: 16384 }, max_output_tokens: { state: "known", value: 4096 },
    max_input_tokens: { state: "unknown" }, reasoning_max_input_tokens: { state: "unknown" }, reasoning_max_output_tokens: { state: "unknown" },
    streaming: "supported", image_input: "unsupported", tool_calls: "supported", reasoning: "supported",
    tool_choice: { auto: "supported", none: "supported", required: "supported", named: "supported" },
    tool_image_projection: "unsupported", reasoning_mode: "always", reasoning_efforts: {}, default_reasoning_effort: null,
  } });
  const session_id = (await command("create_session", { title: "C05 阅读验收", model_selection: selection, workspace_id: app.workspaces[0].workspace_id })).session.session_id;
  async function submit(message: string, wait = true) {
    const result = await command("submit_input", { session_id, message, mode: "normal", variant: "build", attachment_ids: [], quotes: [] });
    if (wait) await expect.poll(async () => (await command("get_run", { session_id, run_id: result.run.run_id })).run.status).toBe("completed");
    return result.run.run_id;
  }
  for (let index = 0; index < 18; index++) await submit(`历史 ${index}`);
  await command("set_session_approval_mode", { session_id, approval_mode: "auto" });
  await submit("DELEGATE_CASE");
  const view = (await command("get_session_view", { session_id })).snapshot.value;
  expect(view.child_tasks[0].usage.context).toMatchObject({ used_tokens: 140, window_tokens: 16384 });
  const issued = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  const { token } = await issued.json();
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await page.getByText("C05 阅读验收", { exact: true }).first().click();
  await page.getByRole("button", { name: "查看子任务：E2E 子任务" }).click();
  await expect(page.getByLabel("子任务标题栏")).toContainText("上下文");
  await expect(page.getByLabel("子任务标题栏")).toContainText("140 Token");
  await page.getByRole("button", { name: "返回主会话" }).click();
  const run_id = await submit("C05_STREAM", false);
  const outer = page.getByLabel("消息列表", { exact: true });
  const reasoning = page.getByLabel("思考内容", { exact: true }).last();
  const gap = (node: Locator) => node.evaluate(el => el.scrollHeight - el.clientHeight - el.scrollTop);
  await expect(reasoning).toBeVisible();
  await expect.poll(() => reasoning.evaluate(el => el.scrollHeight)).toBeGreaterThan(500);
  await expect.poll(() => gap(reasoning)).toBeLessThan(25);
  await reasoning.hover(); await page.mouse.wheel(0, -400);
  await expect.poll(() => gap(reasoning)).toBeGreaterThan(200);
  const inner_top = await reasoning.evaluate(el => el.scrollTop);
  await page.waitForTimeout(300);
  expect(Math.abs(await reasoning.evaluate(el => el.scrollTop) - inner_top)).toBeLessThan(3);
  await expect.poll(() => gap(outer)).toBeLessThan(25);
  await reasoning.focus(); await page.keyboard.press("End");
  await expect.poll(() => gap(reasoning)).toBeLessThan(25);
  // 历史分页与输出并发，保持所读消息的相对位置。
  await outer.hover({ position: { x: 12, y: 40 } }); await page.mouse.wheel(0, -100000);
  await expect(outer.getByText("历史 0", { exact: true })).toBeVisible();
  const first_top = await outer.getByText("历史 0", { exact: true }).evaluate(el => el.getBoundingClientRect().top);
  await page.waitForTimeout(400);
  expect(Math.abs(await outer.getByText("历史 0", { exact: true }).evaluate(el => el.getBoundingClientRect().top) - first_top)).toBeLessThan(3);
  const owner = { type: "main_session", session_id };
  let cursor: string | null = null;
  const ids: string[] = [];
  do {
    const history = (await command("list_conversation_page", { owner, cursor, limit: 4 })).snapshot.value;
    ids.unshift(...history.items.map((item: { message_id: string }) => item.message_id)); cursor = history.previous_cursor;
  } while (cursor);
  expect(new Set(ids).size).toBe(ids.length); expect(ids.length).toBeGreaterThan(36);
  await page.getByRole("button", { name: "回到底部", exact: true }).click();
  await expect.poll(() => gap(outer)).toBeLessThan(25);
  await expect.poll(async () => (await command("get_run", { session_id, run_id })).run.status, { timeout: 30000 }).toBe("completed");
  await expect(outer.getByText("C05 完成", { exact: false }).last()).toBeVisible();
  await page.setViewportSize({ width: 720, height: 800 });
  await page.getByRole("button", { name: "查看子任务：E2E 子任务" }).scrollIntoViewIfNeeded();
  await page.getByRole("button", { name: "查看子任务：E2E 子任务" }).click();
  await page.screenshot({ path: testInfo.outputPath("child-context-narrow.png") });
  expect(await page.getByLabel("子任务标题栏").evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByLabel("子任务标题栏").getByText("E2E 子任务", { exact: true })).toBeVisible();
  expect(await page.getByLabel("子任务标题栏").evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  expect(errors).toEqual([]);
});
