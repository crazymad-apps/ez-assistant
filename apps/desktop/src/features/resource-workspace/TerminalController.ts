import { action, computed, makeObservable, observable, runInAction } from "mobx";
import {
  acknowledgeUserTerminal, closeUserTerminal, createUserTerminal, resizeUserTerminal,
  restartUserTerminal, writeUserTerminal, type TerminalEvent, type TerminalSize, type TerminalSource,
} from "../../native-bridge/userTerminal";
import { createTerminalEmulator, type TerminalEmulator } from "./terminalEmulator";

type TerminalStatus = "idle" | "starting" | "running" | "exited" | "error" | "closing" | "closed";
const INPUT_BLOCK = 16 * 1024;
const INPUT_QUEUE_LIMIT = 1024 * 1024;

/** 标签 owner：视图显隐只影响 fit/focus；后台仍写入 xterm 并确认每个输出块。 */
export class TerminalController {
  status: TerminalStatus = "starting";
  error: string | null = null;
  exit_code: number | null = null;
  native_id: string | null = null;
  ready = false;
  readonly source: TerminalSource;
  readonly title = "终端";
  #emulator: TerminalEmulator | null = null;
  #container: HTMLElement | null = null;
  #pending: Promise<void> = Promise.resolve();
  #closing: Promise<void> | null = null;
  #input: Uint8Array[] = [];
  #input_bytes = 0;
  #writing = false;
  #input_timer: ReturnType<typeof setTimeout> | undefined;
  #resize_timer: ReturnType<typeof setTimeout> | undefined;
  #resizing: Promise<void> = Promise.resolve();
  #last_size = "";
  readonly #on_exit: () => void;

  constructor(source: TerminalSource, onExit: () => void, deferred = false) {
    this.source = source;
    this.#on_exit = onExit;
    makeObservable(this, {
      status: observable, error: observable, exit_code: observable, native_id: observable,
      ready: observable, needs_close_confirmation: computed,
      start: action, restart: action, reportError: action,
    });
    this.status = deferred ? "idle" : "starting";
    if (!deferred) this.#pending = this.#start();
  }

  start(): void {
    if (this.status !== "idle") return;
    this.status = "starting";
    this.#pending = this.#start();
  }

  get needs_close_confirmation(): boolean { return this.status === "starting" || this.status === "running"; }

