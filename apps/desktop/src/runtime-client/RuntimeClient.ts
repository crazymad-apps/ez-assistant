import { DesktopSocket, type DesktopEvent } from "./DesktopSocket";
import { compatibilityHeaders } from "@ez-assistant/protocol";
import { hostCompatibilityError, compatibilityMessage, isCompatibilityCode } from "./compatibility";
import { TerminalSocket, type TerminalSource, type TerminalSize, type TerminalEvent } from "./TerminalSocket";
import type {
  HostLoginResult, HostPasswordRequest, HostLogoutResult,
  ShellKind, UserTerminalShellEntry,
  DeviceGatewayCommand,
  HostAccessCommand,
  HostAccessStatus,
  DeviceGatewayCommandResult,
  DeviceGatewayEvent,
  RuntimeCommand,
  RuntimeCommandResult,
  RuntimeErrorInfo,
  RuntimeEventEnvelope,
  RuntimeHostCapabilities,
  RuntimeHostHealth,
  PluginArchiveFormat,
  PluginPackageSelection,
  PluginOperationSnapshot,
  PluginConfigFileSnapshot,
  PluginConfigFilesSnapshot,
} from "@ez-assistant/protocol";
import type { RuntimeBootstrap } from "../native-bridge/runtimeBootstrap";

type RuntimeCommandResponse = {
  readonly request_id: string;
  readonly result: {
    readonly scope: "runtime";
    readonly payload: RuntimeCommandResult;
  };
};

type DeviceGatewayCommandResponse = {
  readonly request_id: string;
  readonly result: {
    readonly scope: "device_gateway";
    readonly payload: DeviceGatewayCommandResult;
  };
};

type CommandFailure = {
  readonly request_id?: string | null;
  readonly error: RuntimeErrorInfo & { target_device?: string; outcome?: "unknown" | "not_executed" };
};

export class RuntimeClientError extends Error {
  readonly code: string;

  constructor(code: string, message: string, readonly target_device: string | null = null, readonly outcome: "unknown" | "not_executed" | null = null) {
    super(message);
    this.name = "RuntimeClientError";
    this.code = code;
  }
}

export type RuntimeEventListener = {
  readonly onEvent: (event: RuntimeEventEnvelope) => void;
  readonly onDeviceGatewayEvent: (event: DeviceGatewayEvent) => void;
  readonly onGap: () => void;
};

export type RuntimeEventConnection = {
  readonly closed: Promise<void>;
};

export class RuntimeClient {
  readonly login_context: string | null;
  readonly instance_id: string;
  readonly capabilities: RuntimeHostCapabilities;
  readonly address: string;

  readonly #base_url: string;
  readonly #access_token: string;
  readonly #abort = new AbortController();

  constructor(private readonly bootstrap: RuntimeBootstrap, private readonly on_unauthorized?: (code: string) => void, readonly target_device: string | null = null) {
    this.login_context = bootstrap.login_context ?? null;
    this.#base_url = bootstrap.base_url;
    this.#access_token = bootstrap.access_token;
    this.instance_id = bootstrap.instance_id;
    this.capabilities = bootstrap.capabilities;
    this.address = new URL(bootstrap.base_url).origin;
  }

  async supportsNetworkProxy(): Promise<boolean> {
    this.#requireLocal();
    return this.resource("/capabilities", { method: "GET", cache: "no-store" }, async (response) => response.headers.get("x-ez-network-proxy") === "2");
  }

  async forDevice(device: string): Promise<RuntimeClient> {
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(device)) throw new Error("设备编号无效。");
    if (!await this.supportsNetworkProxy()) throw new RuntimeClientError("network_proxy_unavailable", "当前 Host 尚不支持企业组网代理。");
    if (this.#abort.signal.aborted) throw new Error("本机连接已关闭。");
    const client = new RuntimeClient(this.bootstrap, undefined, device);
    const dispose = () => client.dispose();
    this.#abort.signal.addEventListener("abort", dispose, { once: true });
    client.#abort.signal.addEventListener("abort", () => this.#abort.signal.removeEventListener("abort", dispose), { once: true });
    return client;
  }

