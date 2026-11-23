import { compatibilityHeaders } from "@ez-assistant/protocol";
import { spawn, type ChildProcess } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import { join, resolve } from "node:path";
import type { FullConfig } from "@playwright/test";

type Discovery = {
  readonly address: string;
  readonly instance_id: string;
  readonly access_token: string;
};

const STARTUP_TIMEOUT_MS = 30_000;

export default async function setupRuntimeHost(_config: FullConfig): Promise<() => Promise<void>> {
  const retain_c03 = process.env.C03_E2E_RETAIN_HOME === "1";
  const retain_home = retain_c03 || process.env.EZ_ASSISTANT_E2E_RETAIN_HOME === "1";
  const retained_label = retain_c03 ? "retained C03 E2E Runtime Home" : "retained E2E Runtime Home";
  const test_root = resolve(process.env.EZ_ASSISTANT_E2E_TEST_ROOT ?? "../../.runtime-test/c02-m4");
  await mkdir(test_root, { recursive: true });
  const runtime_home = await mkdtemp(join(test_root, "ez-assistant-e2e-runtime-"));
  const workspace = await mkdtemp(join(test_root, "ez-assistant-e2e-workspace-"));
  const additional_workspace = await mkdtemp(join(test_root, "ez-assistant-e2e-added-workspace-"));
  const provider = await startFakeProvider();
  const mcp_secret = "e2e-mcp-secret-must-not-leak-9273";
  const reservation = createServer();
  await new Promise<void>((resolve) => reservation.listen(0, "127.0.0.1", resolve));
  const reserved_address = reservation.address();
  if (!reserved_address || typeof reserved_address === "string") throw new Error("test port missing");
  const host_port = reserved_address.port;
  await new Promise<void>((resolve, reject) => reservation.close((error) => error ? reject(error) : resolve()));
  const user_directory = await mkdtemp(join(test_root, "ez-assistant-e2e-os-home-"));
  await writeFile(
    join(runtime_home, "config.toml"),
    `schema_version = 1

[host_access]
remote_enabled = false
scheme = "http"
port = ${host_port}
server_names = []

`,
  );

  const executable = process.env.EZ_ASSISTANT_E2E_HOST_BINARY ?? resolve(process.cwd(), `../../target/debug/ez-assistant-runtime${process.platform === "win32" ? ".exe" : ""}`);
  const ui_fixture = process.env.EZ_ASSISTANT_E2E_MCP_APPS === "1" ? {
    ui_fixture: { command: process.platform === "win32" ? "python" : "python3", args: ["-u", resolve(process.cwd(), "../runtime-host/tests/fixtures/mcp_ui_server.py")] },
  } : {};
  // 只在隔离 Runtime Home 中启用安全 stdio fixture，不读取或变更用户 MCP 配置。
  await writeFile(join(runtime_home, "mcp.json"), JSON.stringify({ mcpServers: {
    ...ui_fixture,
    local_fixture: {
      command: process.platform === "win32" ? "python" : "python3", args: ["-u", resolve(process.cwd(), "../runtime-host/tests/fixtures/mcp_stdio_server.py")],
      displayName: "MCP fixture", description: "Two safe fixture tools",
      env: { TOKEN: mcp_secret },
    },
  } }));
  const child = spawn(executable, ["serve", "--runtime-home", runtime_home], {
    // Home 仅覆盖测试子进程，避免正式技能发现读取开发者个人目录。
    env: { ...process.env, HOME: user_directory, ...(process.platform === "win32" ? { USERPROFILE: user_directory } : {}) },
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  child.stderr?.on("data", (chunk: Buffer) => {
    stderr += chunk.toString("utf8");
  });

  try {
    const discovery = await waitForDiscovery(runtime_home, child, () => stderr);
    const capabilities = await getJson(`${discovery.address}/capabilities`, discovery.access_token);
    // 通过正式新 API 构造测试连接和固定参数，不从旧 TOML 转换。
    const provider_summary = await runtimeCommand(discovery, "create_provider", {
      connection: { display_name: "离线测试服务商", provider_type: "local", endpoint: `${provider.endpoint}/v1`, protocol_preference: "chat_completions", models_path: "/v1/models", discovery_format: "openai" },
      credential: { mode: "replace", value: "e2e-placeholder-not-a-real-secret" },
    }) as { provider_instance_id: string };
    await runtimeCommand(discovery, "refresh_provider_models", {
      provider_instance_id: provider_summary.provider_instance_id,
    });
    for (const model_id of ["offline-model", "alternate-offline-model"]) {
      await runtimeCommand(discovery, "save_model_fixed_config", {
        selection: { provider_instance_id: provider_summary.provider_instance_id, model_id },
        parameters: {
          context_window_tokens: { state: "known", value: 16384 }, max_output_tokens: { state: "known", value: 4096 },
          max_input_tokens: { state: "unknown" }, reasoning_max_input_tokens: { state: "unknown" }, reasoning_max_output_tokens: { state: "unknown" },
          streaming: "supported", image_input: "unsupported", tool_calls: "supported", reasoning: "unsupported",
          tool_choice: { auto: "supported", none: "supported", required: "supported", named: "supported" },
          tool_image_projection: "unsupported", reasoning_mode: "unsupported", reasoning_efforts: {}, default_reasoning_effort: null,
        },
      });
    }
    await runtimeCommand(discovery, "set_default_model", { selection: { provider_instance_id: provider_summary.provider_instance_id, model_id: "offline-model" } });
    const registered = await runtimeCommand(
      discovery,
      "register_workspace",
      {
        label: "E2E Workspace",
        primary_directory: workspace,
        additional_directories: [],
      },
    ) as { workspace: { workspace_id: string } };
    const created = await runtimeCommand(discovery, "create_session", {
      title: "M2 临时会话",
      model_selection: null,
      workspace_id: registered.workspace.workspace_id,
    }) as { session: { session_id: string } };
    await uploadAttachment(discovery, created.session.session_id);
    process.env.EZ_ASSISTANT_E2E_BOOTSTRAP = JSON.stringify({
      base_url: discovery.address,
      instance_id: discovery.instance_id,
      access_token: discovery.access_token,
      capabilities,
      started_runtime: true,
    });
    process.env.EZ_ASSISTANT_E2E_NEW_WORKSPACE = additional_workspace;
    process.env.EZ_ASSISTANT_E2E_RUNTIME_HOME = runtime_home;
    process.env.EZ_ASSISTANT_E2E_MCP_SECRET = mcp_secret;
    process.env.EZ_ASSISTANT_E2E_SEARCH_BASE_URL = provider.endpoint;
    process.env.EZ_ASSISTANT_E2E_SKILL_ROOT = join(user_directory, ".agents", "skills");
  } catch (error) {
    child.kill("SIGTERM");
    await provider.close();
    if (retain_home) {
      // C03 实包联调保留新建的隔离库，供停机后的精确核验；默认 E2E 行为不变。
      process.stdout.write(`${retained_label}: ${runtime_home}\n`);
    } else {
      await removeTemporaryDirectories(runtime_home, workspace, additional_workspace, user_directory);
    }
    throw error;
  }

  return async () => {
    const bootstrap = process.env.EZ_ASSISTANT_E2E_BOOTSTRAP;
    if (bootstrap) {
      const parsed = JSON.parse(bootstrap) as {
        readonly base_url: string;
        readonly access_token: string;
      };
      await runtimeCommand(
        { address: parsed.base_url, access_token: parsed.access_token, instance_id: "" },
        "shutdown_runtime",
        {},
      ).catch(() => undefined);
    }
    await waitForExit(child);
    await provider.close();
    if (retain_home) {
      process.stdout.write(`${retained_label}: ${runtime_home}\n`);
    } else {
      await removeTemporaryDirectories(runtime_home, workspace, additional_workspace, user_directory);
    }
    delete process.env.EZ_ASSISTANT_E2E_BOOTSTRAP;
    delete process.env.EZ_ASSISTANT_E2E_NEW_WORKSPACE;
    delete process.env.EZ_ASSISTANT_E2E_RUNTIME_HOME;
    delete process.env.EZ_ASSISTANT_E2E_MCP_SECRET;
    delete process.env.EZ_ASSISTANT_E2E_SEARCH_BASE_URL;
    if (stderr.includes(mcp_secret)) throw new Error("MCP credential leaked to Host stderr");
  };
}

type FakeProvider = Readonly<{
  endpoint: string;
  close: () => Promise<void>;
}>;

async function startFakeProvider(): Promise<FakeProvider> {
  let response_sequence = 0;
  const server = createServer(async (request, response) => {
    if (request.method === "GET" && request.url === "/v1/models") {
      response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ object: "list", data: [
        { id: "offline-model", object: "model", owned_by: "fixture" }, { id: "alternate-offline-model", object: "model", owned_by: "fixture" },
      ] }));
      return;
    }
    if (request.method !== "POST" || request.url !== "/v1/chat/completions") {
      response.writeHead(404).end();
      return;
    }
    const chunks: Buffer[] = [];
    for await (const chunk of request) {
      chunks.push(Buffer.from(chunk));
    }
    const body = JSON.parse(Buffer.concat(chunks).toString("utf8")) as ProviderRequest;
    const marker = latestMarker(JSON.stringify(body));
    const has_tool_result = currentTurnHasToolResult(body);
    response_sequence += 1;
    const response_id = `e2e-response-${response_sequence}`;
    if (marker === "BLOCK_FOR_QUEUE") {
      await delay(5_000);
    }
    response.writeHead(200, {
      "content-type": "text/event-stream",
      "cache-control": "no-cache",
    });
    let frames: readonly object[];
    const feedback_index = body.messages?.findLastIndex((message) => message.role === "user" && JSON.stringify(message.content).includes("F04_")) ?? -1;
    const feedback_completed = body.messages?.slice(feedback_index + 1).some((message) => message.role === "tool");
    const resource_text = body.messages?.filter((message) => message.role === "user").map((message) => {
      if (typeof message.content === "string") return message.content;
      if (!Array.isArray(message.content)) return "";
      return message.content.map((part: { text?: string }) => part.text ?? "").join("\n");
    }).findLast((text) => text.startsWith("F05_RESOURCES\n"));
    const schedule_summary = body.messages?.some((message) => message.role === "system"
      && JSON.stringify(message.content).includes("The current input is the result of a scheduled task."));
    if (schedule_summary) {
      const source = JSON.stringify(body.messages?.findLast((message) => message.role === "user"));
      frames = textFrames(response_id, source.includes("Agent 归档汇报") && !body.tools?.length
        ? "助理已整理定时任务结果，两份文件可以直接下载。"
        : "定时汇报夹具缺少来源或携带工具。");
    } else if (marker === "GOAL_QUEUE_START" || marker === "GOAL_QUEUE_GUIDANCE") {
      const history = JSON.stringify(body.messages);
      const guidance = history.includes("GOAL_QUEUE_GUIDANCE");
      const has_resume_context = history.includes("GOAL_RESUME_INJECTION_V1");
      if (guidance && !has_resume_context) {
        frames = textFrames(response_id, "Goal resume context missing");
      } else if (has_tool_result) {
        frames = textFrames(response_id, guidance ? "Goal guidance applied" : "Goal paused for guidance");
      } else {
        const args = guidance
          ? { status: "complete", summary: "Guidance applied" }
          : { status: "blocked", summary: "Waiting for fixture guidance" };
        frames = [
          { id: response_id, model: "offline-model", choices: [{ index: 0, delta: { role: "assistant", tool_calls: [{ index: 0, id: `goal-${response_id}`, type: "function", function: { name: "update_goal", arguments: JSON.stringify(args) } }] }, finish_reason: null }] },
          { id: response_id, model: "offline-model", choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] },
        ];
      }
    } else if ((marker === 'F03_CARD' || marker === 'F03_MCP_CARD' || marker === 'F03_APPS_CARD') && !has_tool_result) {
      const name=marker!=='F03_MCP_CARD'?'publish_card':'call_mcp_tool';
      const args=marker==='F03_APPS_CARD'?{plugin:'ui-link-fixture',entry:'counter',arguments:{count:JSON.stringify(body.messages?.findLast(message=>message.role==='user')?.content).includes('TWO')?2:1,app_plugin:'ui-link-fixture'}}:marker==='F03_CARD'?{plugin:'ui-fixture',entry:'counter',arguments:{count:1}}:{server:'ui_fixture',tool:'show_card',arguments:{value:'card input'}};
      const inputs = marker === 'F03_CARD'
        ? [args, { plugin: 'ui-fixture', entry: 'counter', arguments: { count: 7 } }]
        : [args];
      const tool_calls = inputs.map((input, index) => ({
        index, id: `card-${response_id}-${index}`, type: 'function', function: { name, arguments: JSON.stringify(input) },
      }));
      frames=[{id:response_id,model:'offline-model',choices:[{index:0,delta:{role:'assistant',tool_calls},finish_reason:null}]},
        {id:response_id,model:'offline-model',choices:[{index:0,delta:{},finish_reason:'tool_calls'}]}];
    } else if (resource_text) {
      frames = textFrames(response_id, resource_text.slice("F05_RESOURCES\n".length));
    } else if (feedback_index >= 0 && !feedback_completed) {
      const mode = JSON.stringify(body.messages?.[feedback_index]).includes("F04_END") ? "end_turn" : "wait";
      const arguments_text = JSON.stringify({ mode, questions: [
        { question: "最终使用什么格式？", header: "格式", options: [{ label: "PDF", description: "便于阅读" }, { label: "文档" }] },
        { question: "交付包含哪些部分？", header: "内容", multiple: true, options: [{ label: "说明" }, { label: "清单" }] },
        { question: "还有什么补充？", header: "补充" },
      ] });
      frames = [
        { id: response_id, model: "offline-model", choices: [{ index: 0, delta: { role: "assistant", tool_calls: [{ index: 0, id: `feedback-${response_id}`, type: "function", function: { name: "ask_user", arguments: arguments_text } }] }, finish_reason: null }] },
        { id: response_id, model: "offline-model", choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] },
      ];
    } else if (marker === "F02_STREAM") {
      frames = [
        ...Array.from({ length: 120 }, (_, index) => ({ id: response_id, model: "offline-model", choices: [{ index: 0, delta: { content: `F02 streaming ${index}\n` }, finish_reason: null }] })),
        ...textFrames(response_id, "F02 stream completed"),
      ];
    } else if (marker === "C05_STREAM") {
      frames = [
        ...Array.from({ length: 180 }, (_, i) => ({ id: response_id, model: "offline-model", choices: [{ index: 0, delta: { reasoning_content: `推理 ${i}\n`.repeat(6) }, finish_reason: null }] })),
        ...Array.from({ length: 180 }, (_, i) => ({ id: response_id, model: "offline-model", choices: [{ index: 0, delta: { content: `正文 ${i}\n\n`.repeat(3) }, finish_reason: null }] })),
        ...textFrames(response_id, "C05 完成"),
      ];
    } else if (marker === "F02_CHAT_SCHEDULE" || marker === "F02_AGENT_JOB") {
      frames = scheduleFrames(response_id, body, marker, has_tool_result);
    } else if (marker === "MCP_CASE") {
      frames = has_tool_result
        ? textFrames(response_id, JSON.stringify(body.messages?.filter((message) => message.role === "tool").at(-1)).includes("called:first_tool") ? "MCP 调用已回传。" : "MCP 调用已拒绝。")
        : mcpToolCallFrames(response_id);
    } else if (marker === "TOOL_CASE" && !has_tool_result) {
      frames = toolCallFrames(response_id);
    } else if (marker === "F01_BACKGROUND_JOBS" && !has_tool_result) {
      frames = backgroundShellFrames(response_id);
    } else if (marker === "F01_CONTEXT_PAGED" && !has_tool_result) {
      frames = contextPageFrames(response_id, body);
    } else if (marker === "DELEGATE_CASE" && !has_tool_result) {
      frames = delegateTaskFrames(response_id);
    } else {
      frames = textFrames(response_id, marker === "TOOL_CASE" ? "工具执行完成。" : `离线回复：${marker}`);
    }
    for (const frame of frames) {
      response.write(`data: ${JSON.stringify(frame)}\n\n`);
      await delay(35);
    }
    response.end("data: [DONE]\n\n");
  });
  await new Promise<void>((resolve_listen, reject_listen) => {
    server.once("error", reject_listen);
    server.listen(0, "127.0.0.1", () => resolve_listen());
  });
  const address = server.address();
  if (!address || typeof address === "string") {
    throw new Error("fake Provider did not publish a TCP address");
  }
  return {
    endpoint: `http://127.0.0.1:${address.port}`,
    close: () => closeServer(server),
  };
}

