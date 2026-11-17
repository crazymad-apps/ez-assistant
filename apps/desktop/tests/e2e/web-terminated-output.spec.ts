import { spawn, type ChildProcess } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer, type ServerResponse } from "node:http";
import { join, resolve } from "node:path";
import { compatibilityHeaders, type RuntimeCommand, type RuntimeCommandResult } from "@ez-assistant/protocol";
import { expect, test } from "@playwright/test";

type Discovery = { address: string; access_token: string };

test("普通停止和流失败保留已显示输出，刷新和 Host 重启后可读", async ({ page }) => {
  test.setTimeout(120_000);
  const root = resolve(process.env.EZ_ASSISTANT_E2E_TEST_ROOT ?? "../../.runtime-test");
  await mkdir(root, { recursive: true });
  const fixture = await mkdtemp(join(root, "f03-terminated-"));
  const home = join(fixture, "runtime");
  const os_home = join(fixture, "os");
  const workspace_directory = join(fixture, "workspace");
  await Promise.all([mkdir(home), mkdir(os_home), mkdir(workspace_directory)]);

  // 每轮停在确定的流式边界，由普通停止或测试主动关闭流触发终态。
  const streams = new Map<string, ServerResponse>();
  const provider = createServer(async (request, response) => {
    if (request.method === "GET" && request.url === "/v1/models") {
      response.writeHead(200, { "Content-Type": "application/json" })
        .end(JSON.stringify({ object: "list", data: [{ id: "partial-fixture", object: "model" }] }));
      return;
    }
    let raw = "";
    for await (const chunk of request) raw += chunk;
    const scenario = raw.match(/F03_(STOP|FAIL)_(REASONING|MIXED|TEXT)/)?.[0];
    if (!scenario || request.url !== "/v1/chat/completions") {
      response.writeHead(400).end();
      return;
    }
    const send = (delta: object) => response.write(`data: ${JSON.stringify({
      id: scenario, model: "partial-fixture", choices: [{ index: 0, delta, finish_reason: null }],
    })}\n\n`);
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    send({ role: "assistant" });
    if (!scenario.endsWith("_TEXT")) send({ reasoning_content: "已经生成的思考" });
    if (!scenario.endsWith("_REASONING")) send({ content: "已经生成的正文" });
    if (scenario.endsWith("_MIXED")) send({ tool_calls: [{ index: 0, id: "unfinished-call", type: "function", function: { name: "read_file", arguments: "{" } }] });
    streams.set(scenario, response);
  });
  const reservation = createServer();
  await Promise.all([provider, reservation].map(server => new Promise<void>(done => server.listen(0, "127.0.0.1", done))));
  const provider_address = provider.address();
  const host_address = reservation.address();
  if (!provider_address || typeof provider_address === "string" || !host_address || typeof host_address === "string") throw new Error("Fixture ports unavailable");
  await new Promise<void>(done => reservation.close(() => done()));
  await writeFile(join(home, "config.toml"), `schema_version = 1\n[host_access]\nremote_enabled = false\nscheme = "http"\nport = ${host_address.port}\nserver_names = []\n`);

  let child: ChildProcess | undefined;
  let host: Discovery;
  let stderr = "";
  function headers(discovery = host) {
    return { ...compatibilityHeaders(), Authorization: `Bearer ${discovery.access_token}`, "Content-Type": "application/json" };
  }
  async function start() {
    const executable = process.env.EZ_ASSISTANT_E2E_HOST_BINARY ?? resolve(`../../target/debug/ez-assistant-runtime${process.platform === "win32" ? ".exe" : ""}`);
    child = spawn(executable, ["serve", "--runtime-home", home], {
      env: { ...process.env, HOME: os_home, ...(process.platform === "win32" ? { USERPROFILE: os_home } : {}) },
      stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", chunk => { stderr += String(chunk); });
    await expect.poll(async () => {
      if (child?.exitCode !== null) throw new Error(stderr);
      try {
        const discovery = JSON.parse(await readFile(join(home, "run/runtime.json"), "utf8")) as Discovery;
        const response = await fetch(`${discovery.address}/health`, { headers: headers(discovery) });
        if (!response.ok || (await response.json()).status !== "ready") return false;
        host = discovery;
        return true;
      } catch { return false; }
    }, { timeout: 15_000 }).toBe(true);
  }
  async function command<T extends RuntimeCommand["type"]>(type: T, payload: object = {}): Promise<Extract<RuntimeCommandResult, { type: T }>["payload"]> {
    const response = await fetch(`${host.address}/commands`, {
      method: "POST", headers: headers(),
      body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }),
    });
    const body = await response.json();
    expect(response.ok, JSON.stringify(body)).toBe(true);
    return body.result.payload.payload;
  }
  async function login() {
    const response = await fetch(`${host.address}/auth/login`, { method: "POST", headers: headers(), body: JSON.stringify({ method: "desktop" }) });
    expect(response.ok).toBe(true);
    const { token } = await response.json();
    await page.goto(`${host.address}/#token=${encodeURIComponent(token)}`);
  }
  async function select(title: string) {
    await page.getByRole("complementary", { name: "会话导航" }).getByRole("button", { name: title, exact: true }).click();
  }
  async function assistantItems(session_id: string) {
    return (await command("get_session_view", { session_id })).snapshot.value.conversation.items.filter(item => item.type === "assistant");
  }
  const saved = new Map<string, Awaited<ReturnType<typeof assistantItems>>>();
  try {
    await start();
    const model = await command("create_provider", {
      connection: { display_name: "F03 fixture", provider_type: "local", endpoint: `http://127.0.0.1:${provider_address.port}/v1`, protocol_preference: "chat_completions", models_path: "/v1/models", discovery_format: "openai" },
      credential: { mode: "replace", value: "fixture-only" },
    });
    await command("refresh_provider_models", { provider_instance_id: model.provider_instance_id });
    const selection = { provider_instance_id: model.provider_instance_id, model_id: "partial-fixture" };
    await command("save_model_fixed_config", { selection, parameters: {
      context_window_tokens: { state: "known", value: 16384 }, max_output_tokens: { state: "known", value: 4096 },
      max_input_tokens: { state: "unknown" }, reasoning_max_input_tokens: { state: "unknown" }, reasoning_max_output_tokens: { state: "unknown" },
      streaming: "supported", image_input: "unsupported", tool_calls: "supported", reasoning: "unsupported",
      tool_choice: { auto: "supported", none: "supported", required: "supported", named: "supported" },
      tool_image_projection: "unsupported", reasoning_mode: "unsupported", reasoning_efforts: {}, default_reasoning_effort: null,
    } });
    const { workspace } = await command("register_workspace", { label: "F03 Workspace", primary_directory: workspace_directory, additional_directories: [] });
    await page.emulateMedia({ reducedMotion: "reduce" });
    await login();
    for (const action of ["STOP", "FAIL"]) {
      for (const output of ["REASONING", "MIXED", "TEXT"]) {
        const title = `F03_${action}_${output}`;
        const { session } = await command("create_session", { title, model_selection: selection, workspace_id: workspace.workspace_id });
        const session_id = session.session_id;
        await select(title);
        const { input_id } = await command("submit_input", { session_id, message: title, mode: "normal", variant: "build", attachment_ids: [], quotes: [] });
        if (output !== "TEXT") await expect(page.getByText("已经生成的思考", { exact: true })).toBeVisible();
        if (output !== "REASONING") await expect(page.getByText("已经生成的正文", { exact: true })).toBeVisible();
        if (action === "STOP") {
          await page.getByRole("button", { name: "中断当前轮次", exact: true }).click();
        } else {
          streams.get(title)!.end();
        }
        const status = action === "STOP" ? "cancelled" : "failed";
        await expect.poll(async () => (await command("get_session_view", { session_id })).snapshot.value.runs.find(run => run.input_id === input_id)?.status).toBe(status);
        const items = await assistantItems(session_id);
        expect(items).toHaveLength(1);
        expect(items[0]).toMatchObject({ status, step: 1, segments: [
          ...(output === "TEXT" ? [] : [{ type: "reasoning", text: "已经生成的思考" }]),
          ...(output === "REASONING" ? [] : [{ type: "text", text: "已经生成的正文" }]),
        ] });
        saved.set(session_id, items);
        await page.reload();
        await select(title);
        if (output !== "REASONING") await expect(page.getByText("已经生成的正文", { exact: true })).toHaveCount(1);
        if (output !== "TEXT") {
          await page.getByRole("button", { name: "处理过程", exact: true }).click();
          await expect(page.getByText("已经生成的思考", { exact: true })).toBeVisible();
        }
      }
    }
    await command("shutdown_runtime");
    await expect.poll(() => child?.exitCode).toBe(0);
    await start();
    for (const [session_id, items] of saved) expect(await assistantItems(session_id)).toEqual(items);
    await page.goto("about:blank");
    await login();
    await select("F03_FAIL_MIXED");
    await expect(page.getByText("已经生成的正文", { exact: true })).toBeVisible();
  } finally {
    if (child && child.exitCode === null && child.signalCode === null) {
      child.kill("SIGTERM");
      await new Promise<void>(done => child!.once("exit", () => done()));
    }
    provider.closeAllConnections();
    await new Promise<void>(done => provider.close(() => done()));
    await rm(fixture, { recursive: true, force: true });
  }
});