  #requireLocal(): void {
    if (this.target_device) throw new RuntimeClientError("remote_operation_unavailable", "此操作不支持企业组网设备。", this.target_device, "not_executed");
  }

  dispose(): void { this.#abort.abort(); }

  /** 一个阻塞请求等待本人 Runtime；取消页面只取消等待，不撤销后台初始化或任务。 */
  async waitUntilReady(receive: (health: RuntimeHostHealth) => void): Promise<void> {
    if (this.target_device) return;
    const incompatible = hostCompatibilityError(this.capabilities);
    if (incompatible) throw new RuntimeClientError("component_mismatch", compatibilityMessage(incompatible.code));
    // 就绪仍只有一个有副作用的等待入口；只读健康查询仅观察本人已有初始化。
    const observation = new AbortController();
    const signal = AbortSignal.any([observation.signal, this.#abort.signal]);
    const observing = (async () => {
      while (!signal.aborted) {
        await new Promise<void>((resolve) => {
          const finish = () => { clearTimeout(timer); signal.removeEventListener("abort", finish); resolve(); };
          const timer = setTimeout(finish, 500);
          signal.addEventListener("abort", finish, { once: true });
          if (signal.aborted) finish();
        });
        if (signal.aborted) return;
        try {
          const health = await this.resource<RuntimeHostHealth>("/health", { method: "GET", cache: "no-store", signal }, (response) => response.json());
          if (!signal.aborted && health.status !== "ready") receive(health);
        } catch {
          // 进度观察失败不重试初始化，也不替代 ensure-ready 的最终业务结果。
          return;
        }
      }
    })();
    try {
      await this.resource("/runtime/ensure-ready", { method: "POST" }, async () => undefined);
      receive({ status: "ready", stage: null, error: null, database_version: null,
        target_version: this.capabilities.runtime_version, min_compatible_host_version: undefined });
    } finally {
      observation.abort();
      await observing;
    }
  }

  session(): Promise<HostLoginResult> {
    return this.resource("/auth/session", { method: "GET", cache: "no-store" }, (response) => response.json());
  }

  async changePassword(request: HostPasswordRequest): Promise<void> {
    await this.resource("/auth/password", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(request) }, async () => undefined);
  }

  logout(): Promise<HostLogoutResult | null> {
    return this.resource("/auth/logout", { method: "POST" }, (response) => response.status === 204 ? Promise.resolve(null) : response.json());
  }

  inspectPluginPackage(file: File, format: PluginArchiveFormat, signal?: AbortSignal): Promise<PluginPackageSelection> {
    return this.resource("/plugins/inspect", {
      method: "POST", body: file, signal,
      headers: { "x-ez-plugin-format": format },
    }, (response) => response.json() as Promise<PluginPackageSelection>);
  }

  uploadPluginPackage(key: string, file: File, signal?: AbortSignal): Promise<PluginOperationSnapshot> {
    return this.resource(`/plugins/operations/${encodeURIComponent(key)}/package`, {
      method: "POST", body: file, signal,
    }, (response) => response.json() as Promise<PluginOperationSnapshot>);
  }

  listPluginConfigFiles(name: string, sha256: string): Promise<PluginConfigFilesSnapshot> {
    return this.resource(`/plugins/${encodeURIComponent(name)}/config-files?sha256=${encodeURIComponent(sha256)}`,
      { method: "GET" }, (response) => response.json() as Promise<PluginConfigFilesSnapshot>);
  }

  uploadPluginConfigFile(name: string, sha256: string, relative: string, revision: string, file: File): Promise<PluginConfigFileSnapshot> {
    const path = relative.split("/").map(encodeURIComponent).join("/");
    return this.resource(`/plugins/${encodeURIComponent(name)}/config-files/${path}`, {
      method: "PUT", body: file,
      headers: { "x-ez-plugin-sha256": sha256, "x-ez-expected-revision": revision },
    }, (response) => response.json() as Promise<PluginConfigFileSnapshot>);
  }

  listUserTerminalShells(signal?: AbortSignal): Promise<readonly UserTerminalShellEntry[]> {
    return this.resource("/user-terminals/shells", { method: "GET", cache: "no-store", signal }, (response) => response.json());
  }

  openDesktop(device: string, receive: (event: DesktopEvent) => void, display: string | null = null): DesktopSocket {
    return new DesktopSocket(this.#base_url, this.#access_token, device, this.login_context, receive, this.#abort.signal, display);
  }

  openUserTerminal(source: TerminalSource, size: TerminalSize, receive: (event: TerminalEvent) => void, shell: ShellKind | null = null, opening?: { signal: AbortSignal; timeout_ms: number }): TerminalSocket {
    return new TerminalSocket(this.#base_url, this.#access_token, source, size, receive, opening ? AbortSignal.any([this.#abort.signal, opening.signal]) : this.#abort.signal, shell, this.login_context, this.target_device, opening?.timeout_ms);
  }

  /** 文件 HTTP 请求复用当前连接的凭据、取消和登录失效处理，不重新发现本机 Host。 */
  async resource<T>(path: string, init: RequestInit, consume: (response: Response) => Promise<T>): Promise<T> {
    if (this.target_device && !/^\/(sessions\/|session-materializations(?:$|\?)|host-files\/|user-terminals\/shells$)/.test(path)) this.#requireLocal();
    if (!path.startsWith("/") || path.startsWith("//")) throw new Error("资源路径无效。");
    const abort = new AbortController();
    const cancel = () => abort.abort();
    this.#abort.signal.addEventListener("abort", cancel, { once: true });
    init.signal?.addEventListener("abort", cancel, { once: true });
    if (this.#abort.signal.aborted || init.signal?.aborted) cancel();
    // 请求和响应正文共用此取消域；连接 dispose 会中止尚未读完的下载。
    try {
      const headers = this.#headers(false);
      new Headers(init.headers).forEach((value, key) => headers.set(key, value));
      if (this.target_device) headers.set("x-ez-target-device", this.target_device);
      const response = await this.#fetch(`${this.#base_url}${path}`, {
        ...init, signal: abort.signal, headers,
      });
      if (!response.ok) throw await decodeCommandFailure(response);
      return await consume(response);
    } finally {
      this.#abort.signal.removeEventListener("abort", cancel);
      init.signal?.removeEventListener("abort", cancel);
    }
  }

  async hostAccessCommand(command: HostAccessCommand): Promise<HostAccessStatus> {
    this.#requireLocal();
    const request_id = createRequestId();
    const response = await this.#fetch(`${this.#base_url}/commands`, {
      method: "POST", headers: this.#headers(true), body: JSON.stringify({ request_id, command: { scope: "host_access", payload: command } }),
    });
    if (!response.ok) throw await decodeCommandFailure(response);
    const result = await response.json() as { request_id: string; result: { scope: string; payload: HostAccessStatus } };
    if (result.request_id !== request_id || result.result.scope !== "host_access") throw new RuntimeClientError("protocol_mismatch", "Host 返回了不匹配的访问设置。");
    return result.result.payload;
  }

  async #fetch(input: string, init: RequestInit): Promise<Response> {
    const headers = new Headers(init.headers);
    Object.entries(compatibilityHeaders()).forEach(([key, value]) => headers.set(key, value));
    const response = await fetch(input, { ...init, headers, credentials: this.#access_token ? "omit" : "same-origin", redirect: "error", signal: init.signal ? AbortSignal.any([init.signal, this.#abort.signal]) : this.#abort.signal });
    if (response.status === 401 && !this.target_device) {
      this.on_unauthorized?.("authentication_required");
      throw new RuntimeClientError("authentication_required", "登录已失效，请重新登录。");
    }
    if (response.status === 409 && !this.target_device) {
      const failure = await decodeCommandFailure(response.clone());
      if (failure.code === "login_context_changed") {
        this.on_unauthorized?.(failure.code);
        throw failure;
      }
    }
    return response;
  }

  async command<TType extends RuntimeCommand["type"]>(
    command: Extract<RuntimeCommand, { readonly type: TType }>,
    options?: Readonly<{ signal?: AbortSignal }>,
  ): Promise<Extract<RuntimeCommandResult, { readonly type: TType }>> {
    if (this.target_device && !REMOTE_BUSINESS_COMMANDS.has(command.type)) this.#requireLocal();
    const request_id = createRequestId();
    const response = await this.#fetch(`${this.#base_url}/commands`, {
      method: "POST",
      signal: options?.signal,
      headers: this.#headers(true),
      body: JSON.stringify({
        request_id,
        command: { scope: "runtime", payload: command },
      }),
    });
    if (!response.ok) {
      throw await decodeCommandFailure(response);
    }
    const body = (await response.json()) as RuntimeCommandResponse;
    if (
      body.request_id !== request_id ||
      body.result.scope !== "runtime" ||
      body.result.payload.type !== command.type
    ) {
      throw new RuntimeClientError("protocol_mismatch", "Runtime 返回了不匹配的命令结果。");
    }
    return body.result.payload as Extract<RuntimeCommandResult, { readonly type: TType }>;
  }

  async deviceGatewayCommand(command: DeviceGatewayCommand): Promise<DeviceGatewayCommandResult> {
    this.#requireLocal();
    const request_id = createRequestId();
    const response = await this.#fetch(`${this.#base_url}/commands`, {
      method: "POST",
      headers: this.#headers(true),
      body: JSON.stringify({
        request_id,
        command: { scope: "device_gateway", payload: command },
      }),
    });
    if (!response.ok) {
      throw await decodeCommandFailure(response);
    }
    const body = (await response.json()) as DeviceGatewayCommandResponse;
    if (
      body.request_id !== request_id
      || body.result.scope !== "device_gateway"
      || body.result.payload.type !== command.type
    ) {
      throw new RuntimeClientError("protocol_mismatch", "Host 返回了不匹配的设备命令结果。");
    }
    return body.result.payload;
  }

  async connectEvents(
    listener: RuntimeEventListener,
    signal: AbortSignal,
  ): Promise<RuntimeEventConnection> {
    const capabilities = this.target_device ? this.capabilities : await this.resource("/capabilities", { method: "GET", cache: "no-store", signal }, async (response) => await response.json() as RuntimeHostCapabilities);
    const incompatible = hostCompatibilityError(capabilities);
    if (incompatible) throw new RuntimeClientError(incompatible.code, compatibilityMessage(incompatible.code));
    const response = await this.#fetch(`${this.#base_url}/events`, {
      headers: this.#headers(false),
      signal,
    });
    if (!response.ok) throw await decodeCommandFailure(response);
    if (!response.body) {
      throw new RuntimeClientError("event_stream_unavailable", "无法建立 Runtime 事件流。");
    }
    return {
      closed: consumeEventStream(response.body, listener, signal),
    };
  }

  #headers(json: boolean): Headers {
    const headers = new Headers();
    if (this.#access_token) headers.set("Authorization", `Bearer ${this.#access_token}`);
    if (!this.#access_token && this.login_context) headers.set("x-ez-login-context", this.login_context);
    if (this.target_device) headers.set("x-ez-target-device", this.target_device);
    if (json) {
      headers.set("Content-Type", "application/json");
    }
    return headers;
  }
}

