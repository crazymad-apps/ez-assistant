import { type RuntimeCommand, type RuntimeCommandResult, compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test, type Locator } from "@playwright/test";

function fixture() {
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" };
  async function command<T extends RuntimeCommand["type"]>(type: T, payload: object = {}): Promise<Extract<RuntimeCommandResult, { type: T }>["payload"]> {
    const response = await fetch(`${host.base_url}/commands`, { method: "POST", headers,
      body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }) });
    const body = await response.json();
    expect(response.status, JSON.stringify(body)).toBe(200);
    return body.result.payload.payload;
  }
  return { host, headers, command };
}

test("long output keeps reading intent, pages under load, and shows child context", async ({ page }, testInfo) => {
  test.setTimeout(90_000);
  page.setDefaultTimeout(10_000);
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  const { host, headers, command } = fixture();
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
    const findRun = async () => (await command("get_session_view", { session_id })).snapshot.value.runs.find(run => run.input_id === result.input_id);
    if (wait) await expect.poll(async () => (await findRun())?.status).toBe("completed");
    else await expect.poll(async () => (await findRun())?.run_id).toBeTruthy();
    return (await findRun())!.run_id;
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
  const context = page.getByRole("tabpanel", { name: "当前上下文" });
  const child = context.getByRole("button", { name: /^E2E 子任务/ });
  await child.click();
  await expect(page.getByLabel("子任务标题栏")).toContainText("上下文");
  await expect(page.getByLabel("子任务标题栏")).toContainText("140 Token");
  await page.getByRole("button", { name: "返回主会话" }).click();
  const run_id = await submit("C05_STREAM", false);
  const outer = page.getByLabel("消息列表", { exact: true });
  const reasoning = page.getByLabel("思考内容", { exact: true }).last();
  const reasoning_toggle = outer.getByRole("button", { name: "思考过程", exact: true }).last();
  await expect(reasoning_toggle).toBeVisible();
  if (await reasoning_toggle.getAttribute("aria-expanded") === "false") await reasoning_toggle.click();
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
    const history = (await command("list_conversation_page", { owner, cursor, turn_limit: 4 })).snapshot.value;
    ids.unshift(...history.items.map((item: { message_id: string }) => item.message_id)); cursor = history.previous_cursor;
  } while (cursor);
  expect(new Set(ids).size).toBe(ids.length); expect(ids.length).toBeGreaterThan(36);
  await page.getByRole("button", { name: "返回最新", exact: true }).click();
  await expect.poll(() => gap(outer)).toBeLessThan(25);
  await expect.poll(async () => (await command("get_run", { session_id, run_id })).run.status, { timeout: 30000 }).toBe("completed");
  await expect(outer.getByText("C05 完成", { exact: false }).last()).toBeVisible();
  await child.click();
  await page.setViewportSize({ width: 720, height: 800 });
  await page.screenshot({ path: testInfo.outputPath("child-context-narrow.png") });
  expect(await page.getByLabel("子任务标题栏").evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByLabel("子任务标题栏").getByText("E2E 子任务", { exact: true })).toBeVisible();
  expect(await page.getByLabel("子任务标题栏").evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  expect(errors).toEqual([]);
});


