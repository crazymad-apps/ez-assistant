import { isTauri } from "@tauri-apps/api/core";
import { action, makeObservable, observable, observableRef, reaction, runInAction, type IReactionDisposer } from "mobx";
import type { HostLoginResult, HostMode } from "@ez-assistant/protocol";
import { RootStore } from "../../stores/RootStore";
import { RuntimeClient } from "../../runtime-client/RuntimeClient";
import { bootstrapWebRuntime, discoverWebHost, loginWeb, takeWebLoginToken, WebLoginError } from "../../runtime-client/webLogin";
import { bootstrapRuntime, upgradeRuntime, type RuntimeBootstrap } from "../../native-bridge/runtimeBootstrap";
import { activateRuntimeBinding, beginRuntimeConnection, clearRuntimeBinding, connectRuntimeTarget, openRuntimeWeb, probeRuntimeTarget, restoreRuntimeConnection } from "../../native-bridge/runtimeConnection";

/** 入口只管理身份和连接。身份改变时重建 Store，业务代码始终只看到本人的 Runtime。 */
export class ApplicationConnectionStore {
  readonly desktop = isTauri();
  phase: "loading" | "login" | "entry" | "workspace" = "loading";
  current: RootStore | null = null;
  session: HostLoginResult | null = null;
  mode: HostMode = "personal";
  error: string | null = null;
  pending = false;
  local_state: "starting" | "ready" | "failed" | "upgrade_required" = "starting";
  local_error: string | null = null;
  target: "local" | "remote" = "local";
  connected_target: "local" | "remote" | null = null;
  address = "";
  progress = "";
  warning: string | null = null;
  web_pending = false;
  logout_confirmation = false;
  password_open = false;
  #entered = false;
  #bootstrap: RuntimeBootstrap | null = null;
  #local_start: Promise<RuntimeBootstrap> | null = null;
  #initial: RootStore | null;
  #initializing: Promise<void> | null = null;
  #generation = 0;
  #discovery = 0;
  #checking = false;
  #auth_reaction: IReactionDisposer | null = null;
  #channel: BroadcastChannel | null = null;

  constructor(initial: RootStore) {
    this.#initial = initial;
    makeObservable(this, {
      phase: observable, current: observableRef, session: observableRef, mode: observable,
      error: observable, pending: observable, local_state: observable, local_error: observable,
      target: observable, connected_target: observable, address: observable, progress: observable,
      warning: observable, web_pending: observable, logout_confirmation: observable, password_open: observable,
      clearError: action, dispose: action, chooseTarget: action, requestLogout: action, cancelLogout: action, showPassword: action,
    });
    if (!this.desktop && typeof BroadcastChannel !== "undefined") {
      this.#channel = new BroadcastChannel("ez-assistant:login");
      // 通知中没有 Token、用户数据或账号；接收者始终向 Host 核验 Cookie。
      this.#channel.onmessage = () => { void this.recheckSession(); };
    }
  }

