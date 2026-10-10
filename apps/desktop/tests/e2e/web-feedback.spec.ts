import { test, expect } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { compatibilityHeaders } from "@ez-assistant/protocol";
const root = process.env.F04_SCENARIO;
test.skip(!root, "需要新建 F04 隔离环境");
const center = "http://127.0.0.1:17325", host = "http://127.0.0.1:17425";
let admin = "", username = "", owner = 0;
async function api(path: string, method = "GET", body?: object) {
  const response = await fetch(`${center}/api/${path}`, { method, headers: { "Content-Type": "application/json", Authorization: `Bearer ${admin}` }, ...(body ? { body: JSON.stringify(body) } : {}) });
  const result = await response.json(); expect(response.ok, JSON.stringify(result)).toBe(true); return result;
}
test.beforeAll(async () => {
  if (!root || !resolve(root).startsWith(resolve("../../.runtime-test/f04-m12-"))) throw new Error("必须使用 F04 新建隔离场景");
  const scene = JSON.parse(await readFile(resolve(root, "scenario.json"), "utf8"));
  expect(scene).toMatchObject({ root: resolve(root), database: "ez_center_f04_m12", postgres_host: "127.0.0.1", postgres_port: 55485, center_port: 17325, host_port: 17425 });
  await expect.poll(async () => { try { return (await fetch(host)).ok; } catch { return false; } }, { timeout: 30_000 }).toBe(true);
  admin = (await api("auth/login", "POST", { username: "admin", password: "123456" })).token;
  username = `feedback-${Date.now()}`;
  owner = (await api("users", "POST", { username, display_name: username, role: "user", enabled: true, password: "test1234" })).id;
});
test("正式 Host 三类目标、处理回读、未知回执恢复与布局", async ({ page }) => {
  await page.goto(host);
  await page.getByLabel("企业账号", { exact: true }).fill(username); await page.getByLabel("账号密码", { exact: true }).fill("test1234");
  await page.getByRole("button", { name: "进入项目" }).click();
  await page.getByRole("button", { name: "账号与 Runtime", exact: true }).click();
  await page.getByRole("menuitem", { name: "反馈中心", exact: true }).click();
  const panel = page.getByRole("dialog", { name: "反馈中心", exact: true });
  await expect(panel.getByRole("button", { name: "新增反馈", exact: true })).toBeEnabled();
  let first = "";
  for (const kind of ["平台", "插件", "知识库"]) {
    await panel.getByRole("button", { name: "新增反馈", exact: true }).click();
    await panel.getByRole("button", { name: "反馈目标", exact: true }).click(); await page.getByRole("option", { name: kind, exact: true }).click();
    if (kind !== "平台") await panel.getByLabel(kind === "插件" ? "插件名称 *" : "知识库名称 *").fill(`测试${kind}`);
    if (kind === "插件") { await panel.getByLabel("插件标识 *").fill("feedback-test-plugin"); await panel.getByLabel("实际使用版本（可选）").fill("1.2.3"); }
    if (kind === "知识库") { await panel.getByLabel("针对具体内容").check(); await panel.getByLabel("内容位置或章节 *").fill("安全指南 / 第三章"); }
    await panel.getByLabel("标题 *", { exact: true }).fill(`${kind}闭环反馈`);
    await panel.getByLabel("问题描述 *").fill("测试正文 <script>不会执行</script>\n保留换行");
    await panel.getByRole("button", { name: "提交反馈", exact: true }).click();
    await expect(panel).toContainText("反馈已提交");
    await expect(panel.getByRole("heading", { name: `${kind}闭环反馈`, exact: true })).toBeVisible();
    const list = await api(`admin/feedback?owner_user_id=${owner}`);
    const record = list.items.find((item: {title: string}) => item.title === `${kind}闭环反馈`); expect(record).toBeTruthy();
    const detail = await api(`admin/feedback/${record.id}`); expect(detail.environment.client_kind).toBe("web"); expect(detail.environment.host_os).toBe("macos");
    if (kind === "平台") {
      first = detail.id;
      await api(`admin/feedback/${detail.id}`, "PATCH", { expected_status: "pending", expected_handling_note: null, status: "resolved", handling_note: "已定位并修复" });
      await panel.getByRole("button", { name: "刷新处理结果" }).click(); await expect(panel).toContainText("已定位并修复");
    }
    await panel.getByRole("button", { name: "返回", exact: true }).click();
  }
  expect((await api(`admin/feedback?owner_user_id=${owner}`)).total).toBe(3);
  let dropped = false;
  await page.route("**/commands", async (route) => {
    const body = route.request().postDataJSON();
    if (!dropped && body.command?.payload?.type === "submit_feedback") { dropped = true; await route.fetch(); await route.abort(); } else await route.continue();
  });
  await panel.getByRole("button", { name: "新增反馈", exact: true }).click();
  await panel.getByRole("button", { name: "反馈目标", exact: true }).click(); await page.getByRole("option", { name: "平台", exact: true }).click();
  await panel.getByLabel("标题 *", { exact: true }).fill("回执丢失"); await panel.getByLabel("问题描述 *").fill("此记录只应保存一次");
  await panel.getByRole("button", { name: "提交反馈", exact: true }).click(); await expect(panel).toContainText("尚未确认提交结果");
  await panel.getByRole("button", { name: "检查提交结果" }).click(); await expect(panel).toContainText("已找到本次提交");
  expect((await api(`admin/feedback?owner_user_id=${owner}`)).total).toBe(4);
  await panel.getByRole("button", { name: "返回", exact: true }).click();
  for (const scale of [1, 1.25, 1.5]) {
    await page.setViewportSize({ width: 820, height: 640 }); await page.evaluate((scale) => { document.documentElement.style.zoom = String(scale); }, scale);
    await expect(panel.getByRole("button", { name: "新增反馈", exact: true })).toBeInViewport();
    const bounds = await panel.evaluate((element) => { const rect = element.getBoundingClientRect(); return { left: rect.left, right: rect.right, viewport: window.innerWidth, overflow: element.scrollWidth > element.clientWidth }; });
    expect(await panel.locator("li button").evaluateAll((buttons) => buttons.every((button) => button.getBoundingClientRect().bottom >= button.lastElementChild!.getBoundingClientRect().bottom))).toBe(true);
    await page.screenshot({ path: resolve(root!, `feedback-scale-${scale}.png`) });
    expect(bounds.left).toBeGreaterThanOrEqual(0); expect(bounds.right).toBeLessThanOrEqual(bounds.viewport); expect(bounds.overflow).toBe(false);
  }
  await page.evaluate(() => { document.documentElement.style.zoom = "1"; });
  await page.screenshot({ path: resolve(root!, "feedback-web.png") });
  await page.keyboard.press("Escape"); await expect(panel).not.toBeVisible();
  const other = `feedback-other-${Date.now()}`;
  await api("users", "POST", { username: other, display_name: other, role: "user", enabled: true, password: "test1234" });
  const login = await page.request.post(`${host}/auth/login`, { headers: { ...compatibilityHeaders(), Origin: host }, data: { method: "enterprise", native: true, username: other, password: "test1234" } });
  expect(login.ok()).toBe(true);
  const other_identity = await login.json();
  const response = await fetch(`${host}/commands`, { method: "POST", headers: { ...compatibilityHeaders(), "Content-Type": "application/json", Authorization: `Bearer ${other_identity.token}` }, body: JSON.stringify({ request_id: "check-owner", command: { scope: "runtime", payload: { type: "get_submitted_feedback", payload: { id: first } } } }) });
  const result = await response.json(); expect(response.ok, JSON.stringify(result)).toBe(true);
  expect(result.result.payload.payload).toEqual({ status: "failure", data: "not_found" });
});