test("F01 完整 Run 分页与聊天页顺序定位", async ({ page }) => {
  test.setTimeout(60_000);
  const { host, headers, command } = fixture();
  const app = (await command("get_application_snapshot")).snapshot.value;
  const { session } = await command("create_session", { title: "F01 完整 Run 分页", workspace_id: app.workspaces[0].workspace_id });
  const session_id = session.session_id;
  const run_ids: string[] = [];
  for (let index = 0; index < 23; index++) {
    const { input_id } = await command("submit_input", { session_id, message: `历史 ${index}`, mode: "normal", variant: "build", attachment_ids: [], quotes: [] });
    await expect.poll(async () => {
      const view = (await command("get_session_view", { session_id })).snapshot.value;
      const current = view.runs.find(run => run.input_id === input_id);
      if (current?.status === "completed") run_ids[index] = current.run_id;
      return current?.status;
    }).toBe("completed");
  }
  const owner = { type: "main_session", session_id };
  const latest = (await command("list_conversation_page", { owner, cursor: null })).snapshot.value;
  const middle = (await command("list_conversation_page", { owner, cursor: latest.previous_cursor })).snapshot.value;
  const first = (await command("list_conversation_page", { owner, cursor: middle.previous_cursor })).snapshot.value;
  expect([latest, middle, first].map(part => part.items.filter(item => item.type === "assistant").length)).toEqual([10, 10, 3]);
  expect([latest, middle, first].map(part => part.items[0]?.type === "user" ? part.items[0].text : null)).toEqual(["历史 13", "历史 3", "历史 0"]);
  expect(first.has_more).toBe(false);
  const old = await fetch(`${host.base_url}/commands`, {
    method: "POST",
    headers: { ...headers, "x-ez-client-version": "0.33.0", "x-ez-min-compatible-version": "0.33.0" },
    body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type: "list_conversation_page", payload: { owner, cursor: null, limit: 4 } } } }),
  });
  expect(old.ok).toBe(false);
  expect(await old.text()).toContain("client_too_old");

  const around = await command("get_conversation_page_around_run", { session_id, run_id: run_ids[0], turn_limit: 1 });
  expect(around.snapshot.value.items.map(item => item.message_id)).toEqual(first.items.slice(0, 2).map(item => item.message_id));
  const issued = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  expect(issued.ok).toBe(true);
  const { token } = await issued.json();
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await page.getByRole("complementary", { name: "会话导航" }).getByRole("button", { name: session.title, exact: true }).click();
  const messages = page.getByLabel("消息列表", { exact: true });
  await expect(messages.getByText("历史 22", { exact: true })).toBeVisible();
  await expect(messages.getByText("历史 0", { exact: true })).toHaveCount(0);

  const cursors: string[] = [];
  page.on("request", (request) => {
    if (request.method() !== "POST" || !request.url().endsWith("/commands")) return;
    const command = request.postDataJSON()?.command?.payload;
    if (command?.type === "list_conversation_page") cursors.push(command.payload.cursor);
  });
  const context = page.getByRole("tabpanel", { name: "当前上下文" });
  await context.getByRole("button", { name: /运行 #1.*0 个工具.*完成/ }).last().click();
  await expect(messages.getByText("历史 0", { exact: true })).toBeInViewport();
  expect(cursors).toEqual([latest.previous_cursor, middle.previous_cursor]);
  await expect(messages.getByText("历史 12", { exact: true })).toHaveCount(1);
  await expect(messages.getByText("历史 22", { exact: true })).toHaveCount(1);
  await expect(messages.getByRole("button", { name: "加载更早消息" })).toHaveCount(0);

  const stream = await command("submit_input", { session_id, message: "F02_STREAM", mode: "normal", variant: "build", attachment_ids: [], quotes: [] });
  await expect(messages.getByText("F02_STREAM", { exact: true })).toHaveCount(1);
  await expect(messages.getByText("历史 0", { exact: true })).toBeInViewport();
  await page.getByRole("button", { name: "返回最新", exact: true }).click();
  await expect.poll(() => messages.evaluate(node => node.scrollHeight - node.clientHeight - node.scrollTop)).toBeLessThan(25);
  await expect.poll(async () => (await command("get_session_view", { session_id })).snapshot.value.runs.find(run => run.input_id === stream.input_id)?.status).toBe("completed");
  await expect(messages.getByText("历史 0", { exact: true })).toHaveCount(1);
  await expect(messages.getByText("F02_STREAM", { exact: true })).toHaveCount(1);
});

