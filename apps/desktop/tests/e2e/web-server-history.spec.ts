import { compatibilityHeaders, type ProviderSummary, type GetSessionViewResult, type ListConversationNavigationResult } from "@ez-assistant/protocol";
import { expect, test } from "@playwright/test";
import { spawn, type ChildProcess } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join, resolve } from "node:path";

type Discovery = { address: string; access_token: string };
type Wire = { store: boolean; previous_response_id?: string; input: { type?: string; role?: string; content?: unknown; output?: unknown; name?: string; call_id?: string }[]; model: string };

// 新建一次性 SQLite 和消息目录，通过正式 UI/API 写入；不连接开发者 Runtime Home。
for (const milestone of ["m0", "m1"] as const) test(`personal Responses history ${milestone}: tools, isolation and real Host restart`, async ({ page }) => {
  test.setTimeout(150_000);
  page.setDefaultTimeout(10_000);
  const root = resolve(process.env.EZ_ASSISTANT_E2E_TEST_ROOT ?? `../../.runtime-test/f06-${milestone}`);
  await mkdir(root, { recursive: true });
  const home = await mkdtemp(join(root, "runtime-"));
  const os_home = await mkdtemp(join(root, "os-home-"));
  const workspace = await mkdtemp(join(root, "workspace-"));
  await writeFile(join(workspace, "fixture.txt"), "F06_READ_ONCE\n");
  const calls: Wire[] = [];
  let sequence = 0;
  let rejectNext: "missing" | "ordinary" | undefined;
  let rejectToolResult = false;
  let rejectChildResults = false;
  const childToolIds = new Set<string>();
  const responses = new Map<Wire, string>();
  const rejected: Wire[] = [];
  let readFileCalls = 0;
  const provider = createServer(async (request, response) => {
    if (request.method !== "POST" || request.url !== "/v1/responses") { response.writeHead(404).end(); return; }
    let raw = "";
    for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw) as Wire;
    calls.push(body);
    const toolResult = body.input.some(item => item.type === "function_call_output");
    const missing = body.previous_response_id && (rejectNext === "missing" || (rejectToolResult && toolResult) || (rejectChildResults && childToolIds.delete(body.previous_response_id)));
    if (missing || rejectNext === "ordinary") {
      const code = missing ? "previous_response_not_found" : "unknown_invalid_request";
      rejected.push(body);
      rejectNext = undefined;
      if (missing && rejectToolResult) rejectToolResult = false;
      response.writeHead(400, { "Content-Type": "application/json" }).end(JSON.stringify({ error: { message: "fixture rejected request", code, type: "invalid_request_error", param: "previous_response_id" } }));
      return;
    }
    const id = `f06-response-${++sequence}`;
    responses.set(body, id);
    const lastUser = JSON.stringify(body.input.filter(item => item.role === "user").at(-1));
    const childTask = body.store && !toolResult && lastUser?.includes("F06_CHILD_");
    const delegates = body.store && !toolResult && lastUser?.includes("F06_DELEGATE");
    const tool = body.store && !toolResult && (childTask || lastUser?.includes("F06_TOOLS"));
    const usage = body.model === "auto-model" && lastUser?.includes("F06_AUTO_FIRST") ? { input_tokens: 120000, output_tokens: 3, total_tokens: 120003 } : { input_tokens: 10, output_tokens: 3, total_tokens: 13 };
    const items = delegates ? [1, 2].map(index => ({ id: `fc-${sequence}-${index}`, type: "function_call", call_id: `call-${sequence}-${index}`, name: "delegate_task", arguments: JSON.stringify({ title: `child ${index}`, task: `F06_CHILD_${index}: read fixture.txt once`, context: null, expected_output: "F06_OK" }) }))
      : tool ? [{ id: `fc-${sequence}`, type: "function_call", call_id: `call-${sequence}`, name: "read_file", arguments: JSON.stringify({ path: "fixture.txt" }) }]
      : [{ id: `msg-${sequence}`, type: "message", role: "assistant", content: [{ type: "output_text", text: "F06_OK", annotations: [] }] }];
    if (tool) readFileCalls++;
    if (childTask) childToolIds.add(id);
    const events: object[] = [{ type: "response.created", response: { id, model: body.model, status: "in_progress" } }];
    items.forEach((item, index) => {
      const functionCall = item.type === "function_call";
      events.push({ type: "response.output_item.added", output_index: index, item: functionCall ? { ...item, arguments: "" } : { ...item, content: [] } });
      events.push(functionCall ? { type: "response.function_call_arguments.done", output_index: index, item_id: item.id, arguments: "arguments" in item ? item.arguments : "" }
        : { type: "response.output_text.done", output_index: index, content_index: 0, item_id: item.id, text: "F06_OK" });
      events.push({ type: "response.output_item.done", output_index: index, item });
    });
    events.push({ type: "response.completed", response: { id, model: body.model, status: "completed", usage } });
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    for (const event of events) response.write(`data: ${JSON.stringify(event)}\n\n`);
    response.end("data: [DONE]\n\n");
  });
  await new Promise<void>(done => provider.listen(0, "127.0.0.1", done));
  const address = provider.address();
  if (!address || typeof address === "string") throw new Error("provider address absent");
  const reservation = createServer();
  await new Promise<void>(done => reservation.listen(0, "127.0.0.1", done));
  const reserved = reservation.address();
  if (!reserved || typeof reserved === "string") throw new Error("Host port absent");
  await new Promise<void>(done => reservation.close(() => done()));
  await writeFile(join(home, "config.toml"), `schema_version = 1\n[host_access]\nremote_enabled = false\nscheme = "http"\nport = ${reserved.port}\nserver_names = []\n`);
  let child: ChildProcess | undefined;
  let discovery: Discovery;
  let stderr = "";
  async function start() {
    child = spawn(resolve("../../target/debug/ez-assistant-runtime"), ["serve", "--runtime-home", home], {
      env: { ...process.env, HOME: os_home }, stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", chunk => { stderr += String(chunk); });
    await expect.poll(async () => {
      if (child?.exitCode !== null) throw new Error(stderr);
      try {
        const found = JSON.parse(await readFile(join(home, "run/runtime.json"), "utf8")) as Discovery;
        const ready = await fetch(`${found.address}/health`, { headers: headers(found) });
        if (ready.ok && (await ready.json() as { status: string }).status === "ready") { discovery = found; return true; }
      } catch { /* startup admission is still in progress */ }
      return false;
    }, { timeout: 15_000 }).toBe(true);
  }
  function headers(found = discovery) { return { ...compatibilityHeaders(), Authorization: `Bearer ${found.access_token}`, "Content-Type": "application/json" }; }
  async function command<T = unknown>(type: string, payload: object): Promise<T> {
    const response = await fetch(`${discovery.address}/commands`, { method: "POST", headers: headers(), body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }) });
    const body = await response.json() as { result: { payload: { payload: T } } };
    expect(response.ok, JSON.stringify(body)).toBe(true);
    return body.result.payload.payload;
  }
  async function stop() {
    await command("shutdown_runtime", {});
    await expect.poll(() => child?.exitCode, { timeout: 10_000 }).not.toBeNull();
  }
  async function login() {
    const response = await fetch(`${discovery.address}/auth/login`, { method: "POST", headers: headers(), body: JSON.stringify({ method: "desktop" }) });
    expect(response.ok).toBe(true);
    const { token } = await response.json();
    await page.goto(`${discovery.address}/#token=${encodeURIComponent(token)}`);
  }
  async function open() {
    await login();
    await page.getByRole("button", { name: "账号与 Runtime" }).click();
    await page.getByRole("menuitem", { name: "设置" }).click();
    await page.getByRole("dialog", { name: "设置", exact: true }).getByRole("button", { name: "模型", exact: true }).click();
  }
  try {
    await start();
    await page.emulateMedia({ reducedMotion: "reduce" });
    await open();
    const dialog = page.getByRole("dialog", { name: "设置", exact: true });
    await dialog.getByRole("button", { name: "添加服务商", exact: true }).click();
    await dialog.getByLabel("服务商名称", { exact: true }).fill("F06 fixture");
    await dialog.getByLabel("服务地址", { exact: true }).fill(`http://127.0.0.1:${address.port}/v1`);
    await dialog.getByLabel("API Key", { exact: true }).fill("fixture-only-secret");
    const toggle = dialog.getByRole("checkbox", { name: "使用服务端历史续接" });
    await expect(toggle).toBeDisabled();
    await dialog.getByRole("button", { name: "接口选项", exact: true }).click();
    await dialog.getByRole("button", { name: "续接能力", exact: true }).click();
    await page.getByRole("option", { name: "明确支持", exact: true }).click();
    await expect(toggle).toBeEnabled();
    await toggle.press("Space");
    await expect(toggle).toBeChecked();
    await dialog.screenshot({ path: test.info().outputPath("server-history-settings.png") });
    await dialog.getByRole("button", { name: "保存服务商", exact: true }).click();
    await expect(dialog.getByLabel("服务地址", { exact: true })).toHaveCount(0);
    const [saved] = await command<ProviderSummary[]>("list_providers", {});
    expect(saved.connection.server_history_enabled).toBe(true);
    expect(saved.connection.server_history_support_override).toBe("supported");
    const selection = { provider_instance_id: saved.provider_instance_id, model_id: "gpt-4.1" };
    const parameters = {
      context_window_tokens: { state: "known", value: 128000 }, max_output_tokens: { state: "known", value: 4096 },
      max_input_tokens: { state: "unknown" }, reasoning_max_input_tokens: { state: "unknown" }, reasoning_max_output_tokens: { state: "unknown" },
      streaming: "supported", image_input: "unsupported", tool_calls: "supported", reasoning: "unsupported",
      tool_choice: { auto: "supported", none: "supported", required: "supported", named: "supported" },
      tool_image_projection: "unsupported", reasoning_mode: "unsupported", reasoning_efforts: {}, default_reasoning_effort: null,
    };
    await command("save_model_fixed_config", { selection, origin: "manual", parameters });
    await command("set_default_model", { selection });
    const registered = await command<{ workspace: { workspace_id: string } }>("register_workspace", { label: "F06", primary_directory: workspace, additional_directories: [] });
    const session_title = "F06 原始会话";
    const created = await command<{ session: { session_id: string } }>("create_session", { title: session_title, workspace_id: registered.workspace.workspace_id, model_selection: null });
    const session_id = created.session.session_id;
    async function run(message: string, target = session_id, status = "completed") {
      const submitted = await command<{ input_id: string }>("submit_input", { session_id: target, message, mode: "normal", variant: "build" });
      const findRun = async () => (await command<GetSessionViewResult>("get_session_view", { session_id: target })).snapshot.value.runs.find(run => run.input_id === submitted.input_id);
      await expect.poll(async () => (await findRun())?.status, { timeout: 20_000 }).toBe(status);
      return (await findRun())!.run_id;
    }
    await run("F06_FIRST");
    const first = calls.filter(call => call.store);
    expect(first).toHaveLength(1);
    expect(first[0].previous_response_id).toBeUndefined();
    await run("F06_TOOLS");
    const text_and_tools = calls.filter(call => call.store);
    expect(text_and_tools).toHaveLength(3);
    expect(text_and_tools[1].previous_response_id).toBe("f06-response-1");
    expect(text_and_tools[1].input).toHaveLength(1);
    expect(text_and_tools[2].previous_response_id).toBe("f06-response-2");
    expect(text_and_tools[2].input).toHaveLength(1);
    expect(text_and_tools[2].input[0].type).toBe("function_call_output");
    expect(JSON.stringify(text_and_tools[2].input)).toContain("F06_READ_ONCE");
    await stop();
    await start();
    await run("F06_RESTARTED");
    const restored = calls.filter(call => call.store);
    expect(restored).toHaveLength(4);
    expect(restored[3].previous_response_id).toBe("f06-response-3");
    expect(restored[3].input).toHaveLength(1);
    expect(JSON.stringify(restored[3].input)).toContain("F06_RESTARTED");
    if (milestone === "m1") {
      const full = () => calls.filter(call => call.store).at(-1)!;
      await command("set_session_approval_mode", { session_id, approval_mode: "auto" });
      const other = { ...selection, model_id: "gpt-4.1-other" };
      await command("save_model_fixed_config", { selection: other, origin: "manual", parameters });
      // Default ABA has no intervening model call. Restart verifies durable invalidation.
      await command("set_default_model", { selection: other });
      await command("set_default_model", { selection });
      await stop(); await start();
      await run("F06_DEFAULT_ABA");
      expect(full().previous_response_id).toBeUndefined();
      expect(full().input.length).toBeGreaterThan(1);
      let previous = responses.get(full());
      await run("F06_DEFAULT_NEXT");
      expect(full().previous_response_id).toBe(previous);
      // Explicit selection ABA, including a persisted unchanged-selection save.
      for (const selected of [other, selection, selection]) await command("set_session_model", { session_id, model_selection: selected });
      await stop(); await start();
      await run("F06_EXPLICIT_ABA");
      expect(full().previous_response_id).toBeUndefined();
      // Reloaded global sampling ABA also survives restart without an intervening Run.
      const configPath = join(home, "users/_personal/config.toml");
      const baseConfig = await readFile(configPath, "utf8");
      await writeFile(configPath, `${baseConfig}\n[agent.defaults.generation]\ntemperature = 0.2\n`);
      await command("reload_config", {});
      await writeFile(configPath, baseConfig);
      await command("reload_config", {});
      await stop(); await start();
      await run("F06_GLOBAL_ABA");
      expect(full().previous_response_id).toBeUndefined();
      previous = responses.get(full());
      await command("reload_config", {});
      await run("F06_GLOBAL_SAME");
      expect(full().previous_response_id).toBe(previous);
      // Parameter ABA invalidates both persisted selection and the effective model binding.
      await command("save_model_fixed_config", { selection, origin: "manual", parameters: { ...parameters, max_output_tokens: { state: "known", value: 2048 } } });
      await command("save_model_fixed_config", { selection, origin: "manual", parameters });
      await stop(); await start();
      await run("F06_PARAMETERS_ABA");
      expect(full().previous_response_id).toBeUndefined();
      previous = responses.get(full());
      await command("save_model_fixed_config", { selection, origin: "manual", parameters });
      await run("F06_PARAMETERS_SAME");
      expect(full().previous_response_id).toBe(previous);
      // Reject after the read_file exchange: only the full model request is repeated.
      const beforeRecovery = calls.length;
      const readsBefore = readFileCalls;
      rejectToolResult = true;
      await run("F06_TOOLS_REBUILD");
      const recovery = calls.slice(beforeRecovery).filter(call => call.store);
      expect(recovery).toHaveLength(3);
      expect(rejected).toContain(recovery[1]);
      expect(recovery[1].input[0].type).toBe("function_call_output");
      expect(recovery[2].previous_response_id).toBeUndefined();
      expect(JSON.stringify(recovery[2].input)).toContain("F06_READ_ONCE");
      expect(readFileCalls).toBe(readsBefore + 1);
      previous = responses.get(full());
      await run("F06_REBUILT_NEXT");
      expect(full().previous_response_id).toBe(previous);
      const beforeOrdinary = calls.length;
      rejectNext = "ordinary";
      await run("F06_ORDINARY_ERROR", session_id, "failed");
      expect(calls.slice(beforeOrdinary).filter(call => call.store)).toHaveLength(1);
      // Children must never inherit the parent's or their sibling's ID. Each recovers once.
      const beforeChildren = calls.length;
      rejectChildResults = true;
      const parent_run_id = await run("F06_DELEGATE");
      rejectChildResults = false;
      const children = await command<{ tasks: { status: string }[] }>("list_child_tasks", { session_id, parent_run_id });
      expect(children.tasks).toHaveLength(2);
      expect(children.tasks.every(task => task.status === "completed")).toBe(true);
      const childCalls = calls.slice(beforeChildren).filter(call => call.store && JSON.stringify(call.input).includes("F06_CHILD_"));
      const childFirst = childCalls.filter(call => !call.input.some(item => item.type === "function_call_output"));
      expect(childFirst).toHaveLength(2);
      expect(childFirst.every(call => call.previous_response_id === undefined)).toBe(true);
      const firstIds = childFirst.map(call => responses.get(call));
      const childRejected = rejected.filter(call => firstIds.includes(call.previous_response_id));
      expect(childRejected).toHaveLength(2);
      expect(new Set(childRejected.map(call => call.previous_response_id)).size).toBe(2);
      // A Fork copies reliable local history but starts its own remote chain.
      const owner = { type: "main_session", session_id };
      const pageSnapshot = await command<{ snapshot: { value: { generation: number; items: { message_id: string }[] } } }>("list_conversation_page", { owner, cursor: null, turn_limit: 50 });
      const conversation = pageSnapshot.snapshot.value;
      const fork = await command<{ session: { session_id: string } }>("fork_session", { session_id, fork_point: conversation.items.at(-1)!.message_id, expected_generation: conversation.generation });
      await run("F06_FORK_FIRST", fork.session.session_id);
      expect(full().previous_response_id).toBeUndefined();
      previous = responses.get(full());
      await run("F06_FORK_NEXT", fork.session.session_id);
      expect(full().previous_response_id).toBe(previous);

      // F01 使用同一条正式压缩与重启链路，核对聊天中的缩略导航按新代次恢复。
      const navigation = async () => (await command<ListConversationNavigationResult>("list_conversation_navigation", { owner, cursor: null })).snapshot.value;
      const before_compaction = await navigation();
      await page.setViewportSize({ width: 2000, height: 800 });
      const selectSession = () => page.getByRole("complementary", { name: "会话导航" }).getByRole("button", { name: session_title, exact: true }).click();
      await login();
      await selectSession();
      const markers = page.getByRole("navigation", { name: "消息导航" }).locator("[data-navigation-message-id]");
      const markerIds = () => markers.evaluateAll(nodes => nodes.map(node => node.getAttribute("data-navigation-message-id")));
      const preview = page.getByRole("region", { name: "消息预览" });
      await expect(markers).toHaveCount(before_compaction.items.length);
      await markers.first().hover();
      await expect(preview.getByRole("group", { name: "用户输入" })).toHaveText("F06_FIRST");

      // Compaction is stateless; committed generation then rebuilds from summary and retained input.
      const compact = await command<{ outcome: { type: string; result_generation: number } }>("compact_session", { session_id, operation_id: crypto.randomUUID(), expected_generation: conversation.generation });
      expect(compact.outcome.type).toBe("compacted");
      expect(calls.some(call => !call.store && !call.previous_response_id)).toBe(true);
      const compacted = await navigation();
      expect(compacted.generation).toBeGreaterThan(before_compaction.generation);
      const stale_preview = await fetch(`${discovery.address}/commands`, {
        method: "POST",
        headers: headers(),
        body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: {
          type: "get_conversation_message_preview",
          payload: { owner, generation: before_compaction.generation, message_id: before_compaction.items[0].user_message_id },
        } } }),
      });
      expect(stale_preview.ok).toBe(false);
      expect(await stale_preview.text()).toContain("snapshot_stale");
      await expect(preview).toBeHidden();
      await expect.poll(markerIds).toEqual(compacted.items.map(item => item.user_message_id));
      await stop(); await start();
      expect(await navigation()).toEqual(compacted);
      await run("F06_COMPACTED");
      expect(full().previous_response_id).toBeUndefined();
      expect(JSON.stringify(full().input)).toContain("F06_OK");
      previous = responses.get(full());
      await run("F06_COMPACTED_NEXT");
      expect(full().previous_response_id).toBe(previous);
      await login();
      await selectSession();
      await markers.last().hover();
      await expect(preview.getByRole("group", { name: "用户输入" })).toHaveText("F06_COMPACTED_NEXT");
      await markers.last().click();
      await expect(page.getByLabel("消息列表", { exact: true }).getByText("F06_COMPACTED_NEXT", { exact: true })).toBeInViewport();
      const autoSelection = { ...selection, model_id: "auto-model" };
      await command("save_model_fixed_config", { selection: autoSelection, origin: "manual", parameters });
      const automatic = await command<{ session: { session_id: string } }>("create_session", { title: "F06 automatic", workspace_id: registered.workspace.workspace_id, model_selection: autoSelection });
      await run("F06_AUTO_FIRST", automatic.session.session_id);
      const beforeAutomatic = calls.length;
      await run("F06_AUTO_NEXT", automatic.session.session_id);
      const autoCalls = calls.slice(beforeAutomatic);
      expect(autoCalls.some(call => !call.store && !call.previous_response_id)).toBe(true);
      expect(full().previous_response_id).toBeUndefined();
      expect(JSON.stringify(full().input)).toContain("F06_AUTO_NEXT");
      const automaticPage = await command<{ snapshot: { value: { generation: number } } }>("list_conversation_page", { owner: { type: "main_session", session_id: automatic.session.session_id }, cursor: null, turn_limit: 50 });
      expect(automaticPage.snapshot.value.generation).toBe(2);
      await writeFile(join(home, "m1-wire.json"), JSON.stringify({ calls, rejected, readFileCalls }, null, 2));
    }
    const settings = await command<ProviderSummary[]>("list_providers", {});
    expect(settings).toEqual([saved]);
    await open();
    await dialog.getByRole("button", { name: /F06 fixture http/ }).click();
    await dialog.getByRole("button", { name: "编辑", exact: true }).click();
    await expect(toggle).toBeChecked();
    await expect(dialog.getByLabel("API Key", { exact: true })).toHaveValue("");
    await stop();
    expect(stderr).not.toContain("fixture-only-secret");
    if (process.env.EZ_ASSISTANT_KEEP_TEST_FIXTURES) console.log(`F06 verified Runtime Home: ${home}`);
  } finally {
    if (child?.exitCode === null) { child.kill("SIGTERM"); await new Promise<void>(done => child?.once("exit", () => done())); }
    await new Promise<void>(done => provider.close(() => done()));
    if (!process.env.EZ_ASSISTANT_KEEP_TEST_FIXTURES) await Promise.all([home, os_home, workspace].map(path => rm(path, { recursive: true, force: true })));
  }
});