async function uploadAttachment(discovery: Discovery, session_id: string): Promise<void> {
  const body = new FormData();
  body.append("file", new Blob(["formal host attachment projection"], { type: "text/plain" }), "e2e-attachment.txt");
  const response = await fetch(`${discovery.address}/sessions/${session_id}/attachments`, {
    method: "POST",
    headers: { ...compatibilityHeaders(), Authorization: `Bearer ${discovery.access_token}` },
    body,
  });
  if (!response.ok) {
    throw new Error(`temporary Runtime attachment upload failed: ${response.status} ${await response.text()}`);
  }
}

type ProviderMessage = Readonly<{ role?: string; content?: unknown }>;
type ProviderRequest = Readonly<{ messages?: readonly ProviderMessage[]; tools?: readonly unknown[] }>;

function latestMarker(body: string): string {
  const markers = ["FIRST_CASE", "BLOCK_FOR_QUEUE", "QUEUED_CASE", "TOOL_CASE", "DELEGATE_CASE", "MCP_CASE", "PRIVATE_CASE", "C05_STREAM", "F02_STREAM", "GOAL_QUEUE_START", "GOAL_QUEUE_GUIDANCE", 'F03_CARD', 'F03_MCP_CARD', 'F03_APPS_CARD', "F01_BACKGROUND_JOBS", "F01_CONTEXT_PAGED", "F01_CONTEXT_CHILD", "F02_CHAT_SCHEDULE", "F02_AGENT_JOB"];
  return markers
    .flatMap((marker) => {
      const position = body.lastIndexOf(marker);
      return position >= 0 ? [{ marker, position }] : [];
    })
    .sort((left, right) => right.position - left.position)[0]?.marker ?? "DEFAULT_CASE";
}

