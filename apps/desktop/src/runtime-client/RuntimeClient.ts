import type {
  DeviceGatewayCommand,
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
  readonly started_runtime: boolean;
  readonly address: string;

  readonly #base_url: string;
  readonly #access_token: string;

  constructor(bootstrap: RuntimeBootstrap) {
    this.#base_url = bootstrap.base_url;
    this.#access_token = bootstrap.access_token;
    this.instance_id = bootstrap.instance_id;
    this.capabilities = bootstrap.capabilities;
    this.started_runtime = bootstrap.started_runtime;
    this.address = new URL(bootstrap.base_url).origin;
  }

  async command<TType extends RuntimeCommand["type"]>(
    command: Extract<RuntimeCommand, { readonly type: TType }>,
  ): Promise<Extract<RuntimeCommandResult, { readonly type: TType }>> {
    const request_id = createRequestId();
    const response = await fetch(`${this.#base_url}/commands`, {
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
    const response = await fetch(`${this.#base_url}/commands`, {
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
    const response = await fetch(`${this.#base_url}/events`, {
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
    const headers = new Headers({ Authorization: `Bearer ${this.#access_token}` });
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