  get can_retry(): boolean { return this.#bootstrap !== null && this.phase !== "workspace"; }
  get account_label(): string { return this.session?.identity?.display_name || this.session?.identity?.username || "个人用户"; }
  get host_label(): string { return this.#bootstrap?.base_url ?? this.address; }
  clearError(): void { this.error = null; }
  showPassword(open = true): void { this.password_open = open; }
  requestLogout(): void { this.logout_confirmation = true; this.password_open = false; this.error = null; }
  cancelLogout(): void { if (!this.pending) this.logout_confirmation = false; }

  initialize(): Promise<void> { return this.#initializing ??= this.#initialize(); }

  async #initialize(): Promise<void> {
    const generation = this.#generation;
    if (this.desktop) {
      runInAction(() => { this.current = this.#initial; this.phase = "entry"; });
      void this.startLocal().catch(() => undefined);
      try {
        const saved = await restoreRuntimeConnection();
        if (saved && generation === this.#generation) await this.#enter(saved, generation);
      } catch (error) { if (generation === this.#generation) runInAction(() => { this.error = displayError(error); }); }
      return;
    }
    const token = takeWebLoginToken();
    try {
      const capabilities = await discoverWebHost();
      if (generation !== this.#generation) return;
      runInAction(() => { this.mode = capabilities.mode; });
      if (token) { await loginWeb({ method: "token", token }); this.#channel?.postMessage("changed"); }
      const bootstrap = await bootstrapWebRuntime();
      if (generation === this.#generation) await this.#enter(bootstrap, generation);
    } catch (error) {
      if (generation !== this.#generation) return;
      runInAction(() => {
        this.phase = "login";
        this.error = error instanceof WebLoginError && error.code === "authentication_required" && !token ? null : displayError(error);
      });
    }
  }

  startLocal(refresh = false): Promise<RuntimeBootstrap> {
    if (this.#local_start && this.local_state === "starting") return this.#local_start;
    if (refresh) this.#local_start = null;
    if (this.#local_start && this.local_state === "ready") return this.#local_start;
    return this.#startLocal(bootstrapRuntime);
  }

  /** 入口已经说明重启会中断任务；只在用户点击更新后调用原生交接。 */
  upgradeLocal(): Promise<RuntimeBootstrap> {
    if (this.#local_start && this.local_state === "starting") return this.#local_start;
    return this.#startLocal(upgradeRuntime);
  }

  #startLocal(start: () => Promise<RuntimeBootstrap>): Promise<RuntimeBootstrap> {
    runInAction(() => { if (this.error === this.local_error) this.error = null; this.local_state = "starting"; this.local_error = null; });
    this.#local_start = start().then((bootstrap) => {
      runInAction(() => { this.local_state = "ready"; if (this.target === "local") this.mode = bootstrap.capabilities.mode; });
      return bootstrap;
    }, (error: unknown) => {
      runInAction(() => {
        this.local_state = typeof error === "object" && error !== null && "code" in error && error.code === "runtime_upgrade_required" ? "upgrade_required" : "failed";
        this.local_error = displayError(error);
      });
      throw error;
    });
    return this.#local_start;
  }

  chooseTarget(target: "local" | "remote"): void {
    if (target === this.target) return;
    this.#discovery += 1;
    this.target = target;
    this.error = null;
    if (target === "local") void this.startLocal().then((value) => { if (this.target === "local") runInAction(() => { this.mode = value.capabilities.mode; }); }).catch(() => undefined);
    if (this.pending) {
      this.#clear();
      void beginRuntimeConnection().catch(() => undefined);
      this.phase = "entry";
    }
  }

  async discoverTarget(origin: string): Promise<void> {
    const discovery = ++this.#discovery;
    try {
      const capabilities = await probeRuntimeTarget(origin);
      if (discovery === this.#discovery) runInAction(() => { this.mode = capabilities.mode; this.error = null; });
    } catch (error) { if (discovery === this.#discovery) runInAction(() => { this.error = displayError(error); }); }
  }

  async connectDesktop(origin: string | null, password: string, remember: boolean, username = ""): Promise<void> {
    if (this.pending) return;
    this.#clear();
    const generation = this.#generation;
    runInAction(() => {
      this.pending = true; this.error = null; this.target = origin === null ? "local" : "remote";
      this.address = origin ?? ""; this.progress = "正在验证 Host 与账号…"; this.phase = "entry";
    });
    try {
      const binding = await beginRuntimeConnection();
      if (origin === null) await this.startLocal(true);
      if (generation !== this.#generation) return;
      const result = await connectRuntimeTarget(binding, origin, password, remember, username);
      if (generation !== this.#generation) return;
      runInAction(() => { this.warning = result.warning; });
      await this.#enter(result.bootstrap, generation);
    } catch (error) { if (generation === this.#generation) runInAction(() => { this.error = displayError(error); }); }
    finally { if (generation === this.#generation) runInAction(() => { this.pending = false; }); }
  }

  async login(password: string, username = ""): Promise<void> {
    if (this.pending) return;
    const generation = ++this.#generation;
    runInAction(() => { this.pending = true; this.error = null; });
    try {
      await loginWeb(this.mode === "enterprise" ? { method: "enterprise", username, password, native: false } : { method: "password", password, native: false });
      this.#channel?.postMessage("changed");
      if (generation !== this.#generation) return;
      const bootstrap = await bootstrapWebRuntime();
      if (generation === this.#generation) await this.#enter(bootstrap, generation);
    } catch (error) { if (generation === this.#generation) runInAction(() => { this.error = displayError(error); }); }
    finally { if (generation === this.#generation) runInAction(() => { this.pending = false; }); }
  }

  async acceptTokenLink(): Promise<void> {
    if (this.desktop) return;
    const token = takeWebLoginToken();
    if (token === null) return;
    this.#clear();
    const generation = this.#generation;
    runInAction(() => { this.phase = "loading"; });
    try {
      await loginWeb({ method: "token", token });
      this.#channel?.postMessage("changed");
      if (generation !== this.#generation) return;
      const bootstrap = await bootstrapWebRuntime();
      if (generation === this.#generation) await this.#enter(bootstrap, generation);
    } catch (error) { if (generation === this.#generation) runInAction(() => { this.phase = "login"; this.error = displayError(error); }); }
  }

  /** 焦点恢复和同源通知都重新核验；不依据通知内容切换账号，也不重叠发出核验。 */
  async recheckSession(): Promise<void> {
    if (this.desktop || this.pending || this.#checking) return;
    this.#checking = true;
    const generation = this.#generation;
    try {
      const bootstrap = await bootstrapWebRuntime();
      if (generation !== this.#generation) return;
      // 同一身份的焦点/广播核验不能重启初始化；失败后只由“重试初始化”触发。
      if (this.#bootstrap?.login_context === bootstrap.login_context) return;
      this.#clear();
      await this.#enter(bootstrap, this.#generation);
    } catch (error) {
      if (generation !== this.#generation) return;
      this.#clear();
      runInAction(() => { this.phase = "login"; this.error = displayError(error); });
    } finally { this.#checking = false; }
  }

  async retryInitialization(): Promise<void> {
    if (!this.#bootstrap || this.pending) return;
    const bootstrap = this.#bootstrap;
    runInAction(() => { this.pending = true; this.error = null; });
    try { await this.#enter(bootstrap, this.#generation); }
    finally { runInAction(() => { this.pending = false; }); }
  }

  async changePassword(old_password: string, new_password: string): Promise<void> {
    if (!this.#bootstrap) throw new Error("请先登录。");
    const client = new RuntimeClient(this.#bootstrap);
    try { await client.changePassword({ old_password, new_password }); }
    finally { client.dispose(); }
  }

  /** 只能由已确认的对话框调用；失败保留当前身份，避免声称退出成功。 */
  async signOut(notify_host = true): Promise<void> {
    if (!notify_host) { this.#clear(); runInAction(() => { this.phase = this.desktop ? "entry" : "login"; }); return; }
    if (notify_host && !this.logout_confirmation) { this.requestLogout(); return; }
    if (this.pending) return;
    const generation = this.#generation;
    const bootstrap = this.#bootstrap;
    runInAction(() => { this.pending = true; this.error = null; });
    try {
      let warning: string | null = null;
      if (notify_host && bootstrap) {
        const client = new RuntimeClient(bootstrap);
        try {
          const result = await client.logout();
          if (result && !result.center_revocation_confirmed) warning = "本 Host 已退出并中断任务；中心凭据撤销未确认。";
        } finally { client.dispose(); }
      }
      if (generation !== this.#generation) return;
      this.#clear();
      if (this.desktop) await beginRuntimeConnection();
      else if (notify_host) this.#channel?.postMessage("changed");
      runInAction(() => { this.phase = this.desktop ? "entry" : "login"; this.warning = warning; });
    } catch (error) { if (generation === this.#generation) runInAction(() => { this.error = displayError(error); }); }
    finally { runInAction(() => { this.pending = false; }); }
  }

  async openWeb(): Promise<void> {
    if (this.web_pending) return;
    runInAction(() => { this.web_pending = true; });
    try { await openRuntimeWeb(); }
    catch (error) { runInAction(() => { this.error = displayError(error); }); }
    finally { runInAction(() => { this.web_pending = false; }); }
  }

  dispose(): void { this.#clear(); this.current?.dispose(); this.current = null; this.#channel?.close(); this.#channel = null; }

  #clear(): void {
    this.#generation += 1;
    this.#auth_reaction?.(); this.#auth_reaction = null;
    this.current?.dispose(); this.#initial?.dispose(); this.#initial = null;
    this.#bootstrap = null;
    clearRuntimeBinding();
    runInAction(() => { this.current = this.desktop ? new RootStore({ target_kind: this.target, restore_resources: false }) : null; this.session = null; this.pending = false; this.logout_confirmation = false; this.password_open = false; });
  }

  #authenticationRequired(): void {
    this.#clear();
    if (this.desktop) {
      void beginRuntimeConnection().catch(() => undefined);
      void this.startLocal(true).catch(() => undefined);
    }
    runInAction(() => { this.phase = this.desktop ? "entry" : "login"; this.error = "登录已失效，请重新登录。"; });
  }

  async #enter(bootstrap: RuntimeBootstrap, generation: number): Promise<void> {
    // 登录成功和初始化成功是两个阶段；初始化失败仍保留凭据供显式重试、改密和退出。
    this.#bootstrap = bootstrap;
    this.#auth_reaction?.();
    if (this.desktop) activateRuntimeBinding(bootstrap);
    runInAction(() => { this.phase = this.desktop ? "entry" : "loading"; this.progress = "正在初始化用户工作空间…"; this.mode = bootstrap.capabilities.mode; });
    const identity_client = new RuntimeClient(bootstrap);
    let session: HostLoginResult;
    try { session = bootstrap.session ?? await identity_client.session(); }
    catch (error) {
      if (generation === this.#generation) {
        // 原生刷新恢复的 Token 也可能已过期，应回到登录，不能显示初始化重试。
        if (typeof error === "object" && error !== null && "code" in error && error.code === "authentication_required") this.#authenticationRequired();
        else runInAction(() => { this.error = displayError(error); });
      }
      return;
    }
    finally { identity_client.dispose(); }
    if (generation !== this.#generation) return;
    runInAction(() => { this.session = session; });
    const user = session.identity;
    const namespace = user ? `${bootstrap.base_url}/users/${user.center_id}/${user.user_id}` : bootstrap.target_kind === "remote" ? bootstrap.base_url : undefined;
    const store = this.#initial && !user ? this.#initial : new RootStore({ target_kind: bootstrap.target_kind, target_address: namespace, restore_resources: this.desktop });
    // 原生刷新恢复个人连接时可以复用尚未连接的初始 Store，不能先销毁再连接。
    if (this.current !== store) this.current?.dispose();
    if (this.#initial !== store && this.#initial !== this.current) this.#initial?.dispose();
    this.#initial = null;
    this.#auth_reaction = reaction(() => [store.connection.state, store.connection.last_error_code] as const, ([state, code]) => {
      if (generation !== this.#generation || (this.logout_confirmation && this.pending)) return;
      if (code === "login_context_changed") { this.#clear(); runInAction(() => { this.phase = "loading"; }); void this.recheckSession(); }
      else if (code === "authentication_required") this.#authenticationRequired();
      else if (state !== "connected" && this.phase === "workspace") {
        runInAction(() => { this.phase = this.desktop ? "entry" : "loading"; this.error = store.connection.error_message ?? "连接已中断，请重试。"; });
      } else if (state === "connected") { runInAction(() => { this.phase = "workspace"; this.error = null; }); }
    });
    runInAction(() => { this.current = store; });
    await store.connect(bootstrap);
    if (generation !== this.#generation) { store.dispose(); return; }
    runInAction(() => {
      if (store.connection.state === "connected") {
        if (this.desktop) { if (this.#entered) store.settings.open("runtime"); else store.settings.close(); }
        this.#entered = true; this.phase = "workspace"; this.connected_target = bootstrap.target_kind ?? null; this.error = null;
      }
      else { this.error = store.connection.error_message ?? "初始化失败，请重试。"; }
    });
  }
}

function displayError(error: unknown): string {
  return typeof error === "object" && error !== null && "message" in error && typeof error.message === "string" ? error.message : "无法连接 Host。";
}