function currentTurnHasToolResult(body: ProviderRequest): boolean {
  const messages = body.messages ?? [];
  let last_user = -1;
  messages.forEach((message, index) => {
    if (message.role === "user") last_user = index;
  });
  return messages.slice(last_user + 1).some((message) => message.role === "tool");
}

function textFrames(response_id: string, text: string): readonly object[] {
  return [
    { id: response_id, model: "offline-model", choices: [{ index: 0, delta: { role: "assistant", content: text.slice(0, 4) }, finish_reason: null }] },
    { id: response_id, model: "offline-model", choices: [{ index: 0, delta: { content: text.slice(4) }, finish_reason: null }] },
    { id: response_id, model: "offline-model", choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 120, completion_tokens: 20, total_tokens: 140, prompt_tokens_details: { cached_tokens: 80 } } },
  ];
}

function toolCallFrames(response_id: string): readonly object[] {
  return [
    {
      id: response_id,
      model: "offline-model",
      choices: [{
        index: 0,
        delta: {
          role: "assistant",
          tool_calls: [{
            index: 0,
            id: "call-e2e-write",
            type: "function",
            function: {
              name: "write_file",
              arguments: "{\"path\":\"e2e-context-file.txt\",\"content\":\"formal host resource projection\"}",
            },
          }],
        },
        finish_reason: null,
      }],
    },
    { id: response_id, model: "offline-model", choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }], usage: { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110, prompt_tokens_details: { cached_tokens: 40 } } },
  ];
}

