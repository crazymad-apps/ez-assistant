import { TerminalSocket, type TerminalSource, type TerminalSize, type TerminalEvent } from "./TerminalSocket";
import type {
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
} from "../generated/assistant-protocol";
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
  readonly error: RuntimeErrorInfo;
};

export class RuntimeClientError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
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
  readonly instance_id: string;
  readonly capabilities: RuntimeHostCapabilities;
  readonly address: string;

  readonly #base_url: string;
  readonly #access_token: string;
  readonly #abort = new AbortController();

  constructor(bootstrap: RuntimeBootstrap, private readonly on_unauthorized?: () => void) {
    this.#base_url = bootstrap.base_url;
    this.#access_token = bootstrap.access_token;
    this.instance_id = bootstrap.instance_id;
    this.capabilities = bootstrap.capabilities;
    this.address = new URL(bootstrap.base_url).origin;
  }

  dispose(): void { this.#abort.abort(); }

  openUserTerminal(source: TerminalSource, size: TerminalSize, receive: (event: TerminalEvent) => void): TerminalSocket {
    return new TerminalSocket(this.#base_url, this.#access_token, source, size, receive, this.#abort.signal);
  }

  /** 文件 HTTP 请求复用当前连接的凭据、取消和登录失效处理，不重新发现本机 Host。 */
  async resource<T>(path: string, init: RequestInit, consume: (response: Response) => Promise<T>): Promise<T> {
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
    const response = await fetch(input, { ...init, credentials: "same-origin", redirect: "error", signal: init.signal ?? this.#abort.signal });
    if (response.status === 401) {
      this.on_unauthorized?.();
      throw new RuntimeClientError("authentication_required", "登录已失效，请重新登录。");
    }
    return response;
  }

  async command<TType extends RuntimeCommand["type"]>(
    command: Extract<RuntimeCommand, { readonly type: TType }>,
  ): Promise<Extract<RuntimeCommandResult, { readonly type: TType }>> {
    const request_id = createRequestId();
    const response = await this.#fetch(`${this.#base_url}/commands`, {
      method: "POST",
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
    const response = await this.#fetch(`${this.#base_url}/events`, {
      headers: this.#headers(false),
      signal,
    });
    if (!response.ok || !response.body) {
      throw new RuntimeClientError("event_stream_unavailable", "无法建立 Runtime 事件流。");
    }
    return {
      closed: consumeEventStream(response.body, listener, signal),
    };
  }

  #headers(json: boolean): Headers {
    const headers = new Headers();
    if (this.#access_token) headers.set("Authorization", `Bearer ${this.#access_token}`);
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
      return new RuntimeClientError(body.error.code, body.error.message);
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
