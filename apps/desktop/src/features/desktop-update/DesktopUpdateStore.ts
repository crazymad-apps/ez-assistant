import type { UnlistenFn } from "@tauri-apps/api/event";
import { readDesktopUpdate, listenDesktopUpdate, startDesktopUpdate, cancelDesktopUpdate, type DesktopUpdateSnapshot } from "../../native-bridge/desktopUpdate";
import { action, computed, makeObservable, observable, observableRef, runInAction } from "mobx";


const empty: DesktopUpdateSnapshot = { revision: -1, phase: "idle", available: false, required: false, notes: "", received: 0, total: 0, error: null, resume_remote: null, stops_local_host: false, binding_id: null, host_mode: null };

/** 仅投影原生快照，面板开关不会创建或取消更新任务。 */
export class DesktopUpdateStore {
  snapshot = empty;
  open = false;
  command_error: string | null = null;
  #initializing: Promise<void> | null = null;
  #unlisten: UnlistenFn | null = null;
  #disposed = false;

  constructor(readonly desktop: boolean) {
    makeObservable(this, {
      snapshot: observableRef, open: observable, command_error: observable,
      visible: computed, installing: computed, downloading: computed,
      show: action, hide: action, receive: action,
    });
  }

  get visible(): boolean { return this.desktop && (this.snapshot.available || this.downloading || this.installing || this.snapshot.phase === "failed"); }
  get downloading(): boolean { return this.snapshot.phase === "downloading"; }
  get installing(): boolean { return this.snapshot.phase === "installing"; }
  show(): void { this.open = true; this.command_error = null; }
  hide(): void { if (!this.installing) this.open = false; }

  receive(snapshot: DesktopUpdateSnapshot): void {
    if (this.#disposed || snapshot.revision <= this.snapshot.revision) return;
    this.snapshot = snapshot;
  }

  initialize(): Promise<void> {
    if (!this.desktop) return Promise.resolve();
    return this.#initializing ??= this.#initialize();
  }

  async #initialize(): Promise<void> {
    try {
      const unlisten = await listenDesktopUpdate((snapshot) => this.receive(snapshot));
      if (this.#disposed) { unlisten(); return; }
      this.#unlisten = unlisten;
      this.receive(await readDesktopUpdate());
    } catch { /* 未配置更新或原生连接失败不影响既有登录。 */ }
  }

  async start(): Promise<void> { await this.#command(startDesktopUpdate); }
  async cancel(): Promise<void> { await this.#command(cancelDesktopUpdate); }

  async #command(command: () => Promise<void>): Promise<void> {
    runInAction(() => { this.command_error = null; });
    try { await command(); }
    catch (error) {
      runInAction(() => { this.command_error = typeof error === "object" && error !== null && "message" in error && typeof error.message === "string" ? error.message : "暂时无法更新，请稍后重试。"; });
    }
  }

  dispose(): void { this.#disposed = true; this.#unlisten?.(); this.#unlisten = null; }
}
