import { isTauri } from "@tauri-apps/api/core";
import { action, makeObservable, observable, observableRef, reaction, runInAction, type IReactionDisposer } from "mobx";
import { RootStore } from "../../stores/RootStore";
import { bootstrapWebRuntime, loginWeb, logoutWeb, takeWebLoginToken, WebLoginError } from "../../runtime-client/webLogin";
import { bootstrapRuntime, type RuntimeBootstrap } from "../../native-bridge/runtimeBootstrap";

import { activateRuntimeBinding, beginRuntimeConnection, clearRuntimeBinding, connectRuntimeTarget, openRuntimeWeb } from "../../native-bridge/runtimeConnection";

/** 客户端入口拥有连接选择；每次重新登录使用新的 UI 投影，旧请求只能影响已退出的 store。 */
export class ApplicationConnectionStore {
  readonly desktop = isTauri();
  phase: "loading" | "login" | "entry" | "workspace" = "loading";
  current: RootStore | null = null;
  error: string | null = null;
  pending = false;
  local_state: "starting" | "ready" | "failed" = "starting";
  local_error: string | null = null;
  target: "local" | "remote" = "local";
  connected_target: "local" | "remote" | null = null;
  address = "";
  progress = "";
  warning: string | null = null;
  web_pending = false;
  #entered = false;
  #local_start: Promise<RuntimeBootstrap> | null = null;
  #initial: RootStore | null;
  #initializing: Promise<void> | null = null;
  #generation = 0;
  #auth_reaction: IReactionDisposer | null = null;

  constructor(initial: RootStore) {
    this.#initial = initial;
    makeObservable(this, {
      phase: observable, current: observableRef, error: observable, pending: observable,
      clearError: action, dispose: action, local_state: observable, local_error: observable, target: observable, connected_target: observable, address: observable, progress: observable, warning: observable, web_pending: observable, chooseTarget: action,
    });
  }

  initialize(): Promise<void> {
    this.#initializing ??= this.#initialize();
    return this.#initializing;
  }

  clearError(): void { this.error = null; }