test("Agent 工具在实际 Runtime 权限链、Plan/Build 和子任务中提交", async () => {
  test.setTimeout(120_000);
  const { createServer } = await import("node:http");
  const seen: { marker: string; tools: string[] }[] = [];
  const inputs = new Map<string, object>();
  const upstream = createServer(async (request, response) => {
    if (request.method === "GET") { response.setHeader("Content-Type", "application/json"); response.end(JSON.stringify({ object: "list", data: [{ id: "f04-fixture", object: "model" }] })); return; }
    let raw = ""; for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw);
    const marker = raw.match(/F04_(?:CHILD_WORK|CHILD|BUILD_ASK|PLAN_ASK|BUILD_AUTO|PLAN_AUTO|DENY|EXPLICIT_ASK)/)?.[0] ?? "UNKNOWN";
    const names = (body.tools ?? []).map((tool: {function: {name: string}}) => tool.function.name);
    seen.push({ marker, tools: names });
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    const chunk = (delta: object, reason: string | null = null) => response.write(`data: ${JSON.stringify({ id: `fixture-${seen.length}`, model: "f04-fixture", choices: [{ index: 0, delta, finish_reason: reason }] })}\n\n`);
    if (marker === "UNKNOWN" || body.messages.at(-1)?.role === "tool") { chunk({ content: "F04 工具验证已结束。" }, "stop"); }
    else {
      const child = marker === "F04_CHILD" && names.includes("delegate_task");
      const args = child ? { title: "子任务反馈", task: "F04_CHILD_WORK 请提交已确认的知识内容问题", context: null, expected_output: null } : {
        target: { kind: marker === "F04_CHILD_WORK" ? "knowledge" : "platform", name: marker === "F04_CHILD_WORK" ? "子任务知识库" : "EZ Assistant", reference: null, version: null, content_ref: marker === "F04_CHILD_WORK" ? "doc-123" : null, location: null },
        title: marker, description: "受控模型发现并提交的问题", reproduction: null, expected_result: null,
      };
      inputs.set(marker, args);
      chunk({ role: "assistant", tool_calls: [{ index: 0, id: `call-${seen.length}`, type: "function", function: { name: child ? "delegate_task" : "submit_feedback", arguments: JSON.stringify(args) } }] }); chunk({}, "tool_calls");
    }
    response.end("data: [DONE]\n\n");
  });
  await new Promise<void>((done) => upstream.listen(0, "127.0.0.1", done));
  const address = upstream.address(); if (!address || typeof address === "string") throw new Error("missing fixture port");
  try {
    const provider = await api("model-providers", "POST", { connection: { display_name: "F04 隔离模型", provider_type: "local", endpoint: `http://127.0.0.1:${address.port}/v1`, protocol_preference: "chat_completions", models_path: "/v1/models", discovery_format: "openai", server_history_enabled: false, server_history_support_override: null }, credential: { mode: "replace", value: "fixture-only" } });
    const selection = { provider_instance_id: provider.provider_instance_id, model_id: "f04-fixture" };
    await api(`model-providers/${provider.provider_instance_id}/refresh`, "POST", {});
    await api("model-configuration", "PUT", { ...selection, origin: "online", parameters: {
      context_window_tokens: { state: "known", value: 32768 }, max_input_tokens: { state: "unknown" }, max_output_tokens: { state: "known", value: 2048 }, reasoning_max_input_tokens: { state: "unknown" }, reasoning_max_output_tokens: { state: "unknown" },
      streaming: "supported", tool_calls: "supported", image_input: "unsupported", reasoning: "unsupported", reasoning_mode: "unsupported", reasoning_efforts: null, default_reasoning_effort: null, tool_image_projection: "unsupported",
      tool_choice: { auto: "supported", none: "supported", named: "supported", required: "supported" },
    } });
    await api("model-settings", "PUT", { default_model: selection });
    const login = await fetch(`${host}/auth/login`, { method: "POST", headers: { ...compatibilityHeaders(), "Content-Type": "application/json", Origin: host }, body: JSON.stringify({ method: "enterprise", username, password: "test1234", native: true }) });
    const identity = await login.json(); expect(login.ok, JSON.stringify(identity)).toBe(true);
    const headers = { ...compatibilityHeaders(), "Content-Type": "application/json", Authorization: `Bearer ${identity.token}` };
    async function command(type: string, payload: object = {}) {
      const response = await fetch(`${host}/commands`, { method: "POST", headers, body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }) });
      const result = await response.json(); expect(response.ok, JSON.stringify(result)).toBe(true); return result.result.payload.payload;
    }
    await command("refresh_model_source");
    const before = (await api(`admin/feedback?owner_user_id=${owner}&source=agent`)).total;
    for (const [marker, variant, approval_mode, rule] of [
      ["BUILD_ASK", "build", "ask", ""], ["PLAN_ASK", "plan", "ask", ""], ["BUILD_AUTO", "build", "auto", ""], ["PLAN_AUTO", "plan", "auto", ""],
      ["DENY", "build", "auto", "deny"], ["EXPLICIT_ASK", "build", "auto", "ask"], ["CHILD", "build", "auto", ""],
    ]) {
      const session_id = (await command("create_session", { title: `F04 ${marker}`, model_selection: null })).session.session_id;
      await command("set_session_approval_mode", { session_id, approval_mode });
      if (rule) {
        const scope = { type: "session", payload: { session_id } };
        const document = (await command("get_permission_document", { scope })).document;
        await command("replace_permission_document", { scope, expected_revision: document.revision, document: { schema_version: 1, rules: [{ id: "explicit-feedback", effect: rule, variants: ["plan", "build"], matcher: { type: "general", payload: { tool_name: "submit_feedback" } } }] } });
      }
      const initial = (await api(`admin/feedback?owner_user_id=${owner}&source=agent`)).total;
      const { run } = await command("submit_input", { session_id, variant, message: `F04_${marker} ${marker === "PLAN_ASK" ? "继续分析，发现明确问题时自主反馈" : "请提交此问题的反馈"}` });
      if (rule === "ask") {
        await expect.poll(async () => (await command("list_pending_approvals", { session_id })).approvals.length).toBe(1);
        expect((await api(`admin/feedback?owner_user_id=${owner}&source=agent`)).total).toBe(initial);
        const approval = (await command("list_pending_approvals", { session_id })).approvals[0];
        await command("decide_approval", { session_id, approval_id: approval.approval_id, decision: "allow_once" });
      }
      await expect.poll(async () => (await command("get_run", { session_id, run_id: run.run_id })).run.status, { timeout: 20_000 }).toBe("completed");
      expect((await command("list_pending_approvals", { session_id })).approvals).toHaveLength(0);
      expect((await api(`admin/feedback?owner_user_id=${owner}&source=agent`)).total).toBe(initial + (rule === "deny" ? 0 : 1));
    }
    const records = await api(`admin/feedback?owner_user_id=${owner}&source=agent`); expect(records.total).toBe(before + 6);
    expect(records.items.some((item: {title: string}) => item.title === "F04_CHILD_WORK")).toBe(true);
    const main_tools = seen.find((item) => item.marker === "F04_BUILD_ASK")!.tools;
    expect(main_tools).toContain("submit_feedback");
    expect(seen.find((item) => item.marker === "F04_PLAN_ASK")!.tools).toEqual(main_tools);
    expect(seen.find((item) => item.marker === "F04_CHILD_WORK")!.tools).toContain("submit_feedback");
    expect(seen.find((item) => item.marker === "F04_CHILD_WORK")!.tools).not.toContain("delegate_task");
  } finally { upstream.closeAllConnections(); await new Promise<void>((done) => upstream.close(() => done())); }
});