function backgroundShellFrames(response_id: string): readonly object[] {
  const common = { id: response_id, model: "offline-model" };
  const tool_calls = Array.from({ length: 6 }, (_, index) => ({
    index, id: `call-f01-background-${response_id}-${index}`, type: "function",
    function: {
      name: "shell",
      arguments: JSON.stringify({
        command: `printf 'BOOT${index}\\n'; i=1; while [ "$i" -le 250 ]; do printf 'line-%04d 中文🌿 output retained for upward pagination\\n' "$i"; i=$((i+1)); done; printf 'TAIL${index}\\n'; while [ ! -f f01-go-${index} ]; do sleep 0.1; done; printf 'UPDATE${index}\\n'; while [ ! -f f01-exit-${index} ]; do sleep 0.1; done; exit 7`,
        process_mode: "background", wait_ms: 0, timeout_ms: 45_000,
      }),
    },
  }));
  return [
    { ...common, choices: [{ index: 0, delta: { role: "assistant", tool_calls }, finish_reason: null }] },
    { ...common, choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }], usage: { prompt_tokens: 100, completion_tokens: 40, total_tokens: 140 } },
  ];
}

function scheduleFrames(response_id: string, body: ProviderRequest, marker: string, has_tool_result: boolean) {
  const user = body.messages?.findLast((message) => message.role === "user");
  const schedule_id = JSON.stringify(user?.content)?.match(/F02_CHAT_SCHEDULE ([a-zA-Z0-9_-]+)/)?.[1];
  const is_chat = marker === "F02_CHAT_SCHEDULE";
  if (has_tool_result) {
    const raw = JSON.stringify(body.messages?.findLast((message) => message.role === "tool"));
    const text = is_chat
      ? (raw.includes("false") ? "聊天已暂停定时任务。" : "聊天任务操作已结束。")
      : "**Agent 归档汇报**\n\n- 生成 `schedule-report.txt`\n- 保存结果和明细";
    return textFrames(response_id, text);
  }
  if (is_chat && !schedule_id) return textFrames(response_id, "缺少计划标识");
  const calls = is_chat
    ? [{ name: "manage_schedules", arguments: { operation: "set_enabled", schedule_id, enabled: false } }]
    : [
      { name: "declare_artifact", arguments: { action: "declare", path: "schedule-report.txt", title: "定时归档文件" } },
      { name: "declare_artifact", arguments: { action: "declare", path: "schedule-details.txt", title: "定时明细文件" } },
    ];
  const tool_calls = calls.map((call, index) => ({
    index, id: `schedule-${response_id}-${index}`, type: "function",
    function: { name: call.name, arguments: JSON.stringify(call.arguments) },
  }));
  const common = { id: response_id, model: "offline-model" };
  return [
    { ...common, choices: [{ index: 0, delta: { role: "assistant", tool_calls }, finish_reason: null }] },
    { ...common, choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] },
  ];
}