  async #initialize(): Promise<void> {
    const generation = this.#generation;
    if (this.desktop) {
      runInAction(() => { this.current = this.#initial; this.phase = "entry"; });
      void this.startLocal().catch(() => undefined);
      return;
    }
    const token = takeWebLoginToken();
    try {
      if (token) await loginWeb({ method: "token", token });
      if (generation !== this.#generation) return;
      const bootstrap = await bootstrapWebRuntime();
      if (generation !== this.#generation) return;
      await this.#enterWeb(bootstrap);
    } catch (error) {
      if (generation !== this.#generation) return;
      runInAction(() => {
        this.phase = "login";
        this.error = error instanceof WebLoginError && !token ? null : displayError(error);
      });
    }
  }

  startLocal(): Promise<RuntimeBootstrap> {
    if (this.#local_start && this.local_state !== "failed") return this.#local_start;
    runInAction(() => {
      // 进入按钮也会展示启动失败；重试时同步清除同一次失败，保留其他目标的连接错误。
      if (this.target === "local" && this.error === this.local_error) this.error = null;
      this.local_state = "starting";
      this.local_error = null;
    });
    this.#local_start = bootstrapRuntime().then((bootstrap) => {
      runInAction(() => { this.local_state = "ready"; });
      return bootstrap;
    }, (error: unknown) => {
      runInAction(() => { this.local_state = "failed"; this.local_error = displayError(error); });
      throw error;
    });
    return this.#local_start;
  }

  chooseTarget(target: "local" | "remote"): void {
    if (target === this.target) return;
    this.target = target;
    this.error = null;
    if (this.pending) {
      this.#generation += 1;
      this.current?.dispose();
      this.current = new RootStore({ target_kind: target, restore_resources: false });
      if (this.#entered) this.current.settings.open("runtime_connection");
      clearRuntimeBinding();
      void beginRuntimeConnection().catch(() => undefined);
      this.pending = false;
      this.progress = "";
    }
  }

  async connectDesktop(origin: string | null, password: string, remember: boolean): Promise<void> {
    if (this.pending) return;
    const switching = this.#entered;
    const generation = ++this.#generation;
    const target = origin === null ? "local" : "remote";
    this.#auth_reaction?.();
    this.#auth_reaction = null;
    clearRuntimeBinding();
    this.current?.dispose();
    this.#initial = null;
    const store = new RootStore({ target_kind: target, target_address: origin ?? undefined, restore_resources: true });
    runInAction(() => {
      this.current = store; this.pending = true; this.error = null; this.warning = null;
      this.target = target; this.address = origin ?? ""; this.progress = "正在准备连接…";
      if (this.#entered) { this.phase = "workspace"; store.settings.open("runtime_connection"); }
    });
    try {
      const binding = await beginRuntimeConnection();
      if (generation !== this.#generation) return;
      if (target === "local") {
        runInAction(() => { this.progress = "等待本机 Runtime…"; });
        await this.startLocal();
        if (generation !== this.#generation) return;
      }
      runInAction(() => { this.progress = target === "local" ? "正在连接本机 Runtime…" : "正在验证 Host 与密码…"; });
      const result = await connectRuntimeTarget(binding, origin, password, remember);
      if (generation !== this.#generation) return;
      activateRuntimeBinding(result.bootstrap);
      runInAction(() => { this.progress = "正在载入工作空间…"; });
      await store.connect(result.bootstrap);
      if (generation !== this.#generation) return;
      if (store.connection.state !== "connected") throw new Error(store.connection.error_message ?? "无法载入工作空间。");
      this.#entered = true;
      this.#auth_reaction = reaction(() => store.connection.last_error_code, (code) => {
        if (code !== "authentication_required" || generation !== this.#generation) return;
        this.#generation += 1;
        this.#auth_reaction?.(); this.#auth_reaction = null;
        store.dispose(); clearRuntimeBinding();
        void beginRuntimeConnection().catch(() => undefined);
        const empty = new RootStore({ target_kind: target, restore_resources: false });
        empty.settings.open("runtime_connection");
        runInAction(() => { this.current = empty; this.error = "登录已失效，请重新输入密码连接。"; });
      });
      runInAction(() => {
        this.phase = "workspace"; this.connected_target = target; this.warning = result.warning;
        if (result.warning) store.settings.showNotice(result.warning);
        if (switching) store.settings.open("runtime");
        else store.settings.close();
      });
    } catch (error) {
      if (generation !== this.#generation) return;
      store.dispose(); clearRuntimeBinding();
      const empty = new RootStore({ target_kind: target, restore_resources: false });
      if (this.#entered) empty.settings.open("runtime_connection");
      runInAction(() => { this.current = empty; empty.connection.markDisconnected(displayError(error), "authentication_required"); });
      runInAction(() => { this.error = displayError(error); });
    } finally {
      if (generation === this.#generation) runInAction(() => { this.pending = false; this.progress = ""; });
    }
  }

  async openWeb(): Promise<void> {
    if (this.web_pending) return;
    runInAction(() => { this.web_pending = true; });
    try { await openRuntimeWeb(); }
    catch (error) { runInAction(() => { this.error = displayError(error); }); this.current?.settings.open("runtime_connection"); }
    finally { runInAction(() => { this.web_pending = false; }); }
  }

  async acceptTokenLink(): Promise<void> {
    if (this.desktop) return;
    const token = takeWebLoginToken();
    if (token === null) return;
    // 本地退出同步清除旧投影；在任何 await 前冻结本次 token 的连接代次。
    void this.signOut(false);
    const generation = this.#generation;
    runInAction(() => { this.phase = "loading"; });
    try {
      await loginWeb({ method: "token", token });
      if (generation !== this.#generation) return;
      const bootstrap = await bootstrapWebRuntime();
      if (generation !== this.#generation) return;
      await this.#enterWeb(bootstrap);
    } catch (error) {
      if (generation !== this.#generation) return;
      runInAction(() => { this.phase = "login"; this.error = displayError(error); });
    }
  }

  async login(password: string): Promise<void> {
    if (this.pending) return;
    const generation = ++this.#generation;
    runInAction(() => { this.pending = true; this.error = null; });
    try {
      await loginWeb({ method: "password", password, native: false });
      if (generation !== this.#generation) return;
      const bootstrap = await bootstrapWebRuntime();
      if (generation !== this.#generation) return;
      await this.#enterWeb(bootstrap);
    } catch (error) {
      if (generation === this.#generation) runInAction(() => { this.error = displayError(error); });
    } finally {
      if (generation === this.#generation) runInAction(() => { this.pending = false; });
    }
  }

  async signOut(notify_host = true): Promise<void> {
    this.#generation += 1;
    const generation = this.#generation;
    this.#auth_reaction?.();
    this.#auth_reaction = null;
    this.current?.dispose();
    runInAction(() => { this.current = null; this.phase = "login"; this.error = null; this.pending = notify_host; });
    if (notify_host) {
      try { await logoutWeb(); }
      catch {
        if (generation === this.#generation) runInAction(() => { this.error = "已断开连接，Host 退出请求未完成。"; });
      } finally {
        if (generation === this.#generation) runInAction(() => { this.pending = false; });
      }
    }
  }

  dispose(): void {
    this.#generation += 1;
    this.#auth_reaction?.();
    this.#auth_reaction = null;
    this.current?.dispose();
    this.#initial?.dispose();
  }

  async #enterWeb(bootstrap: RuntimeBootstrap): Promise<void> {
    const generation = this.#generation;
    const store = this.#takeStore();
    this.#auth_reaction?.();
    this.#auth_reaction = reaction(() => store.connection.last_error_code, (code) => {
      if (code === "authentication_required" && generation === this.#generation) void this.signOut(false);
    });
    runInAction(() => { this.current = store; this.phase = "loading"; });
    await store.connect(bootstrap);
    if (generation !== this.#generation) { store.dispose(); return; }
    if (store.connection.state !== "connected") {
      store.dispose();
      runInAction(() => { this.current = null; this.phase = "login"; this.error = store.connection.error_message ?? "无法载入工作区。"; });
      throw new Error(store.connection.error_message ?? "无法载入工作区。");
    }
    runInAction(() => { this.phase = "workspace"; });
  }

  #takeStore(): RootStore {
    const store = this.#initial ?? new RootStore();
    this.#initial = null;
    return store;
  }
}

function displayError(error: unknown): string {
  if (typeof error === "object" && error !== null && "message" in error && typeof error.message === "string") return error.message;
  return "无法连接 Host。";
}