async function decodeCommandFailure(response: Response): Promise<RuntimeClientError> {
  try {
    const body = (await response.json()) as CommandFailure;
    if (body.error && typeof body.error.code === "string") {
      return new RuntimeClientError(body.error.code, isCompatibilityCode(body.error.code) ? compatibilityMessage(body.error.code) : body.error.message, body.error.target_device ?? null, body.error.outcome ?? null);
    }
  } catch {
    // Transport fallback below intentionally avoids exposing a response body.
  }
  return new RuntimeClientError("transport_error", `Runtime 请求失败（${response.status}）。`);
}

async function consumeEventStream(
  body: ReadableStream<Uint8Array>,
  listener: RuntimeEventListener,
  signal: AbortSignal,
): Promise<void> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  try {
    while (!signal.aborted) {
      const chunk = await reader.read();
      if (chunk.done) {
        break;
      }
      buffer += decoder.decode(chunk.value, { stream: true }).replaceAll("\r\n", "\n");
      let boundary = buffer.indexOf("\n\n");
      while (boundary >= 0) {
        const frame = buffer.slice(0, boundary);
        buffer = buffer.slice(boundary + 2);
        dispatchFrame(frame, listener);
        boundary = buffer.indexOf("\n\n");
      }
    }
  } finally {
    reader.releaseLock();
  }
}