function contextPageFrames(response_id: string, body: ProviderRequest): readonly object[] {
  const user = body.messages?.filter((message) => message.role === "user").at(-1)?.content;
  const n = JSON.stringify(user)?.match(/F01_CONTEXT_PAGED (\d+)/)?.[1];
  if (!n) return textFrames(response_id, "分页夹具");
  const tool_calls = [
    { name: "declare_artifact", arguments: { action: "declare", path: `deliver-${n}.txt`, title: `分页产物 ${n}` } },
    { name: "delegate_task", arguments: { title: `分页子任务 ${n}`, task: `F01_CONTEXT_CHILD ${n}` } },
  ].map((call, index) => ({ index, id: `f01-context-${n}-${index}`, type: "function",
    function: { name: call.name, arguments: JSON.stringify(call.arguments) } }));
  return [
    { id: response_id, model: "offline-model", choices: [{ index: 0, delta: { role: "assistant", tool_calls }, finish_reason: null }] },
    { id: response_id, model: "offline-model", choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }], usage: { prompt_tokens: 100, completion_tokens: 40, total_tokens: 140 } },
  ];
}

function mcpToolCallFrames(response_id: string): readonly object[] {
  return [
    { id: response_id, model: "offline-model", choices: [{ index: 0, delta: {
      role: "assistant", tool_calls: [{ index: 0, id: `call-mcp-${response_id}`, type: "function",
        function: { name: "call_mcp_tool", arguments: JSON.stringify({ server: "local_fixture", tool: "first_tool", arguments: { value: "e2e" } }) },
      }],
    }, finish_reason: null }] },
    { id: response_id, model: "offline-model", choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] },
  ];
}

