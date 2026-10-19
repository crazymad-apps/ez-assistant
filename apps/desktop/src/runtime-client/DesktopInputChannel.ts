import type { DesktopInput, DesktopInputFrame } from "@ez-assistant/protocol";

export type DesktopControlEvent = { type: "control_ready" } | { type: "control_stopped"; message: string | null };
type Binding = Pick<DesktopInputFrame, "view_id" | "control_id" | "display_id">;
const messages: Record<string, string> = {
  control_denied: "当前账号没有桌面控制权限。",
  control_busy: "其他用户正在控制这台设备。",
  input_permission_required: "请在目标 Mac 的系统设置中允许辅助功能访问。",
  input_unavailable: "目标设备暂时无法接收输入，请检查系统权限和当前桌面。",
  input_stalled: "输入响应超时，已停止控制；未确认的操作不会重发。",
  input_failed: "目标设备无法继续接收输入，已停止控制。",
  invalid_input: "输入状态已失效，请重新开启控制。",
  control_ended: "控制已结束，可以重新开启控制。",
};

/** 一次查看的输入传输投影；许可由 Host 决定，ACK 只确认执行器接收，不确认应用业务结果。 */
export class DesktopInputChannel {
  #binding: Binding | null = null;
  #sequence = 0;
  #request = 0;
  #requesting = false;
  #closed = false;
  #pending = new Map<number, { at: number; bytes: number; resolve: () => void; reject: (error: Error) => void }>();
  #bytes = 0;
  #heartbeat = 0;
  #timer: ReturnType<typeof setInterval>;
  #requestTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(private readonly channel: RTCDataChannel, private readonly command: (value: unknown) => void,
    private readonly receive: (event: DesktopControlEvent) => void) {
    channel.onmessage = (event: MessageEvent<unknown>) => this.#message(event.data);
    channel.onclose = () => this.stop("输入连接已结束。");
    channel.onerror = () => this.stop("输入连接异常，已停止控制。");
    this.#timer = setInterval(() => {
      if (!this.#binding) return;
      const now = performance.now();
      const oldest = this.#pending.values().next().value;
      if (oldest && now - oldest.at >= 250) { this.stop(messages.input_stalled!); return; }
      if (now - this.#heartbeat >= 250) { this.#heartbeat = now; void this.send({ type: "heartbeat" }).catch(() => {}); }
    }, 50);
  }

  start(): void {
    if (this.#closed || this.#binding || this.#requesting) return;
    if (this.channel.readyState !== "open") { this.receive({ type: "control_stopped", message: "输入连接尚未就绪，请稍后重试。" }); return; }
    this.#requesting = true;
    this.#request += 1;
    this.command({ type: "control_start", request_id: this.#request });
    this.#requestTimer = setTimeout(() => {
      if (!this.#requesting) return;
      this.command({ type: "control_status" });
      this.#requestTimer = setTimeout(() => this.stop("控制请求未能确认，请重新开启控制。"), 2000);
    }, 8000);
  }

  #message(raw: unknown): void {
    if (this.#closed) return;
    try {
      if (typeof raw !== "string" || new TextEncoder().encode(raw).length > 4096) throw new Error();
      const value: unknown = JSON.parse(raw);
      if (!value || typeof value !== "object" || !("type" in value)) throw new Error();
      const data = value as Record<string, unknown>;
      if (data.type === "control_ready") {
        if (data.request_id !== this.#request || !this.#requesting) return;
        if (![data.view_id, data.control_id, data.display_id].every((id) => typeof id === "string" && id.length > 0 && id.length <= 128)) throw new Error();
        this.#binding = { view_id: data.view_id as string, control_id: data.control_id as string, display_id: data.display_id as string };
        this.#sequence = 0;
        this.#requesting = false;
        clearTimeout(this.#requestTimer);
        this.#heartbeat = performance.now();
        void this.send({ type: "heartbeat" }).catch(() => {});
        this.receive({ type: "control_ready" });
      } else if (data.type === "control_stopped") {
        if (data.request_id !== this.#request || (!this.#requesting && !this.#binding)) return;
        this.#clear();
        this.receive({ type: "control_stopped", message: messages[String(data.code)] ?? "控制已停止。" });
      } else if (data.type === "input_ack" && data.control_id === this.#binding?.control_id) {
        if (!Number.isSafeInteger(data.sequence) || Number(data.sequence) > this.#sequence) throw new Error();
        const pending = this.#pending.get(Number(data.sequence));
        if (pending) { this.#pending.delete(Number(data.sequence)); this.#bytes -= pending.bytes; pending.resolve(); }
      }
    } catch { this.stop("输入响应无效，已停止控制。"); }
  }

  send(input: DesktopInput): Promise<void> {
    if (this.#closed || !this.#binding || this.channel.readyState !== "open") return Promise.reject(new Error("控制已停止。"));
    const frame: DesktopInputFrame = { ...this.#binding, sequence: ++this.#sequence, input };
    const text = JSON.stringify(frame);
    const bytes = new TextEncoder().encode(text).length;
    if (bytes > 4096 || this.#bytes + bytes > 65_536 || this.channel.bufferedAmount + bytes > 65_536) {
      this.stop(messages.input_stalled!);
      return Promise.reject(new Error(messages.input_stalled));
    }
    return new Promise<void>((resolve, reject) => {
      this.#pending.set(frame.sequence, { at: performance.now(), bytes, resolve, reject });
      this.#bytes += bytes;
      try { this.channel.send(text); } catch { this.stop(messages.input_stalled!); }
    });
  }

  async text(text: string): Promise<void> {
    if (new TextEncoder().encode(text).length > 16_384 || text.includes("\0") || !text) throw new Error("文本须为 1 至 16384 字节，且不能包含空字符。");
    const binding = this.#binding;
    const send = async (input: DesktopInput) => {
      if (!binding || this.#binding !== binding) throw new Error("控制已变化，文本未继续发送。");
      await this.send(input);
    };
    await send({ type: "release" });
    await send({ type: "text_start" });
    let chunk = "";
    // 包含 JSON 转义后仍低于单帧 4 KiB；不拆 Unicode 码点。
    for (const point of text) {
      if (new TextEncoder().encode(JSON.stringify(chunk + point)).length > 2048) { await send({ type: "text_chunk", text: chunk }); chunk = ""; }
      chunk += point;
    }
    if (chunk) await send({ type: "text_chunk", text: chunk });
    await send({ type: "text_end" });
  }

  stop(message: string | null = null): void {
    if (this.#closed) return;
    const active = this.#binding !== null || this.#requesting;
    // WS 控制面独立于拥塞的输入 DataChannel。
    if (active) this.command({ type: "control_stop" });
    this.#clear();
    if (active || message) this.receive({ type: "control_stopped", message });
  }

  #clear(): void {
    this.#binding = null;
    this.#requesting = false;
    clearTimeout(this.#requestTimer);
    for (const item of this.#pending.values()) item.reject(new Error("操作结果未确认，未自动重发。"));
    this.#pending.clear();
    this.#bytes = 0;
  }

  close(): void {
    if (this.#closed) return;
    this.stop();
    this.#closed = true;
    clearInterval(this.#timer);
    this.channel.onmessage = null;
    this.channel.onclose = null;
    this.channel.onerror = null;
  }
}