function dispatchFrame(frame: string, listener: RuntimeEventListener): void {
  let event_name = "message";
  const data: string[] = [];
  for (const line of frame.split("\n")) {
    if (line.startsWith("event:")) {
      event_name = line.slice(6).trim();
    } else if (line.startsWith("data:")) {
      data.push(line.slice(5).trimStart());
    }
  }
  if (event_name === "stream_gap") {
    listener.onGap();
    return;
  }
  if (data.length === 0) {
    return;
  }
  try {
    if (event_name === "runtime_event") {
      listener.onEvent(JSON.parse(data.join("\n")) as RuntimeEventEnvelope);
    } else if (event_name === "device_gateway_event") {
      listener.onDeviceGatewayEvent(JSON.parse(data.join("\n")) as DeviceGatewayEvent);
    }
  } catch {
    listener.onGap();
  }
}

function createRequestId(): string {
  return typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : `desktop-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

// 与 F02 业务准入保持一致；本机控制在各自入口独立拒绝。
const REMOTE_BUSINESS_COMMANDS = new Set<RuntimeCommand["type"]>([
  "register_workspace",
  "update_workspace",
  "get_workspace",
  "list_workspaces",
  "remove_workspace",
  "restore_workspace",
  "get_application_snapshot",
  "get_session_view",
  "get_child_task_view",
  "list_conversation_page",
  "get_conversation_page_around_run",
  "get_conversation_page_around_message",
  "search_conversation_history",
  "get_conversation_recall_window",
  "get_tool_detail",
  "get_attachment",
  "list_attachments",
  "create_session",
  "fork_session",
  "prepare_delete_session",
  "delete_session",
  "clear_session",
  "compact_session",
  "cancel_session_compaction",
  "list_sessions",
  "get_session",
  "get_session_materialization",
  "submit_input",
  "archive_session",
  "restore_session",
  "rename_session",
  "generate_session_title",
  "set_message_feedback",
  "reenter_from_user_message",
  "prioritize_queued_input",
  "interrupt_run",
  "resume_queued_input",
  "reject_approval_and_stop_run",
  "cancel_queued_input",
  "resume_session",
  "retry_run",
  "get_run",
  "list_runs",
  "list_child_tasks",
  "get_child_task",
  "cancel_child_task",
  "cancel_run",
  "clear_work_plan",
  "stop_goal",
  "resume_goal",
  "clear_goal",
  "list_pending_approvals",
  "decide_approval",
  "list_feedback_requests",
  "answer_feedback",
  "cancel_feedback",
  "set_session_model",
  "set_session_reasoning_effort",
  "set_session_variant",
  "set_session_approval_mode",
  "reload_permissions",
  "list_providers",
  "list_provider_models",
  "get_model_settings",
  "get_model_configuration",
  "list_fixed_model_configs",
  "get_agent_shell_settings",
  "list_custom_agents",
  "prepare_custom_agent",
  "list_skills",
  "get_skill_detail",
  "list_mcp_server_options",
  "submit_session_command",
  "get_permission_document",
  "replace_permission_document",
]);