function delegateTaskFrames(response_id: string): readonly object[] {
  return [
    {
      id: response_id,
      model: "offline-model",
      choices: [{
        index: 0,
        delta: {
          role: "assistant",
          tool_calls: [{
            index: 0,
            id: "call-e2e-delegate",
            type: "function",
            function: {
              name: "delegate_task",
              arguments: "{\"title\":\"E2E 子任务\",\"task\":\"返回一条正式 Host 子任务结果。\"}",
            },
          }],
        },
        finish_reason: null,
      }],
    },
    { id: response_id, model: "offline-model", choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }], usage: { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110, prompt_tokens_details: { cached_tokens: 40 } } },
  ];
}

function closeServer(server: Server): Promise<void> {
  return new Promise((resolve_close, reject_close) => server.close((error) => error ? reject_close(error) : resolve_close()));
}

async function waitForDiscovery(
  runtime_home: string,
  child: ChildProcess,
  stderr: () => string,
): Promise<Discovery> {
  const deadline = Date.now() + STARTUP_TIMEOUT_MS;
  const discovery_path = join(runtime_home, "run/runtime.json");
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`Runtime Host exited before readiness: ${stderr()}`);
    }
    try {
      const discovery = JSON.parse(await readFile(discovery_path, "utf8")) as Discovery;
      const response = await fetch(`${discovery.address}/health`, {
        headers: { ...compatibilityHeaders(), Authorization: `Bearer ${discovery.access_token}` },
      });
      if (response.ok && (await response.json() as { status: string }).status === "ready") {
        return discovery;
      }
    } catch {
      // Discovery precedes recovery; wait for actual readiness before fixture commands.
    }
    await delay(40);
  }
  throw new Error(`Runtime Host did not become ready: ${stderr()}`);
}

