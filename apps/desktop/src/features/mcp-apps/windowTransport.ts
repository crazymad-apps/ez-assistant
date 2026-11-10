import { JSONRPCMessageSchema } from '@modelcontextprotocol/core';

type JSONRPCMessage = ReturnType<typeof JSONRPCMessageSchema.parse>;

/** 只接收当前 Window 和指定有效来源；替代 SDK 会输出原始消息的通用 transport。 */
export class AppWindowTransport {
  onrequest?: (request: { id: string | number; method: string; params?: Record<string, unknown> }) => boolean;
  onmessage?: (message: JSONRPCMessage) => void;
  onerror?: (error: Error) => void;
  onclose?: () => void;
  #closed = false;
  #pending = new Set<string>();
  #gesture_requests = new Set<string>();
  #initialized = false;
  readonly #gesture_channel: string | undefined;

  constructor(
    private readonly target: Window,
    private readonly origin: string,
    private readonly retire: () => void,
    track_tool_gestures = false,
  ) {
    if (track_tool_gestures) {
      this.#gesture_channel = crypto.getRandomValues(new Uint32Array(4)).join('-');
    }
  }

  async start(): Promise<void> {
    window.addEventListener('message', this.#receive);
  }

  async send(message: JSONRPCMessage): Promise<void> {
    if (this.#closed) throw new Error('应用连接已结束');
    if ('id' in message && !('method' in message)) {
      const id = `${typeof message.id}:${String(message.id)}`;
      this.#pending.delete(id);
      this.#gesture_requests.delete(id);
    }
    if (
      this.#gesture_channel &&
      'method' in message && message.method === 'ui/notifications/sandbox-resource-ready'
    ) {
      // 该消息由代理消费；通道值不会进入应用 HTML 或普通 MCP 消息。
      message = { ...message, params: { ...message.params, ez_assistant_gesture_channel: this.#gesture_channel } };
    }
    this.target.postMessage(message, this.origin === 'null' ? '*' : this.origin);
  }

  consumeToolGesture(id: string | number): boolean {
    return this.#gesture_requests.delete(`${typeof id}:${String(id)}`);
  }

  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    window.removeEventListener('message', this.#receive);
    this.#pending.clear();
    this.#gesture_requests.clear();
    this.onclose?.();
  }

  #invalidate(): void {
    if (this.#closed) return;
    void this.close();
    this.retire();
  }

  #receive = (event: MessageEvent<unknown>): void => {
    if (this.#closed || event.source !== this.target) return;
    if (event.origin !== this.origin) {
      this.#invalidate();
      return;
    }
    try {
      const wire = event.data;
      if (!wire || typeof wire !== 'object' || Array.isArray(wire)) throw new Error('消息无效');
      const { ez_assistant_gesture, ...data } = wire as Record<string, unknown>;
      const user_gesture = !!this.#gesture_channel && ez_assistant_gesture === this.#gesture_channel;
      if (new TextEncoder().encode(JSON.stringify(user_gesture ? data : wire)).length > 256 * 1024)
        throw new Error('消息过大');
      const message = JSONRPCMessageSchema.parse(data);
      if ('method' in message && message.method === 'ez-assistant/notifications/retired') {
        this.#invalidate();
        return;
      }
      if ('method' in message && 'id' in message) {
        if (message.method === 'ui/initialize') {
          if (this.#initialized) {
            this.#invalidate();
            return;
          }
          this.#initialized = true;
        }
        const id = `${typeof message.id}:${String(message.id)}`;
        if (this.#pending.has(id) || this.#pending.size >= 8) {
          this.#invalidate();
          return;
        }
        this.#pending.add(id);
        if (message.method === 'tools/call' && user_gesture) {
          this.#gesture_requests.add(id);
        }
        if (this.onrequest?.(message)) return;
      }
      this.onmessage?.(message);
    } catch {
      // 不输出原始载荷：工具私有 _meta 可能包含业务敏感信息。
      this.onerror?.(new Error('应用消息无效或超过限制'));
      this.#invalidate();
    }
  };
}
