import type { DesktopDisplay, DesktopInput } from "@ez-assistant/protocol";
import { action, computed, makeObservable, observable, observableRef, runInAction } from "mobx";
import type { RuntimeClient } from "../../runtime-client/RuntimeClient";
import type { DesktopSocket } from "../../runtime-client/DesktopSocket";

/** 本地标签的播放投影；授权、采集和连接所有权由 Host 管理。 */
export class DesktopController {
  displays: readonly DesktopDisplay[] = [];
  display_id: string | null = null;
  status: "connecting" | "viewing" | "disconnected" = "disconnected";
  stream: MediaStream | null = null;
  error: string | null = null;
  control: "readonly" | "requesting" | "controlled" = "readonly";
  input_focused = false;
  control_error: string | null = null;
  text_pending = false;
  #socket: DesktopSocket | null = null;
  #generation = 0;

  constructor(readonly device: string, readonly name: string, private readonly getClient: () => RuntimeClient | null, private readonly controlPermission: boolean | (() => boolean) = false, display: string | null = null, deferred = false) {
    this.display_id = display;
    makeObservable(this, { displays: observableRef, display_id: observable, can_control: computed, selectDisplay: action, status: observable, stream: observableRef, error: observable, connect: action, disconnect: action, control: observable, input_focused: observable, control_error: observable, text_pending: observable, startControl: action, stopControl: action, focusInput: action });
    if (!deferred) this.connect();
  }

  get can_control(): boolean { return typeof this.controlPermission === "function" ? this.controlPermission() : this.controlPermission; }

  connect(): void {
    this.disconnect();
    const generation = ++this.#generation;
    this.status = "connecting";
    this.error = null;
    try {
      const client = this.getClient();
      if (!client) throw new Error("本机 Runtime 尚未连接。");
      this.#socket = client.openDesktop(this.device, (event) => {
        if (generation !== this.#generation) return;
        runInAction(() => {
          if (event.type === "displays") { this.displays = event.displays; if (event.selected !== null) this.display_id = event.selected; }
          else if (event.type === "stream") this.stream = event.stream;
          else if (event.type === "ready") this.status = "viewing";
          else if (event.type === "control_ready") { this.control = "controlled"; this.control_error = null; }
          else if (event.type === "control_stopped") { this.control = "readonly"; this.input_focused = false; this.control_error = event.message; }
          else { this.disconnect(); this.error = event.message; }
        });
      }, this.display_id);
    } catch (error) {
      this.disconnect();
      this.error = error instanceof Error ? error.message : "桌面连接失败。";
    }
  }

  selectDisplay(display: string): void {
    if (display === this.display_id || !this.displays.some((item) => item.id === display)) return;
    this.display_id = display;
    this.connect();
  }

  startControl(): void {
    if (!this.can_control || this.status !== "viewing" || this.control !== "readonly") return;
    this.control = "requesting";
    this.control_error = null;
    this.#socket?.startControl();
  }

  stopControl(): void {
    this.control = "readonly";
    this.input_focused = false;
    this.#socket?.stopControl();
  }

  focusInput(focused: boolean): void {
    this.input_focused = focused && this.control === "controlled" && !this.text_pending;
    if (!this.input_focused && this.control === "controlled" && !this.text_pending) void this.#socket?.input({ type: "release" }).catch(() => {});
  }

  input(value: DesktopInput): void {
    if (this.control === "controlled" && this.input_focused && !this.text_pending) void this.#socket?.input(value).catch(() => {});
  }

  async sendText(value: string): Promise<void> {
    if (this.control !== "controlled" || this.text_pending || !this.#socket) throw new Error("请先开启控制。");
    const generation = this.#generation;
    const socket = this.#socket;
    runInAction(() => { this.text_pending = true; this.input_focused = false; });
    try { await socket.sendText(value); }
    finally { if (generation === this.#generation) runInAction(() => { this.text_pending = false; }); }
  }

  decoded(): void { this.#socket?.decoded(); }

  disconnect(): void {
    this.#generation += 1;
    this.control = "readonly";
    this.input_focused = false;
    this.text_pending = false;
    this.#socket?.disconnect();
    this.#socket = null;
    this.stream?.getTracks().forEach((track) => track.stop());
    this.stream = null;
    this.status = "disconnected";
  }
}