async function runtimeCommand(
  discovery: Discovery,
  type: string,
  payload: object,
): Promise<unknown> {
  const request_id = crypto.randomUUID();
  const response = await fetch(`${discovery.address}/commands`, {
    method: "POST",
    headers: { ...compatibilityHeaders(), Authorization: `Bearer ${discovery.access_token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      request_id,
      command: { scope: "runtime", payload: { type, payload } },
    }),
  });
  const body = await response.json() as {
    readonly result?: { readonly payload?: { readonly payload?: unknown } };
    readonly error?: { readonly message?: string };
  };
  if (!response.ok) {
    throw new Error(body.error?.message ?? `Runtime command ${type} failed`);
  }
  return body.result?.payload?.payload;
}

async function getJson(url: string, access_token: string): Promise<unknown> {
  const response = await fetch(url, {
    headers: { ...compatibilityHeaders(), Authorization: `Bearer ${access_token}` },
  });
  if (!response.ok) {
    throw new Error(`Runtime request failed: ${response.status}`);
  }
  return response.json();
}

async function waitForExit(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null) {
    return;
  }
  await Promise.race([
    new Promise<void>((resolve_exit) => child.once("exit", () => resolve_exit())),
    delay(2_000).then(() => {
      child.kill("SIGTERM");
    }),
  ]);
}

async function removeTemporaryDirectories(...directories: readonly string[]): Promise<void> {
  if (process.env.EZ_ASSISTANT_KEEP_TEST_FIXTURES) {
    console.log(`retained fixtures: ${directories.join(", ")}`);
    return;
  }
  const [runtime_home] = directories;
  await chmod(runtime_home, 0o700).catch(() => undefined);
  await Promise.all(directories.map((directory) => rm(directory, { recursive: true, force: true })));
}

function delay(duration_ms: number): Promise<void> {
  return new Promise((resolve_delay) => setTimeout(resolve_delay, duration_ms));
}