test("F01 M1 Host returns paired IDs and bounded previews for authorized owners", async () => {
  const { host, headers, command } = fixture();
  const app = (await command("get_application_snapshot")).snapshot.value;
  const { session } = await command("create_session", {
    title: "F01 M1 缩略读取", workspace_id: app.workspaces[0].workspace_id,
  });
  const owner = { type: "main_session", session_id: session.session_id };
  await command("set_session_approval_mode", { session_id: session.session_id, approval_mode: "auto" });
  const readView = async (session_id: string) => (await command("get_session_view", { session_id })).snapshot.value;
  const submit = async (session_id: string, message: string, attachment_ids: string[] = []) => {
    const { input_id } = await command("submit_input", { session_id, message, mode: "normal", variant: "build", attachment_ids, quotes: [] });
    await expect.poll(async () => (await readView(session_id)).runs.find(run => run.input_id === input_id)?.status).toBe("completed");
  };
  const unicode = "树".repeat(159) + "🪴";
  await submit(session.session_id, unicode + "尾");
  await submit(session.session_id, "TOOL_CASE");
  await submit(session.session_id, "DELEGATE_CASE");
  await command("submit_session_command", { session_id: session.session_id, command: { type: "skill_refresh" } });
  await expect.poll(async () => (await readView(session.session_id)).conversation.items.some(item => item.type === "skill_refresh_result")).toBe(true);
  const main = await readView(session.session_id);
  const navigation = (await command("list_conversation_navigation", { owner, cursor: null })).snapshot.value;
  expect(navigation.items).toHaveLength(3);
  for (const item of navigation.items) {
    expect(Object.keys(item).sort()).toEqual(["assistant_message_id", "user_message_id"]);
  }
  const preview = async (message_id: string, generation = navigation.generation) => (await command("get_conversation_message_preview", {
    owner, generation, message_id,
  })).snapshot.value;
  expect(await preview(navigation.items[0].user_message_id)).toMatchObject({ text: unicode, truncated: true });
  expect(await preview(navigation.items[1].assistant_message_id!)).toMatchObject({ text: "工具执行完成。", truncated: false });
  const rejected = async (payload: object, code: string) => {
    const res = await fetch(`${host.base_url}/commands`, {
      method: "POST", headers, body: JSON.stringify({ request_id: crypto.randomUUID(),
        command: { scope: "runtime", payload: { type: "get_conversation_message_preview", payload } } }),
    });
    expect(res.ok).toBe(false);
    expect((await res.json()).error.code).toBe(code);
  };
  const intermediate = main.conversation.items.find(item => item.type === "assistant"
    && item.segments.some(segment => segment.type === "tool_group"));
  const control = main.conversation.items.find(item => item.type === "skill_refresh_result");
  expect(intermediate).toBeDefined();
  expect(control).toBeDefined();
  for (const message_id of [intermediate!.message_id, control!.message_id]) {
    await rejected({ owner, generation: navigation.generation, message_id }, "invalid_request");
  }
  await rejected({ owner, generation: navigation.generation + 1, message_id: navigation.items[0].user_message_id }, "snapshot_stale");

  const child = main.child_tasks[0].task;
  const child_owner = { type: "child_task", session_id: session.session_id, child_task_id: child.child_task_id };
  const child_view = (await command("get_child_task_view", { session_id: session.session_id, child_task_id: child.child_task_id })).snapshot.value;
  const child_page = (await command("list_conversation_navigation", { owner: child_owner, cursor: null })).snapshot.value;
  expect(child_page.owner).toEqual(child_owner);
  expect(child_page.items.map(item => item.user_message_id)).toEqual(child_view.conversation.items
    .filter(item => item.type === "user").map(item => item.message_id));
  await rejected({ owner: child_owner, generation: child_page.generation, message_id: navigation.items[0].user_message_id }, "invalid_request");

  const attached = (await command("list_sessions", { query: "M2 临时会话" })).sessions.find(item => item.title === "M2 临时会话")!;
  const attached_owner = { type: "main_session", session_id: attached.session_id };
  const attachments = (await readView(attached.session_id)).attachments;
  expect(attachments).toHaveLength(1);
  await submit(attached.session_id, "", [attachments[0].attachment_id]);
  const attached_page = (await command("list_conversation_navigation", { owner: attached_owner, cursor: null })).snapshot.value;
  expect(attached_page.items).toHaveLength(1);
  expect((await command("get_conversation_message_preview", {
    owner: attached_owner, generation: attached_page.generation, message_id: attached_page.items[0].user_message_id,
  })).snapshot.value).toMatchObject({ text: "", truncated: false });
});

