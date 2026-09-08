import type { UserTerminalControl, UserTerminalNotice, UserTerminalSize, UserTerminalSource } from "../generated/assistant-protocol";

export type TerminalSource = UserTerminalSource;
export type TerminalSize = UserTerminalSize;
export type TerminalEvent = { type: "output"; bytes: Uint8Array } | Extract<UserTerminalNotice, { type: "exited" | "error" }>;
type CreatedTerminal = Extract<UserTerminalNotice, { type: "created" }>;

/** 当前 RuntimeClient 的单个终端连接；没有自动重连、跨连接 ID 操作或全 Host 清理。 */
export class TerminalSocket {
  readonly created: Promise<CreatedTerminal>;
  readonly #socket: WebSocket;
  readonly #closed: Promise<void>;
  #resolveCreated!: (value: CreatedTerminal) => void;
  #rejectCreated!: (error: Error) => void;
  #resolveClosed!: () => void;
  #created = false;
  #closing = false;
  #ended = false;
  #server_error: Error | null = null;
  #cleaned = false;
  #input: { resolve: () => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> } | null = null;
  #timer: ReturnType<typeof setTimeout>;

  constructor(address: string, bearer: string, source: TerminalSource, size: TerminalSize,
    receive: (event: TerminalEvent) => void, signal: AbortSignal) {
    const url = new URL("/user-terminals/socket", address);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    this.created = new Promise((resolve, reject) => { this.#resolveCreated = resolve; this.#rejectCreated = reject; });
    this.#closed = new Promise((resolve) => { this.#resolveClosed = resolve; });
    this.#socket = new WebSocket(url);
    this.#socket.binaryType = "arraybuffer";
    const cancel = () => this.disconnect();
    signal.addEventListener("abort", cancel, { once: true });
    this.#timer = setTimeout(() => { this.#rejectCreated(new Error("终端连接超时。")); this.disconnect(); }, 15_000);
    this.#socket.onopen = () => {
      if (this.#closing) { this.disconnect(); return; }
      this.#send({ type: "open", bearer: bearer || null, source, size });
    };
    this.#socket.onmessage = ({ data }: MessageEvent<unknown>) => {
      if (data instanceof ArrayBuffer) {
        if (this.#created && !this.#closing) receive({ type: "output", bytes: new Uint8Array(data) });
        return;
      }
      try {
        if (typeof data !== "string" || data.length > 8192) throw new Error();
        const notice = JSON.parse(data) as UserTerminalNotice;
        switch (notice.type) {
          case "created":
            if (this.#created) throw new Error();
            this.#created = true;
            clearTimeout(this.#timer);
            this.#resolveCreated(notice);
            break;
          case "input_ack": this.#finishInput(); break;
          case "error":
            if (this.#created) this.#server_error = new Error(notice.message);
            this.#rejectCreated(new Error(notice.message));
            if (!this.#closing) receive(notice);
            break;
          case "exited": this.#ended = true; if (!this.#closing) receive(notice); break;
          case "closed": this.#cleaned = true; this.#ended = true; this.#resolveClosed(); this.#socket.close(); break;
          default: throw new Error();
        }
      } catch {
        this.#rejectCreated(new Error("终端消息无效。"));
        if (!this.#closing) receive({ type: "error", message: "终端消息无效，连接已关闭。" });
        this.disconnect();
      }
    };
    this.#socket.onerror = () => this.#rejectCreated(new Error("无法连接 Host 终端。"));
    this.#socket.onclose = () => {
      clearTimeout(this.#timer);
      signal.removeEventListener("abort", cancel);
      this.#rejectCreated(new Error("终端连接已关闭。"));
      this.#finishInput(new Error("终端连接已关闭。"));
      if (this.#created && !this.#closing && !this.#ended) receive({ type: "error", message: "终端已断开，请重新创建。" });
      this.#resolveClosed();
    };
    if (signal.aborted) cancel();
  }

  #send(control: UserTerminalControl): void {
    if (this.#socket.readyState !== WebSocket.OPEN) throw new Error("终端连接已关闭。");
    this.#socket.send(JSON.stringify(control));
  }
  acknowledge(): void { if (!this.#closing) this.#send({ type: "ack" }); }
  resize(size: TerminalSize): void { this.#send({ type: "resize", size }); }
  write(bytes: Uint8Array): Promise<void> {
    if (this.#input || !bytes.length || bytes.length > 16384 || this.#closing || this.#socket.readyState !== WebSocket.OPEN)
      return Promise.reject(new Error("终端暂时不能接收输入。"));
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.#finishInput(new Error("终端输入超时。")); this.disconnect(); }, 30_000);
      this.#input = { resolve, reject, timer };
      this.#socket.send(bytes.slice().buffer);
    });
  }
  #finishInput(error?: Error): void {
    if (!this.#input) return;
    clearTimeout(this.#input.timer);
    if (error) this.#input.reject(error); else this.#input.resolve();
    this.#input = null;
  }
  /** 页面离开/连接切换不能等待异步渲染或 xterm，立即关闭本 socket。 */
  disconnect(): void {
    this.#closing = true;
    this.#socket.close();
  }
  async close(): Promise<void> {
    if (!this.#closing && this.#socket.readyState === WebSocket.OPEN) this.#send({ type: "close" });
    this.#closing = true;
    const timer = setTimeout(() => this.disconnect(), 5000);
    try {
      await this.#closed;
      if (!this.#cleaned && this.#server_error) throw this.#server_error;
    } finally { clearTimeout(timer); }
  }
}