  mount(container: HTMLElement): void {
    this.#container = container;
    if (this.#emulator) container.append(this.#emulator.host);
    this.fit();
  }

  unmount(): void { this.#container = null; }
  focus(): void { this.#emulator?.terminal.focus(); }

  fit(): void {
    clearTimeout(this.#resize_timer);
    this.#resize_timer = setTimeout(() => {
      const emulator = this.#emulator;
      const rect = this.#container?.getBoundingClientRect();
      if (!emulator || !rect || rect.width < 40 || rect.height < 24) return;
      const size = emulator.fit.proposeDimensions();
      if (!size) return;
      const bounded = { cols: Math.min(1000, Math.max(2, size.cols)), rows: Math.min(500, Math.max(1, size.rows)) };
      emulator.terminal.resize(bounded.cols, bounded.rows);
      const key = `${bounded.cols}:${bounded.rows}`;
      if (this.status !== "running" || key === this.#last_size) return;
      this.#last_size = key;
      this.#resizing = this.#resizing.then(async () => {
        if (this.native_id && this.status === "running") await resizeUserTerminal(this.native_id, bounded);
      }).catch((failure: unknown) => { if (!this.#closing) this.reportError(failure); });
    }, 50);
  }

  restart(): void {
    if (this.#closing || (this.status !== "exited" && this.status !== "error")) return;
    this.status = "starting";
    this.error = null;
    this.exit_code = null;
    this.#last_size = "";
    this.#input = [];
    this.#input_bytes = 0;
    this.#emulator?.terminal.reset();
    this.#pending = this.#pending.then(() => this.#start());
  }

  close(): Promise<void> {
    if (this.#closing) return this.#closing;
    runInAction(() => { this.status = "closing"; this.error = null; });
    clearTimeout(this.#input_timer);
    this.#input_timer = undefined;
    clearTimeout(this.#resize_timer);
    this.#input = [];
    this.#input_bytes = 0;
    this.#closing = this.#pending.then(async () => {
      // 创建过程中关闭也必须接住原生句柄；只有回收成功才移除标签与模拟器。
      if (this.native_id) await closeUserTerminal(this.native_id);
      this.#emulator?.terminal.dispose();
      this.#emulator?.host.remove();
      this.#emulator = null;
      runInAction(() => { this.status = "closed"; this.native_id = null; this.ready = false; });
    }).catch((failure: unknown) => {
      this.#closing = null;
      this.reportError(failure);
      throw failure;
    });
    return this.#closing;
  }

  reportError(failure: unknown): void {
    this.error = terminalError(failure);
    this.status = "error";
  }

  async #start(): Promise<void> {
    try {
      if (!this.#emulator) {
        this.#emulator = await createTerminalEmulator();
        const terminal = this.#emulator.terminal;
        terminal.onData((data) => this.#enqueue(new TextEncoder().encode(data), [...data].some((char) => char.charCodeAt(0) < 32 || char.charCodeAt(0) === 127)));
        terminal.onBinary((data) => this.#enqueue(Uint8Array.from(data, (value) => value.charCodeAt(0)), true));
        // macOS 的 Cmd+C/Cmd+V 使用 xterm 自带 copy/paste；不映射为中断/普通字符。
        terminal.attachCustomKeyEventHandler((event) => !(event.metaKey && ["c", "v"].includes(event.key.toLowerCase())));
        if (this.#container) this.#container.append(this.#emulator.host);
        runInAction(() => { this.ready = true; });
      }
      if (this.status === "closing") return;
      const size = this.#size();
      if (this.native_id) await restartUserTerminal(this.native_id, size, (event) => this.#receive(event));
      else {
        const created = await createUserTerminal(this.source, size, (event) => this.#receive(event));
        runInAction(() => { this.native_id = created.terminal_id; });
      }
      runInAction(() => { if (this.status === "starting") this.status = "running"; });
      this.fit();
      void this.#flushInput();
    } catch (failure) { if (this.status !== "closing") this.reportError(failure); }
  }

  #receive(event: TerminalEvent): void {
    if (event.type === "output") {
      // write 的解析回调独立于可见区域渲染；不能在隐藏标签时暂停 ack。
      this.#emulator?.terminal.write(new Uint8Array(event.bytes), () => {
        void this.#pending.then(async () => {
          if (this.native_id && !this.#closing) await acknowledgeUserTerminal(this.native_id);
        }).catch((failure: unknown) => { if (!this.#closing) this.reportError(failure); });
      });
    } else if (!this.#closing) {
      runInAction(() => {
        if (event.type === "exited") { this.status = "exited"; this.exit_code = event.code; }
        else this.reportError(event.message);
      });
      // 只由原生 Shell 退出事实通知标签 owner；Ctrl+D 也可能只是前台程序的 EOF。
      if (event.type === "exited") this.#on_exit();
    }
  }

  #enqueue(bytes: Uint8Array, immediate: boolean): void {
    if ((this.status !== "running" && this.status !== "starting") || !bytes.length) return;
    if (this.#input_bytes + bytes.length > INPUT_QUEUE_LIMIT) {
      runInAction(() => { this.error = "终端输入仍在发送，本次粘贴过大，请分段粘贴。"; });
      return;
    }
    this.#input.push(bytes);
    this.#input_bytes += bytes.length;
    if (immediate) { clearTimeout(this.#input_timer); this.#input_timer = undefined; void this.#flushInput(); }
    else this.#input_timer ??= setTimeout(() => { this.#input_timer = undefined; void this.#flushInput(); }, 8);
  }

  async #flushInput(): Promise<void> {
    if (this.#writing) return;
    this.#writing = true;
    try {
      while (this.#input_bytes && this.status === "running" && this.native_id) {
        const block = new Uint8Array(Math.min(INPUT_BLOCK, this.#input_bytes));
        let offset = 0;
        while (offset < block.length) {
          const first = this.#input[0]!;
          const count = Math.min(first.length, block.length - offset);
          block.set(first.subarray(0, count), offset);
          offset += count;
          if (count === first.length) this.#input.shift(); else this.#input[0] = first.subarray(count);
        }
        this.#input_bytes -= block.length;
        await writeUserTerminal(this.native_id, block);
      }
    } catch (failure) { if (!this.#closing) this.reportError(failure); }
    finally { this.#writing = false; }
  }

  #size(): TerminalSize { return { cols: this.#emulator?.terminal.cols ?? 80, rows: this.#emulator?.terminal.rows ?? 24 }; }
}

function terminalError(failure: unknown): string {
  if (failure instanceof Error) return failure.message;
  if (failure && typeof failure === "object" && "message" in failure) return String(failure.message);
  return typeof failure === "string" ? failure : "终端操作失败。";
}