test("F01 M2 previews independently, retries a failed page, and locates the selected input", async ({ page }, info) => {
  test.setTimeout(60_000);
  await page.setViewportSize({ width: 2100, height: 800 });
  await page.emulateMedia({ reducedMotion: "reduce" });
  const { host, headers, command } = fixture();
  const app = (await command("get_application_snapshot")).snapshot.value;
  const { session } = await command("create_session", {
    title: "F01 M2 缩略导航", workspace_id: app.workspaces[0].workspace_id,
  });
  const session_id = session.session_id;
  const owner = { type: "main_session", session_id };
  const submit = async (message: string) => {
    const { input_id } = await command("submit_input", { session_id, message, mode: "normal", variant: "build", attachment_ids: [], quotes: [] });
    await expect.poll(async () => (await command("get_session_view", { session_id })).snapshot.value.runs.find(run => run.input_id === input_id)?.status).toBe("completed");
  };
  await command("set_session_approval_mode", { session_id, approval_mode: "auto" });
  await submit("TOOL_CASE");
  for (let index = 1; index < 22; index++) await submit(`阅读条目 ${index} FIRST_CASE`);
  const items = (await command("list_conversation_navigation", { owner, cursor: null })).snapshot.value.items;
  const first = items[0];
  expect(first.assistant_message_id).toBeTruthy();

  const issued = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  const { token } = await issued.json();
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await page.getByRole("complementary", { name: "会话导航" }).getByRole("button", { name: session.title, exact: true }).click();
  const messages = page.getByLabel("消息列表", { exact: true });
  const nav = page.getByRole("navigation", { name: "消息导航" });
  const marker = (id: string) => nav.locator(`[data-navigation-message-id="${id}"]`);
  const first_marker = marker(first.user_message_id);
  const preview = page.getByRole("region", { name: "消息预览" });
  const scroll = () => messages.evaluate(node => node.scrollTop);
  await expect.poll(() => messages.evaluate(node => node.firstElementChild!.getBoundingClientRect().left
    - node.getBoundingClientRect().left)).toBeGreaterThanOrEqual(88);
  await expect(nav.locator("[data-navigation-message-id]")).toHaveCount(22);
  const nav_box = (await nav.boundingBox())!;
  const body_box = (await messages.boundingBox())!;
  const area_box = (await page.locator("[data-conversation-area]").boundingBox())!;
  expect(nav_box.height).toBe(22 * 12);
  expect(nav_box.y + nav_box.height / 2).toBeCloseTo((body_box.y + area_box.y + area_box.height) / 2);
  const marker_box = (await first_marker.boundingBox())!;
  const next_marker_box = (await marker(items[1].user_message_id).boundingBox())!;
  expect(marker_box.height).toBe(12);
  expect(next_marker_box.y - marker_box.y).toBeCloseTo(12);
  const latest_top = await scroll();
  await expect(messages.getByText("TOOL_CASE", { exact: true })).toHaveCount(0);
  expect(latest_top).toBeGreaterThan(0);

  let failed_preview = false;
  let release_preview_failure!: () => void;
  const preview_failure = new Promise<void>(resolve => { release_preview_failure = resolve; });
  let fail_second_page = true;
  const preview_ids: string[] = [];
  const body_cursors: string[] = [];
  await page.route("**/commands", async route => {
    const request = route.request().postDataJSON();
    const call = request?.command?.payload;
    if (call?.type === "get_conversation_message_preview") {
      preview_ids.push(call.payload.message_id);
      if (call.payload.message_id === first.assistant_message_id && !failed_preview) {
        failed_preview = true;
        await preview_failure;
        return route.fulfill({ status: 400, json: { request_id: request.request_id, error: { code: "invalid_request", message: "预览故障夹具" } } });
      }
    }
    if (call?.type === "list_conversation_page") {
      body_cursors.push(call.payload.cursor);
      if (body_cursors.length === 2 && fail_second_page) {
        fail_second_page = false;
        return route.fulfill({ status: 400, json: { request_id: request.request_id, error: { code: "invalid_request", message: "正文故障夹具" } } });
      }
    }
    return route.continue();
  });
  const assistant_preview = preview.getByRole("group", { name: "助手最后回复" });
  try {
    await first_marker.hover();
    await expect(preview.getByRole("group", { name: "用户输入" })).toHaveText("TOOL_CASE");
    await expect(assistant_preview.getByRole("status").locator("[aria-hidden='true'] > span")).toHaveCount(3);
    await expect(assistant_preview.getByText("正在加载…")).toHaveCSS("clip-path", "inset(50%)");
  } finally {
    release_preview_failure();
  }
  await expect(preview.getByText("回复预览加载失败")).toBeVisible();
  await preview.getByRole("button", { name: "重试" }).hover();
  await expect(preview).toBeVisible();
  await preview.getByRole("button", { name: "重试" }).click();
  await expect(preview.getByRole("group", { name: "助手最后回复" })).toHaveText("工具执行完成。");
  expect(preview_ids.filter(id => id === first.user_message_id)).toHaveLength(1);
  expect(preview_ids.filter(id => id === first.assistant_message_id)).toHaveLength(2);
  expect(body_cursors).toHaveLength(0);
  expect(await scroll()).toBe(latest_top);
  const preview_box = (await preview.boundingBox())!;
  expect(preview_box.x - marker_box.x - marker_box.width).toBeCloseTo(12);
  expect(preview_box.y).toBeGreaterThanOrEqual(body_box.y);
  expect(preview_box.y + preview_box.height).toBeLessThanOrEqual(body_box.y + body_box.height);
  await page.screenshot({ path: info.outputPath("navigation-preview.png"), animations: "disabled" });

  await page.keyboard.press("Escape");
  await expect(preview).toBeHidden();
  await expect(nav.locator("button[tabindex='0']")).toHaveCount(1);
  await first_marker.focus();
  await first_marker.press("ArrowDown");
  await expect(marker(items[1].user_message_id)).toBeFocused();
  await expect(preview.getByRole("group", { name: "用户输入" })).toHaveText("阅读条目 1 FIRST_CASE");
  await page.keyboard.press("Home");
  await expect(first_marker).toBeFocused();
  await page.keyboard.press("Space");
  await expect(preview.getByText(/定位失败/)).toBeVisible();
  await expect(messages.getByText("阅读条目 3 FIRST_CASE", { exact: true })).toHaveCount(1);
  await expect(messages.getByText("TOOL_CASE", { exact: true })).toHaveCount(0);
  expect(body_cursors).toHaveLength(2);
  await first_marker.press("ArrowRight");
  await expect(preview.getByRole("button", { name: "重试" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(preview).toBeHidden();
  await expect(first_marker).toBeFocused();
  await expect(nav.locator("[aria-current], [aria-selected], [aria-pressed]")).toHaveCount(0);
  await expect(messages.locator(`[data-message-id="${first.user_message_id}"]`)).toHaveAttribute("data-navigation-highlight", "true");
  await expect(messages.getByText("TOOL_CASE", { exact: true })).toBeInViewport();
  await expect(messages.getByText("阅读条目 21 FIRST_CASE", { exact: true })).toHaveCount(1);
  expect(body_cursors).toHaveLength(3);
  expect(body_cursors[2]).toEqual(body_cursors[1]);

  await submit("浏览期间的新消息 FIRST_CASE");
  await expect(nav.locator("[data-navigation-message-id]")).toHaveCount(23);
  await expect(messages.getByText("TOOL_CASE", { exact: true })).toBeInViewport();
  await page.getByRole("button", { name: "返回最新", exact: true }).click();
  await expect(messages.getByText("浏览期间的新消息 FIRST_CASE", { exact: true })).toBeInViewport();

  await first_marker.focus();
  await page.setViewportSize({ width: 830, height: 800 });
  await expect(nav).toBeHidden();
  await expect(messages).toBeFocused();
  await page.setViewportSize({ width: 2100, height: 800 });
  await expect(nav.locator("[data-navigation-message-id]")).toHaveCount(23);
  await expect(preview).toBeHidden();

  await page.getByRole("button", { name: "收起资源栏", exact: true }).click();
  await page.getByRole("button", { name: "收起会话栏", exact: true }).click();
  await expect(page.getByRole("button", { name: "展开会话栏", exact: true })).toBeVisible();
  const leftGap = () => messages.evaluate(node => node.firstElementChild!.getBoundingClientRect().left
    - node.getBoundingClientRect().left);
  const width = page.viewportSize()!.width;
  const reference_gap = await leftGap();
  for (const [gap, visible] of [[79, false], [84, false], [88, true], [84, true], [120, true]] as const) {
    await page.setViewportSize({ width: Math.round(width + (gap - reference_gap) * 2), height: 800 });
    if (visible) await expect(nav).toBeVisible();
    else await expect(nav).toBeHidden();
    await expect.poll(leftGap).toBeCloseTo(gap);
  }
  await page.setViewportSize({ width, height: 800 });
  await page.getByRole("button", { name: "展开会话栏", exact: true }).click();
  await page.getByRole("button", { name: "展开资源栏", exact: true }).click();

  const surfaces = new Set<string>();
  const lineWidth = (id: string) => marker(id).evaluate(node => node.firstElementChild!.getBoundingClientRect().width);
  const lines = nav.locator("[data-navigation-message-id] > span:first-child");
  const peak_id = items[8].user_message_id;
  const peak_marker = marker(peak_id);
  for (const [name, theme] of [
    ["默认浅色", "default_light"], ["Monokai", "monokai"], ["Solarized Light", "solarized_light"], ["Solarized Dark", "solarized_dark"],
  ]) {
    await page.getByRole("button", { name: "账号与 Runtime", exact: true }).click();
    await page.getByRole("menuitem", { name: "设置", exact: true }).click();
    await page.getByRole("button", { name: "外观", exact: true }).click();
    await page.getByRole("radio", { name, exact: true }).check();
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await page.getByRole("button", { name: "关闭设置", exact: true }).click();
    await expect(page.getByRole("dialog", { name: "设置", exact: true })).toBeHidden();
    await peak_marker.hover();
    await expect(preview.getByRole("group", { name: "用户输入" })).toHaveText("阅读条目 8 FIRST_CASE");
    expect(await peak_marker.evaluate(node => getComputedStyle(node).backgroundColor)).toBe("rgba(0, 0, 0, 0)");
    expect(await lineWidth(peak_id)).toBe(32);
    const peak_color = await peak_marker.evaluate(node => getComputedStyle(node).color);
    const colors = await lines.evaluateAll(nodes => nodes.map(node => getComputedStyle(node).backgroundColor));
    expect(colors[8]).toBe(peak_color);
    expect(colors.filter(color => color === peak_color)).toHaveLength(1);
    surfaces.add(await preview.evaluate(node => getComputedStyle(node).backgroundColor));
    await page.screenshot({ path: info.outputPath(`navigation-${theme}.png`), animations: "disabled" });
  }
  expect(surfaces.size).toBe(4);

  const nearby = items.slice(4, 13);
  const widths = () => Promise.all(nearby.map(item => lineWidth(item.user_message_id)));
  await expect.poll(widths).toEqual([8, 15, 19, 24, 32, 24, 19, 15, 8]);
  await page.mouse.move(0, 0);
  await expect.poll(() => lines.evaluateAll(nodes => nodes.map(node => node.getBoundingClientRect().width)))
    .toEqual(Array(23).fill(8));
  expect(await lines.evaluateAll(nodes => new Set(nodes.map(node => getComputedStyle(node).backgroundColor)).size)).toBe(1);
});

test("F01 M2 bounds long navigation DOM, preserves the page anchor and reads evicted IDs again", async ({ page }) => {
  await page.setViewportSize({ width: 1880, height: 760 });
  const { host, headers, command } = fixture();
  const app = (await command("get_application_snapshot")).snapshot.value;
  const { session } = await command("create_session", {
    title: "F01 M2 长导航", workspace_id: app.workspaces[0].workspace_id,
  });
  const session_id = session.session_id;
  const owner = { type: "main_session", session_id };
  const { input_id } = await command("submit_input", { session_id, message: "长列表正文基准", mode: "normal", variant: "build", attachment_ids: [], quotes: [] });
  await expect.poll(async () => (await command("get_session_view", { session_id })).snapshot.value.runs.find(run => run.input_id === input_id)?.status).toBe("completed");
  const baseline = await command("list_conversation_navigation", { owner, cursor: null });
  const starts: number[] = [];
  // 用正式 Host 的 owner／代次和响应结构扩大 ID 列表，隔离验证客户端窗口边界。
  await page.route("**/commands", async route => {
    const request = route.request().postDataJSON();
    const call = request?.command?.payload;
    if (call?.type !== "list_conversation_navigation" || call.payload.owner.session_id !== session_id) return route.continue();
    const start = call.payload.cursor ? Number(call.payload.cursor) : 10_000 - 128;
    starts.push(start);
    return route.fulfill({ json: { request_id: request.request_id, result: { scope: "runtime", payload: {
      type: "list_conversation_navigation",
      payload: { snapshot: { ...baseline.snapshot, value: { ...baseline.snapshot.value,
        items: Array.from({ length: 128 }, (_, index) => ({ user_message_id: `fixture-${start + index}`, assistant_message_id: null })),
        previous_cursor: start > 0 ? String(start - 128) : null,
        next_cursor: start + 128 < 10_000 ? String(start + 128) : null,
      } } },
    } } } });
  });
  const issued = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  const { token } = await issued.json();
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await page.getByRole("complementary", { name: "会话导航" }).getByRole("button", { name: session.title, exact: true }).click();
  const nav = page.getByRole("navigation", { name: "消息导航" });
  const messages = page.getByLabel("消息列表", { exact: true });
  const marker = (index: number) => nav.locator(`[data-navigation-message-id="fixture-${index}"]`);
  await expect(marker(9999)).toBeInViewport();
  for (const height of [760, 580]) {
    await page.setViewportSize({ width: 1880, height });
    await expect(marker(9999)).toBeInViewport();
    const nav_box = (await nav.boundingBox())!;
    const body_box = (await messages.boundingBox())!;
    const area_box = (await page.locator("[data-conversation-area]").boundingBox())!;
    const content_height = area_box.y + area_box.height - body_box.y;
    expect(nav_box.height / content_height).toBeCloseTo(0.6, 3);
    expect((nav_box.y - body_box.y) / content_height).toBeCloseTo(0.2, 3);
  }
  await nav.hover();
  expect(await nav.evaluate(node => ({
    width: getComputedStyle(node).scrollbarWidth,
    has_gutter: node.offsetWidth !== node.clientWidth,
    scrollable: node.scrollHeight > node.clientHeight,
  }))).toEqual({ width: "none", has_gutter: false, scrollable: true });
  await marker(9999).focus();
  for (let page_number = 1; page_number <= 4; page_number++) {
    const old_first = 9872 - (page_number - 1) * 128;
    await nav.hover();
    await page.mouse.wheel(0, -20000);
    await expect.poll(() => starts.includes(old_first - 128)).toBe(true);
    await expect(marker(old_first)).toBeInViewport();
    await expect.poll(() => marker(old_first).evaluate(el => {
      const bounds = el.closest("nav")!.getBoundingClientRect();
      return Math.abs(el.getBoundingClientRect().top - bounds.top);
    })).toBeLessThan(2);
    const render_limit = Math.ceil(await nav.evaluate(node => node.clientHeight) / 12) + 18;
    expect(await nav.locator("[data-navigation-message-id]").count()).toBeLessThanOrEqual(render_limit);
  }
  expect(await page.evaluate(() => document.activeElement?.getAttribute("data-navigation-message-id"))).toMatch(/^fixture-/);
  await nav.hover();
  await page.mouse.wheel(0, 20000);
  await expect.poll(() => starts.filter(start => start === 9872).length).toBe(2);
  await expect(marker(9871)).toBeInViewport();
  await expect(marker(9999)).toHaveCount(0);
  await page.mouse.wheel(0, 20000);
  await expect(marker(9999)).toBeInViewport();
  await marker(9999).press("Home");
  await expect(marker(9488)).toBeFocused();
  await page.keyboard.press("ArrowUp");
  await expect(marker(9487)).toBeFocused();
  await page.keyboard.press("End");
  await expect(marker(9999)).toBeFocused();
  await expect(messages.getByText("长列表正文基准", { exact: true })).toBeInViewport();
});
