import { spawn, type ChildProcess } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import type { FullConfig } from "@playwright/test";

type Discovery = {
  readonly address: string;
  readonly instance_id: string;
  readonly access_token: string;
};

const STARTUP_TIMEOUT_MS = 10_000;

export default async function setupRuntimeHost(_config: FullConfig): Promise<() => Promise<void>> {
  const runtime_home = await mkdtemp(join(tmpdir(), "ez-assistant-e2e-runtime-"));
  const workspace = await mkdtemp(join(tmpdir(), "ez-assistant-e2e-workspace-"));
  const additional_workspace = await mkdtemp(join(tmpdir(), "ez-assistant-e2e-added-workspace-"));
  const provider = await startFakeProvider();
  const mcp_secret = "e2e-mcp-secret-must-not-leak-9273";
  const user_directory = join(runtime_home, "fixture-user-home");
  await mkdir(user_directory);
  await writeFile(
    join(runtime_home, "config.toml"),
    `schema_version = 1
default_model = "fixture"

[models.fixture]
protocol = "chat_completions"
provider = "fixture"
endpoint = "${provider.endpoint}/v1"
model = "offline-model"
api_key = "e2e-placeholder-not-a-real-secret"
context_window_tokens = 8192
max_output_tokens = 4096

[models.alternate]
protocol = "chat_completions"
provider = "fixture"
endpoint = "${provider.endpoint}/v1"
model = "alternate-offline-model"
api_key = "e2e-placeholder-not-a-real-secret"
context_window_tokens = 16384
max_output_tokens = 4096
`,
  );

  const executable = resolve(process.cwd(), "../../target/debug/ez-assistant-runtime");
  // 只在隔离 Runtime Home 中启用安全 stdio fixture，不读取或变更用户 MCP 配置。
  await writeFile(join(runtime_home, "mcp.json"), JSON.stringify({ mcpServers: {
    local_fixture: {
      command: "python3", args: ["-u", resolve(process.cwd(), "../runtime-host/tests/fixtures/mcp_stdio_server.py")],
      displayName: "MCP fixture", description: "Two safe fixture tools",
      env: { TOKEN: mcp_secret },
    },
  } }));
  const child = spawn(executable, ["serve", "--runtime-home", runtime_home], {
    // Home 仅覆盖测试子进程，避免正式技能发现读取开发者个人目录。
    env: { ...process.env, HOME: user_directory },
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  child.stderr?.on("data", (chunk: Buffer) => {
    stderr += chunk.toString("utf8");
  });

  try {
    const discovery = await waitForDiscovery(runtime_home, child, () => stderr);
    const capabilities = await getJson(`${discovery.address}/capabilities`, discovery.access_token);
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
      model_key: null,
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
    process.env.EZ_ASSISTANT_E2E_MCP_SECRET = mcp_secret;
  } catch (error) {
    child.kill("SIGTERM");
    await provider.close();
    await removeTemporaryDirectories(runtime_home, workspace, additional_workspace);
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
    await removeTemporaryDirectories(runtime_home, workspace, additional_workspace);
    delete process.env.EZ_ASSISTANT_E2E_BOOTSTRAP;
    delete process.env.EZ_ASSISTANT_E2E_NEW_WORKSPACE;
    delete process.env.EZ_ASSISTANT_E2E_MCP_SECRET;
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
    if (marker === "MCP_CASE") {
      frames = has_tool_result
        ? textFrames(response_id, JSON.stringify(body.messages?.filter((message) => message.role === "tool").at(-1)).includes("called:first_tool") ? "MCP 调用已回传。" : "MCP 调用已拒绝。")
        : mcpToolCallFrames(response_id);
    } else if (marker === "TOOL_CASE" && !has_tool_result) {
      frames = toolCallFrames(response_id);
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
    headers: { Authorization: `Bearer ${discovery.access_token}` },
    body,
  });
  if (!response.ok) {
    throw new Error(`temporary Runtime attachment upload failed: ${response.status} ${await response.text()}`);
  }
}

type ProviderMessage = Readonly<{ role?: string; content?: unknown }>;
type ProviderRequest = Readonly<{ messages?: readonly ProviderMessage[] }>;

function latestMarker(body: string): string {
  const markers = ["FIRST_CASE", "BLOCK_FOR_QUEUE", "QUEUED_CASE", "TOOL_CASE", "DELEGATE_CASE", "MCP_CASE"];
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
        headers: { Authorization: `Bearer ${discovery.access_token}` },
      });
      if (response.ok) {
        return discovery;
      }
    } catch {
      // Discovery is published atomically after Runtime recovery; poll until it is visible.
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
    headers: {
      Authorization: `Bearer ${discovery.access_token}`,
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
    headers: { Authorization: `Bearer ${access_token}` },
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
  const [runtime_home] = directories;
  await chmod(runtime_home, 0o700).catch(() => undefined);
  await Promise.all(directories.map((directory) => rm(directory, { recursive: true, force: true })));
}

function delay(duration_ms: number): Promise<void> {
  return new Promise((resolve_delay) => setTimeout(resolve_delay, duration_ms));
}
