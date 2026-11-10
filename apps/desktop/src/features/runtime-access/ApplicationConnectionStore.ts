import { startupMessage } from "../../runtime-client/startupStatus";
import { isTauri } from "@tauri-apps/api/core";
import { action, computed, makeObservable, observable, observableRef, reaction, runInAction, type IReactionDisposer } from "mobx";
import type { HostLoginResult, HostOrganization, HostOrganizationSelection, HostOrganizationSwitchRequest, HostOrganizationSwitchResult, HostMode } from "@ez-assistant/protocol";
import { RootStore } from "../../stores/RootStore";
import { RuntimeClient } from "../../runtime-client/RuntimeClient";
import { bootstrapWebRuntime, discoverWebHost, loginWeb, takeWebLoginToken, WebLoginError } from "../../runtime-client/webLogin";
import { bootstrapRuntime, upgradeRuntime, type RuntimeBootstrap } from "../../native-bridge/runtimeBootstrap";
import { activateRuntimeBinding, beginRuntimeConnection, clearRuntimeBinding, connectRuntimeTarget, openRuntimeWeb, probeRuntimeTarget, restoreRuntimeConnection, prepareDesktopUpdate, manageLocalHost, switchRuntimeOrganization } from "../../native-bridge/runtimeConnection";

import { restartNativeRuntime } from "../../native-bridge/desktopLifecycle";
import { centerAddressError, loginOptionsApplied, loginOptionsChanged, loginOptionsCommand, loginOptionsErrorCode, loginOptionsIntent, matchesLoginOptions, type LoginOptionsDraft, type LoginOptionsState } from "./loginOptions";

import { DesktopUpdateStore } from "../desktop-update";

type SwitchPreview = Extract<HostOrganizationSwitchResult, { state: "confirm" }>;
type OrganizationSwitchState =
  | { phase: "options"; organizations: HostOrganization[]; message?: string }
  | { phase: "loading" | "checking" }
  | { phase: "selection_error"; message: string }
  | { phase: "confirm"; preview: SwitchPreview }
  | { phase: "stopping" | "exchanging" | "verify" | "stop_failed" | "exchange_failed"; preview: SwitchPreview };

const SWITCH_POLL_MS = 400;
const PENDING_ORGANIZATION_SWITCH = "ez-assistant:organization-switch";

/** 入口只管理身份和连接。身份改变时重建 Store，业务代码始终只看到本人的 Runtime。 */
export class ApplicationConnectionStore {
  readonly desktop = isTauri();
  readonly updates = new DesktopUpdateStore(this.desktop);
  phase: "loading" | "login" | "entry" | "workspace" = "loading";
  current: RootStore | null = null;
  session: HostLoginResult | null = null;
  organization_selection: HostOrganizationSelection | null = null;
  organization_switch: OrganizationSwitchState | null = null;
  organization_switch_visible = false;
  #organization_target: { binding: string; origin: string | null } | null = null;
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
  host_update_required = false;
  login_options: LoginOptionsState | null = null;
  login_options_notice: string | null = null;
  credentials_epoch = 0;
  #login_options_owner = 0;
  #return_to_settings = false;
  #phase_reaction: IReactionDisposer;
  #bootstrap: RuntimeBootstrap | null = null;
  #local_start: Promise<RuntimeBootstrap> | null = null;
  #initial: RootStore | null;
  #initializing: Promise<void> | null = null;
  #generation = 0;
  #discovery = 0;
  #update_selection = 0;
  #checking = false;
  #auth_reaction: IReactionDisposer | null = null;
  #update_reaction: IReactionDisposer;
  #channel: BroadcastChannel | null = null;

  constructor(initial: RootStore, private readonly flush_application_preferences?: () => Promise<void>) {
    this.#initial = initial;
    makeObservable(this, {
      phase: observable, current: observableRef, session: observableRef, organization_selection: observableRef, mode: observable,
      organization_switch: observableRef, organization_switch_visible: observable, closeOrganizationSwitch: action,
      error: observable, pending: observable, local_state: observable, local_error: observable,
      target: observable, connected_target: observable, address: observable, progress: observable,
      warning: observable, web_pending: observable, logout_confirmation: observable, password_open: observable,
      login_options: observableRef, login_options_notice: observable, credentials_epoch: observable, login_options_busy: computed,
      closeLoginOptions: action, editLoginOptions: action, confirmLoginOptions: action,
      host_update_required: observable, update_required: computed,
      clearError: action, cancelOrganizationSelection: action, dispose: action, chooseTarget: action, requestLogout: action, cancelLogout: action, showPassword: action,
    });
    this.#phase_reaction = reaction(() => this.phase, (phase) => {
      if (phase !== "workspace") this.current?.settings.close();
      else this.closeLoginOptions();
    });
    this.#update_reaction = reaction(() => this.updates.snapshot.required, (required) => {
      if (required) {
        runInAction(() => { this.host_update_required = false; });
        this.#enterUpdateGate();
      }
    });
    if (!this.desktop && typeof BroadcastChannel !== "undefined") {
      this.#channel = new BroadcastChannel("ez-assistant:login");
      // 通知中没有 Token、用户数据或账号；接收者始终向 Host 核验 Cookie。
      this.#channel.onmessage = () => { void this.recheckSession(); };
    }
  }

  get login_options_busy(): boolean {
    return this.login_options !== null && ["saving", "checking_save", "restarting", "checking_applied"].includes(this.login_options.phase);
  }

  async openLoginOptions(): Promise<void> {
    if (!this.desktop || this.target !== "local" || this.phase === "workspace" || this.pending || this.login_options) return;
    const owner = ++this.#login_options_owner;
    const generation = this.#generation;
    runInAction(() => {
      this.login_options_notice = null;
      this.login_options = { phase: "loading", baseline: null, draft: { mode: "personal", center_url: "" }, intended: null,
        expected_instance: null, recovery: null, message: null, address_error: null };
    });
    try {
      await this.startLocal();
      if (!this.#ownsLoginOptions(owner, generation)) return;
      await this.reloadLoginOptions();
    } catch {
      if (this.#ownsLoginOptions(owner, generation)) this.#setLoginOptions({ phase: "issue", recovery: "reload", message: "无法读取登录选项，请检查本机 Runtime 状态。" });
    }
  }

  closeLoginOptions(): void {
    if (this.login_options_busy) return;
    this.#login_options_owner += 1;
    this.login_options = null;
  }

  editLoginOptions(draft: Partial<LoginOptionsDraft> = {}): void {
    const state = this.login_options;
    if (!state?.baseline || !["edit", "confirm"].includes(state.phase)) return;
    this.#setLoginOptions({ phase: "edit", draft: { ...state.draft, ...draft }, address_error: null, message: null, intended: null });
  }

  confirmLoginOptions(): void {
    const state = this.login_options;
    if (!state?.baseline || this.updates.downloading || this.updates.installing) return;
    if (!(state.phase === "edit" || (state.phase === "issue" && state.recovery === "restart"))) return;
    const address_error = centerAddressError(state.draft);
    if (address_error) { this.#setLoginOptions({ address_error }); return; }
    if (!loginOptionsChanged(state.baseline, state.draft) && !state.baseline.restart_required) return;
    this.#setLoginOptions({ phase: "confirm", message: null, intended: loginOptionsIntent(state.baseline, state.draft) });
  }

  async reloadLoginOptions(): Promise<void> {
    if (!this.login_options || this.login_options_busy) return;
    const owner = ++this.#login_options_owner, generation = this.#generation;
    this.#setLoginOptions({ phase: "loading", message: null });
    try {
      const status = await manageLocalHost({ type: "get_status" });
      if (!this.#ownsLoginOptions(owner, generation)) return;
      runInAction(() => { this.mode = status.current_mode; this.local_state = "ready"; });
      this.#setLoginOptions({ phase: "edit", baseline: status, draft: { mode: status.mode ?? "personal", center_url: status.center_url ?? "" },
        intended: null, expected_instance: status.instance_id, recovery: null, address_error: null });
    } catch {
      if (this.#ownsLoginOptions(owner, generation)) this.#setLoginOptions({ phase: "issue", recovery: "reload", message: "无法读取登录选项。重新载入将放弃未保存的修改。" });
    }
  }

  async applyLoginOptions(): Promise<void> {
    const state = this.login_options;
    if (state?.phase !== "confirm" || !state.baseline || !state.intended || this.pending || this.updates.downloading || this.updates.installing) return;
    const owner = this.#login_options_owner;
    let generation = this.#generation;
    const changed = loginOptionsChanged(state.baseline, state.draft);
    this.#setLoginOptions({ phase: changed ? "saving" : "restarting", message: null });
    runInAction(() => { this.pending = true; this.updates.operation_blocked = true; });
    try {
      let saved = state.baseline;
      if (changed) {
        try { saved = await manageLocalHost(loginOptionsCommand(state.baseline, state.draft), state.baseline.instance_id); }
        catch (error) {
          if (!this.#ownsLoginOptions(owner, generation)) return;
          const code = loginOptionsErrorCode(error);
          if (["configuration_conflict"].includes(code ?? "")) {
            this.#setLoginOptions({ phase: "issue", recovery: "reload", message: "配置已变化，请重新核对。重新载入将放弃未保存的修改。" });
          } else if (code === "desktop_update_busy") {
            this.#setLoginOptions({ phase: "edit", message: "更新正在下载或安装，请完成后再保存登录选项。" });
          } else if (["invalid_request", "operation_not_allowed"].includes(code ?? "")) {
            const message = typeof error === "object" && error !== null && "message" in error && typeof error.message === "string"
              ? error.message : "配置未保存，请检查输入和本机管理权限。";
            this.#setLoginOptions({ phase: "edit", message });
          } else {
            this.#setLoginOptions({ recovery: "check_save", expected_instance: state.baseline.instance_id });
            await this.#checkLoginOptions(owner, generation, "check_save");
          }
          return;
        }
        if (!this.#ownsLoginOptions(owner, generation)) return;
        if (saved.instance_id !== state.baseline.instance_id || !matchesLoginOptions(saved, state.intended)) {
          this.#setLoginOptions({ phase: "issue", recovery: "reload", message: "实际配置与本次确认不一致，请重新核对。" }); return;
        }
      }
      this.#setLoginOptions({ baseline: saved, intended: saved, expected_instance: saved.instance_id, phase: "restarting" });
      // 撤销普通连接和原生资源，不发送账号退出；本机管理证明由原生层独立持有。
      this.#update_selection += 1; this.#discovery += 1;
      this.#clear(true); generation = this.#generation; this.#local_start = null;
      runInAction(() => { this.pending = true; this.credentials_epoch += 1; this.phase = "entry"; this.progress = "正在应用登录选项…"; this.local_state = "starting"; this.error = null; });
      const binding = await beginRuntimeConnection();
      if (!this.#ownsLoginOptions(owner, generation)) return;
      let bootstrap: RuntimeBootstrap | null;
      try { bootstrap = await restartNativeRuntime({ instance_id: saved.instance_id, revision: saved.revision }); }
      catch {
        if (!this.#ownsLoginOptions(owner, generation)) return;
        // 只发现原实例，不调用可自动启动随包 Host 的普通 bootstrap。
        this.#setLoginOptions({ recovery: "check_applied", message: "配置已保存，重启结果尚未确认。" });
        await this.#checkLoginOptions(owner, generation, "check_applied");
        return;
      }
      if (!this.#ownsLoginOptions(owner, generation)) return;
      if (!bootstrap) throw new Error("missing bootstrap");
      this.#setLoginOptions({ expected_instance: bootstrap.instance_id });
      await this.#checkLoginOptions(owner, generation, "check_applied");
      if (owner === this.#login_options_owner && generation === this.#generation) {
        await prepareDesktopUpdate(binding, null).catch(() => undefined);
      }
    } catch {
      if (this.#ownsLoginOptions(owner, generation)) this.#setLoginOptions({ phase: "issue", recovery: "check_applied", message: "配置已保存，Runtime 状态尚未确认。请重新检查；若未启动，请检查本机 Runtime。" });
    } finally {
      if (owner === this.#login_options_owner && generation === this.#generation) runInAction(() => { this.pending = false; this.updates.operation_blocked = false; });
    }
  }

  async checkLoginOptions(): Promise<void> {
    const state = this.login_options;
    if (state?.phase !== "issue" || (state.recovery !== "check_save" && state.recovery !== "check_applied")) return;
    const owner = this.#login_options_owner, generation = this.#generation;
    runInAction(() => { this.pending = true; this.updates.operation_blocked = true; });
    try { await this.#checkLoginOptions(owner, generation, state.recovery); }
    finally { if (owner === this.#login_options_owner && generation === this.#generation) runInAction(() => { this.pending = false; this.updates.operation_blocked = false; }); }
  }

  async #checkLoginOptions(owner: number, generation: number, recovery: "check_save" | "check_applied"): Promise<void> {
    const state = this.login_options;
    if (!state?.intended || !state.expected_instance) return;
    this.#setLoginOptions({ phase: recovery === "check_save" ? "checking_save" : "checking_applied" });
    try {
      const actual = await manageLocalHost({ type: "get_status" }, state.expected_instance);
      if (!this.#ownsLoginOptions(owner, generation)) return;
      runInAction(() => { this.mode = actual.current_mode; this.local_state = "ready"; this.local_error = null; });
      if (recovery === "check_save" && state.intended.mode === "enterprise" && !state.intended.center_id && actual.revision === state.baseline?.revision) {
        this.#setLoginOptions({ phase: "edit", recovery: null, message: "未确认中心身份已核验，请重新保存。" }); return;
      }
      if (!matchesLoginOptions(actual, state.intended) || (recovery === "check_applied" && actual.revision !== state.intended.revision)) {
        this.#setLoginOptions({ phase: "issue", baseline: actual, recovery: "reload", message: "实际配置与本次确认不一致，请重新核对。重新载入将放弃未保存的修改。" }); return;
      }
      if (recovery === "check_applied" && loginOptionsApplied(actual, state.intended)) {
        runInAction(() => { this.login_options = null; this.login_options_notice = "登录选项已生效"; }); return;
      }
      this.#setLoginOptions({ phase: "issue", baseline: actual, draft: { mode: actual.mode ?? "personal", center_url: actual.center_url ?? "" },
        recovery: actual.restart_required ? "restart" : recovery, message: "当前配置已保存，尚未确认重启。" });
    } catch {
      if (!this.#ownsLoginOptions(owner, generation)) return;
      if (recovery === "check_applied") runInAction(() => { this.local_state = "failed"; this.local_error = "Runtime 生效状态尚未确认，请在登录选项中重新检查。"; });
      this.#setLoginOptions({ phase: "issue", recovery, message: recovery === "check_save" ? "保存结果尚未确认，请重新检查。" : "配置已保存，Runtime 启动或生效状态尚未确认。请重新检查，或检查本机 Runtime。" });
    }
  }

  #ownsLoginOptions(owner: number, generation: number): boolean {
    return owner === this.#login_options_owner && generation === this.#generation && this.login_options !== null && this.target === "local" && this.phase !== "workspace";
  }

  #setLoginOptions(patch: Partial<LoginOptionsState>): void {
    runInAction(() => { if (this.login_options) this.login_options = { ...this.login_options, ...patch }; });
  }

  get can_retry(): boolean { return this.#bootstrap !== null && this.phase !== "workspace"; }
  get update_required(): boolean { return this.updates.snapshot.required || this.host_update_required; }
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
      await this.updates.initialize();
      const update = this.updates.snapshot;
      if (update.binding_id) {
        runInAction(() => {
          this.target = update.resume_remote ? "remote" : "local";
          this.address = update.resume_remote ?? "";
          this.mode = update.host_mode ?? this.mode;
          if (!update.resume_remote) this.local_state = "ready";
        });
      }
      if (this.update_required) return;
      try {
        const saved = await restoreRuntimeConnection();
        if (saved && generation === this.#generation) {
          if (!await this.#recoverOrganizationSwitch(saved)) await this.#enter(saved, generation);
          return;
        }
      } catch (error) { if (generation === this.#generation) this.#connectionError(error); }
      const remote = this.updates.snapshot.resume_remote;
      if (remote) runInAction(() => { this.target = "remote"; this.address = remote; });
      if (update.binding_id) return;
      void this.#prepareEntryUpdate(remote);
      return;
    }
    const token = takeWebLoginToken();
    try {
      const capabilities = await discoverWebHost();
      if (generation !== this.#generation) return;
      runInAction(() => { this.mode = capabilities.mode; });
      if (!token && await this.#recoverOrganizationSwitch({
        base_url: window.location.origin, instance_id: "", capabilities,
        access_token: "", started_runtime: false, authentication: "web",
      })) return;
      if (token) { await loginWeb({ method: "token", token }); this.#channel?.postMessage("changed"); }
      const bootstrap = await bootstrapWebRuntime();
      if (generation === this.#generation) await this.#enter(bootstrap, generation);
    } catch (error) {
      if (generation !== this.#generation) return;
      runInAction(() => {
        this.phase = "login";
        this.error = error instanceof WebLoginError && error.code === "authentication_required" && !token ? null : displayError(error);
      });
      if (!(error instanceof WebLoginError && error.code === "authentication_required" && !token)) this.#connectionError(error);
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
    const generation = this.#generation;
    this.#local_start = start().then((bootstrap) => {
      if (generation !== this.#generation) return bootstrap;
      runInAction(() => { this.local_state = "ready"; if (this.target === "local") this.mode = bootstrap.capabilities.mode; });
      return bootstrap;
    }, (error: unknown) => {
      if (generation !== this.#generation) throw error;
      runInAction(() => {
        this.local_state = typeof error === "object" && error !== null && "code" in error && error.code === "runtime_upgrade_required" ? "upgrade_required" : "failed";
        this.local_error = displayError(error);
      });
      throw error;
    });
    return this.#local_start;
  }

  chooseTarget(target: "local" | "remote"): void {
    if (target === this.target || this.updates.installing || this.login_options_busy) return;
    this.closeLoginOptions();
    this.login_options_notice = null;
    this.#discovery += 1;
    this.target = target;
    this.error = null;
    this.host_update_required = false;
    if (this.phase !== "workspace") void this.#prepareEntryUpdate(target === "local" ? null : this.address);
    const generation = this.#generation;
    if (target === "local") void this.startLocal().then((value) => { if (generation === this.#generation && this.target === "local") runInAction(() => { this.mode = value.capabilities.mode; }); }).catch(() => undefined);
    if (this.pending) {
      this.#clear();
      this.phase = "entry";
    }
  }

  async discoverTarget(origin: string): Promise<void> {
    if (this.updates.installing || this.login_options_busy) return;
    const changed = origin !== this.address;
    runInAction(() => { this.address = origin; if (changed) this.host_update_required = false; });
    if (this.phase !== "workspace" && (changed || !this.updates.snapshot.binding_id)) void this.#prepareEntryUpdate(origin);
    const discovery = ++this.#discovery;
    try {
      const capabilities = await probeRuntimeTarget(origin);
      if (discovery === this.#discovery) runInAction(() => { this.mode = capabilities.mode; this.error = null; });
    } catch (error) { if (discovery === this.#discovery) runInAction(() => { this.error = displayError(error); }); }
  }

  async connectDesktop(origin: string | null, password: string, remember: boolean, username = ""): Promise<void> {
    if (this.pending || this.login_options_busy || this.updates.installing || this.updates.downloading || this.update_required) return;
    this.#return_to_settings = this.phase === "workspace";
    this.closeLoginOptions();
    this.#update_selection += 1;
    this.#clear();
    const generation = this.#generation;
    runInAction(() => {
      this.pending = true; this.error = null; this.target = origin === null ? "local" : "remote";
      this.address = origin ?? ""; this.progress = "正在验证 Host 与账号…"; this.phase = "entry";
    });
    try {
      const binding = await beginRuntimeConnection();
      if (generation !== this.#generation) return;
      if (origin === null) {
        try { await this.startLocal(true); }
        catch (error) {
          await prepareDesktopUpdate(binding, null).catch(() => undefined);
          throw error;
        }
      }
      if (generation !== this.#generation) return;
      const result = await connectRuntimeTarget(binding, origin, password, remember, username);
      if (generation !== this.#generation) return;
      if (result.organization_selection) {
        this.#organization_target = { binding, origin };
        runInAction(() => { this.organization_selection = result.organization_selection ?? null; });
        return;
      }
      if (!result.bootstrap) throw new Error("Host 未返回登录状态。");
      runInAction(() => { this.warning = result.warning; });
      await this.#enter(result.bootstrap, generation);
    } catch (error) { if (generation === this.#generation) this.#connectionError(error); }
    finally { if (generation === this.#generation) runInAction(() => { this.pending = false; }); }
  }

  async login(password: string, username = ""): Promise<void> {
    if (this.pending || this.update_required) return;
    const generation = ++this.#generation;
    runInAction(() => { this.pending = true; this.error = null; });
    try {
      const reply = await loginWeb(this.mode === "enterprise" ? { method: "enterprise", username, password, native: false } : { method: "password", password, native: false });
      if (generation !== this.#generation) return;
      if (reply?.organization_selection) {
        runInAction(() => { this.organization_selection = reply.organization_selection ?? null; });
        return;
      }
      this.#channel?.postMessage("changed");
      const bootstrap = await bootstrapWebRuntime();
      if (generation === this.#generation) await this.#enter(bootstrap, generation);
    } catch (error) { if (generation === this.#generation) this.#connectionError(error); }
    finally { if (generation === this.#generation) runInAction(() => { this.pending = false; }); }
  }

  cancelOrganizationSelection(): void {
    this.organization_selection = null;
    this.#organization_target = null;
  }

  async selectOrganization(organization_id: string): Promise<void> {
    const selection = this.organization_selection;
    if (this.pending || !selection?.organizations.some((org) => org.id === organization_id)) return;
    const generation = this.#generation;
    const target = this.#organization_target;
    runInAction(() => { this.pending = true; this.error = null; this.progress = "正在进入组织…"; });
    try {
      let bootstrap: RuntimeBootstrap | null;
      if (target) {
        const result = await connectRuntimeTarget(target.binding, target.origin, "", false, "", {
          selectionToken: selection.selection_token, organizationId: organization_id,
        });
        bootstrap = result.bootstrap;
      } else {
        await loginWeb({ method: "enterprise_select", organization_id, selection_token: selection.selection_token });
        if (generation !== this.#generation) return;
        this.#channel?.postMessage("changed");
        bootstrap = await bootstrapWebRuntime();
      }
      if (generation !== this.#generation) return;
      if (!bootstrap) throw new Error("Host 未返回登录状态。");
      this.cancelOrganizationSelection();
      await this.#enter(bootstrap, generation);
    } catch (error) {
      if (generation === this.#generation) {
        this.cancelOrganizationSelection();
        this.#connectionError(error);
      }
    } finally {
      if (generation === this.#generation) runInAction(() => { this.pending = false; });
    }
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
    } catch (error) { if (generation === this.#generation) { runInAction(() => { this.phase = "login"; }); this.#connectionError(error); } }
  }

  /** 焦点恢复和同源通知都重新核验；不依据通知内容切换账号，也不重叠发出核验。 */
  async recheckSession(): Promise<void> {
    if (this.desktop || this.pending || this.#checking || this.organization_switch && "preview" in this.organization_switch && this.organization_switch.phase !== "confirm") return;
    this.#checking = true;
    const generation = this.#generation;
    try {
      const bootstrap = await bootstrapWebRuntime();
      if (generation !== this.#generation) return;
      // 同一身份的焦点/广播核验不能重启初始化；失败后只由“重试初始化”触发。
      if (this.#bootstrap && this.#bootstrap.login_context === bootstrap.login_context && !this.update_required) return;
      this.#clear();
      await this.#enter(bootstrap, this.#generation);
    } catch (error) {
      if (generation !== this.#generation) return;
      const code = typeof error === "object" && error !== null && "code" in error ? error.code : null;
      if (code === "authentication_required" || code === "login_context_changed") {
        const required = this.update_required;
        this.#clear();
        runInAction(() => { this.phase = "login"; this.host_update_required = required; });
      }
      // 核验请求失败不代表 Host 事件流断连；真实断连由连接所有者检测和恢复。
      this.#connectionError(error);
    } finally { this.#checking = false; }
  }

  async retryInitialization(): Promise<void> {
    if (!this.#bootstrap || this.pending || this.login_options_busy || this.update_required) return;
    const bootstrap = this.#bootstrap;
    const generation = this.#generation;
    runInAction(() => { this.pending = true; this.error = null; });
    try { await this.#enter(bootstrap, generation); }
    finally { if (generation === this.#generation) runInAction(() => { this.pending = false; }); }
  }

  async #requestOrganizationSwitch(request: HostOrganizationSwitchRequest, source: RuntimeBootstrap) {
    if (this.desktop) return switchRuntimeOrganization(request, source);
    const client = new RuntimeClient(source);
    try { return { result: await client.switchOrganization(request), bootstrap: null }; }
    finally { client.dispose(); }
  }

  async openOrganizationSwitch(): Promise<void> {
    const current = this.organization_switch;
    if (current?.phase === "stop_failed" || current?.phase === "exchange_failed" || current?.phase === "verify") {
      runInAction(() => { this.organization_switch_visible = true; });
      return;
    }
    const source = this.#bootstrap;
    if (!source || !this.session?.identity || this.pending || this.phase !== "workspace") return;
    const loading = { phase: "loading" } as const;
    runInAction(() => { this.organization_switch = loading; this.organization_switch_visible = false; });
    try {
      const { result } = await this.#requestOrganizationSwitch({ action: "options" }, source);
      if (this.organization_switch !== loading || this.#bootstrap !== source) return;
      if (result.state !== "options") throw new Error("无法读取可切换的组织。");
      runInAction(() => { this.organization_switch = { phase: "options", organizations: result.organizations }; });
    } catch (error) {
      if (this.organization_switch === loading) runInAction(() => { this.organization_switch = { phase: "options", organizations: [], message: displayError(error) }; });
    }
  }

  closeOrganizationSwitch(): void {
    if (this.pending) return;
    this.organization_switch_visible = false;
    if (this.organization_switch?.phase !== "stop_failed" && this.organization_switch?.phase !== "exchange_failed") this.organization_switch = null;
  }

  leaveFailedOrganizationSwitch(): void {
    if (this.pending || this.organization_switch?.phase !== "stop_failed") return;
    this.#forgetOrganizationSwitch();
    this.#authenticationRequired();
    runInAction(() => { this.error = "当前组织收尾未完成，请重启 Runtime 后重新登录。"; });
  }

  async previewOrganizationSwitch(organization_id: string): Promise<void> {
    const source = this.#bootstrap;
    const options = this.organization_switch;
    if (!source || this.pending || options?.phase !== "options" || !options.organizations.some((organization) => organization.id === organization_id)) return;
    const checking = { phase: "checking" } as const;
    runInAction(() => { this.organization_switch = checking; this.organization_switch_visible = true; });
    try {
      const { result } = await this.#requestOrganizationSwitch({ action: "preview", organization_id }, source);
      if (!this.#ownsOrganizationSwitch(checking, source)) return;
      if (result.state !== "confirm") throw new Error("无法核对当前组织的任务。");
      runInAction(() => { this.organization_switch = { phase: "confirm", preview: result }; });
    } catch (error) {
      if (this.#ownsOrganizationSwitch(checking, source)) runInAction(() => {
        this.organization_switch = { phase: "selection_error", message: displayError(error) };
      });
    }
  }

  #ownsOrganizationSwitch(state: OrganizationSwitchState, source: RuntimeBootstrap): boolean {
    return this.organization_switch === state && this.#bootstrap === source;
  }

  async confirmOrganizationSwitch(): Promise<void> {
    if (this.organization_switch?.phase === "confirm") await this.#followOrganizationSwitch(this.organization_switch.preview, true);
  }

  async checkOrganizationSwitch(): Promise<void> {
    if (this.organization_switch && "preview" in this.organization_switch && this.organization_switch.phase !== "confirm") {
      await this.#followOrganizationSwitch(this.organization_switch.preview, false);
    }
  }

  async resumeOrganizationAfterSwitch(): Promise<void> {
    const state = this.organization_switch, source = this.#bootstrap;
    if (!source || state?.phase !== "exchange_failed" || this.pending) return;
    runInAction(() => { this.pending = true; });
    try {
      const active_source = this.desktop || source.login_context ? source : await bootstrapWebRuntime();
      if (this.#bootstrap !== source || active_source.instance_id !== source.instance_id) return;
      const { result } = await this.#requestOrganizationSwitch({ action: "resume", operation_id: state.preview.operation_id }, active_source);
      if (this.#bootstrap !== source || result.state !== "resumed") return;
      this.#forgetOrganizationSwitch();
      this.#clear();
      await this.#enter(active_source, this.#generation);
    } catch {
      if (this.#bootstrap === source) runInAction(() => { this.organization_switch = { ...state, phase: "verify" }; });
    } finally {
      if (this.#bootstrap === source) runInAction(() => { this.pending = false; });
    }
  }

  async #recoverOrganizationSwitch(source: RuntimeBootstrap): Promise<boolean> {
    let saved: { base_url: string; instance_id: string; preview: SwitchPreview };
    try {
      const encoded = sessionStorage.getItem(PENDING_ORGANIZATION_SWITCH);
      if (!encoded) return false;
      saved = JSON.parse(encoded);
      if (!saved.preview?.operation_id || !saved.preview.organization?.id || saved.base_url !== source.base_url
        || (source.instance_id && source.instance_id !== saved.instance_id)) return false;
    } catch { return false; }
    this.#bootstrap = { ...source, instance_id: saved.instance_id };
    if (this.desktop) activateRuntimeBinding(this.#bootstrap);
    runInAction(() => { this.organization_switch_visible = true; });
    await this.#followOrganizationSwitch(saved.preview, false, true);
    return this.#bootstrap !== null || this.error !== null;
  }

  #forgetOrganizationSwitch(): void {
    try { sessionStorage.removeItem(PENDING_ORGANIZATION_SWITCH); }
    catch { /* 状态查询仍可在当前页面进行。 */ }
  }

  async #followOrganizationSwitch(preview: SwitchPreview, confirm: boolean, recovering = false): Promise<void> {
    const source = this.#bootstrap;
    const generation = this.#generation;
    if (!source || this.pending) return;
    const operation_id = preview.operation_id;
    if (confirm) {
      try { sessionStorage.setItem(PENDING_ORGANIZATION_SWITCH, JSON.stringify({ base_url: source.base_url, instance_id: source.instance_id, preview })); }
      catch { /* 此窗口仍能查询进度。 */ }
    }
    runInAction(() => { this.pending = true; this.updates.operation_blocked = true; this.organization_switch = { phase: confirm ? "stopping" : "verify", preview }; });
    try {
      let reply = await this.#requestOrganizationSwitch({ action: confirm ? "confirm" : "status", operation_id }, source);
      const deadline = Date.now() + 45_000;
      while (generation === this.#generation && (reply.result.state === "stopping" || reply.result.state === "exchanging")) {
        runInAction(() => { this.organization_switch = { phase: reply.result.state as "stopping" | "exchanging", preview }; });
        if (Date.now() >= deadline) throw new Error("切换仍在进行。");
        await new Promise<void>((resolve) => setTimeout(resolve, SWITCH_POLL_MS));
        if (generation !== this.#generation) return;
        reply = await this.#requestOrganizationSwitch({ action: "status", operation_id }, source);
      }
      if (generation !== this.#generation) return;
      const { result, bootstrap } = reply;
      switch (result.state) {
        case "switched": {
          const login = result.login;
          if (login.instance_id !== source.instance_id || login.identity?.organization_id !== preview.organization.id || (this.desktop && !bootstrap)) throw new Error("组织切换结果与请求不一致。");
          const target = { ...(bootstrap ?? source), session: login, login_context: login.login_context };
          this.#forgetOrganizationSwitch();
          this.#clear();
          this.#channel?.postMessage("changed");
          const target_generation = this.#generation;
          await this.#enter(target, target_generation, `正在加载「${preview.organization.name}」工作区…`);
          if (target_generation === this.#generation && this.phase !== "workspace" && this.error) {
            runInAction(() => { this.error = `组织已切换，工作区加载失败。${this.error}`; });
          }
          return;
        }
        case "changed":
          this.#forgetOrganizationSwitch();
          if (recovering) this.#clear();
          else {
            runInAction(() => {
              this.organization_switch = { phase: "selection_error", message: "任务或组织状态已变化，请重新选择组织。" };
            });
          }
          return;
        case "stop_failed":
        case "exchange_failed":
          runInAction(() => { this.organization_switch = { phase: result.state, preview }; });
          return;
        case "reauthenticate":
          this.#forgetOrganizationSwitch();
          this.#authenticationRequired();
          runInAction(() => { this.error = "切换结果尚未确认，请重新登录并选择组织。"; });
          return;
        default: throw new Error("无法确认切换状态。");
      }
    } catch (error) {
      if (generation !== this.#generation) return;
      if (!confirm && typeof error === "object" && error !== null && "code" in error && error.code === "authentication_required") {
        this.#forgetOrganizationSwitch();
        this.#authenticationRequired();
        runInAction(() => { this.error = "切换结果尚未确认，请重新登录并选择组织。"; });
      } else runInAction(() => { this.organization_switch = { phase: "verify", preview }; });
    } finally {
      if (generation === this.#generation) runInAction(() => { this.pending = false; this.updates.operation_blocked = false; });
    }
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
    } catch (error) { if (generation === this.#generation) this.#connectionError(error); }
    finally { runInAction(() => { this.pending = false; }); }
  }

  async openWeb(): Promise<void> {
    if (this.web_pending) return;
    runInAction(() => { this.web_pending = true; });
    try { await openRuntimeWeb(); }
    catch (error) { runInAction(() => { this.error = displayError(error); }); }
    finally { runInAction(() => { this.web_pending = false; }); }
  }

  dispose(): void { this.#login_options_owner += 1; this.login_options = null; this.#phase_reaction(); this.#update_reaction(); this.updates.dispose(); this.#update_selection += 1; this.#clear(); this.current?.dispose(); this.current = null; this.#channel?.close(); this.#channel = null; }

  async retryUpdateConnection(): Promise<void> {
    if (this.pending || this.updates.downloading || this.updates.installing) return;
    if (!this.desktop) { await this.recheckSession(); return; }
    // 重连当前目标不清除已知门禁；原生层只有取得有效的新声明后才能解除它。
    const binding = this.updates.snapshot.binding_id;
    if (!binding) return;
    const generation = this.#generation;
    runInAction(() => { this.pending = true; });
    try { await prepareDesktopUpdate(binding, this.target === "local" ? null : this.address); }
    catch { /* 网络失败保留已知的更新要求。 */ }
    finally { if (generation === this.#generation) runInAction(() => { this.pending = false; }); }
  }

  #enterUpdateGate(): void {
    this.#login_options_owner += 1;
    runInAction(() => { this.login_options = null; this.updates.operation_blocked = false; });
    // 只释放本客户端的业务投影与订阅，不退出 Host 账号、不停止任务或取消更新。
    this.#generation += 1;
    this.#auth_reaction?.(); this.#auth_reaction = null;
    this.current?.settings.close();
    this.current?.dispose(); this.#initial?.dispose(); this.#initial = null;
    this.#bootstrap = null;
    clearRuntimeBinding();
    runInAction(() => {
      this.current = this.desktop ? new RootStore({ flush_application_preferences: this.flush_application_preferences, target_kind: this.target, restore_resources: false }) : null;
      this.session = null; this.pending = false; this.password_open = false; this.logout_confirmation = false;
      this.organization_switch = null; this.organization_switch_visible = false;
      this.phase = this.desktop ? "entry" : "login"; this.error = null;
    });
  }

  #connectionError(error: unknown): void {
    if (typeof error === "object" && error !== null && "code" in error && error.code === "center_protocol_incompatible") {
      runInAction(() => { this.host_update_required = !this.updates.snapshot.required; });
      this.#enterUpdateGate();
    } else runInAction(() => { this.error = displayError(error); });
  }

  async #prepareEntryUpdate(origin: string | null): Promise<void> {
    const selection = ++this.#update_selection;
    try {
      const binding = await beginRuntimeConnection();
      if (selection !== this.#update_selection) return;
      if (origin === null) await this.startLocal().catch(() => undefined);
      // DMG 已替换随包来源时，先由用户确认接续本机 Host；不能用仍在运行的旧协议遮住该入口。
      if (origin === null && this.local_state === "upgrade_required") return;
      // await 期间连接可以进入工作区；解除前一次判断留下的静态收窄，仍按最新状态检查。
      if (selection !== this.#update_selection || (this.phase as ApplicationConnectionStore["phase"]) === "workspace") return;
      if (origin === null || origin.trim()) await prepareDesktopUpdate(binding, origin);
    } catch { /* 普通检查失败保持入口隐藏，既有连接错误独立显示。 */ }
  }

  #clear(keep_login_options = false): void {
    this.cancelOrganizationSelection();
    if (!keep_login_options) runInAction(() => {
      this.#login_options_owner += 1;
      this.login_options = null;
      this.updates.operation_blocked = false;
    });
    this.#generation += 1;
    this.#auth_reaction?.(); this.#auth_reaction = null;
    this.current?.settings.close();
    this.current?.dispose(); this.#initial?.dispose(); this.#initial = null;
    this.#bootstrap = null;
    clearRuntimeBinding();
    runInAction(() => { this.current = this.desktop ? new RootStore({ flush_application_preferences: this.flush_application_preferences, target_kind: this.target, restore_resources: false }) : null; this.session = null; this.pending = false; this.logout_confirmation = false; this.password_open = false; this.organization_switch = null; this.organization_switch_visible = false; this.host_update_required = false; });
  }

  #authenticationRequired(): void {
    this.#clear();
    if (this.desktop) {
      void beginRuntimeConnection().catch(() => undefined);
      void this.startLocal(true).catch(() => undefined);
    }
    runInAction(() => { this.phase = this.desktop ? "entry" : "login"; this.error = "登录已失效，请重新登录。"; });
  }

  async #enter(bootstrap: RuntimeBootstrap, generation: number, progress = "正在初始化用户项目…"): Promise<void> {
    if (this.update_required || generation !== this.#generation) return;
    // 登录成功和初始化成功是两个阶段；初始化失败仍保留凭据供显式重试、改密和退出。
    this.#bootstrap = bootstrap;
    this.#auth_reaction?.();
    if (this.desktop) activateRuntimeBinding(bootstrap);
    runInAction(() => { this.phase = this.desktop ? "entry" : "loading"; this.progress = progress; this.mode = bootstrap.capabilities.mode; });
    const identity_client = new RuntimeClient(bootstrap);
    let session: HostLoginResult;
    try { session = bootstrap.session ?? await identity_client.session(); }
    catch (error) {
      if (generation === this.#generation) {
        // 原生刷新恢复的 Token 也可能已过期，应回到登录，不能显示初始化重试。
        if (typeof error === "object" && error !== null && "code" in error && error.code === "authentication_required") this.#authenticationRequired();
        else this.#connectionError(error);
      }
      return;
    }
    finally { identity_client.dispose(); }
    if (generation !== this.#generation || this.update_required) return;
    runInAction(() => { this.session = session; });
    const user = session.identity;
    const namespace = user ? `${bootstrap.base_url}/users/${user.center_id}/${user.organization_id}/${user.user_id}` : bootstrap.target_kind === "remote" ? bootstrap.base_url : undefined;
    const store = this.#initial && !user ? this.#initial : new RootStore({ flush_application_preferences: this.flush_application_preferences, target_kind: bootstrap.target_kind, target_address: namespace, restore_resources: this.desktop });
    // 原生刷新恢复个人连接时可以复用尚未连接的初始 Store，不能先销毁再连接。
    if (this.current !== store) this.current?.dispose();
    if (this.#initial !== store && this.#initial !== this.current) this.#initial?.dispose();
    this.#initial = null;
    // 基础连接就绪即完成登录；会话正文、右栏及置顶恢复继续由 RootStore 管理。
    // 不能让这些恢复请求继续占用账号操作锁，也不能在它们迟到完成时重设页面。
    let connected!: () => void;
    const ready = new Promise<void>((resolve) => { connected = resolve; });
    this.#auth_reaction = reaction(() => [store.local_connection.state, store.local_connection.last_error_code, store.local_connection.startup] as const, ([state, code, health]) => {
      if (generation !== this.#generation || (this.logout_confirmation && this.pending)) return;
      if (this.organization_switch && "preview" in this.organization_switch && this.organization_switch.phase !== "confirm") return;
      if (health && health.status !== "ready") runInAction(() => { this.progress = startupMessage(health); });
      if (code === "center_protocol_incompatible") { this.#connectionError({ code }); }
      else if (code === "login_context_changed") { this.#clear(); runInAction(() => { this.phase = "loading"; }); void this.recheckSession(); }
      else if (code === "authentication_required") this.#authenticationRequired();
      else if (state !== "connected" && this.phase === "workspace") {
        // 暂时断连仍显示原工作区及快照，由已有提示和重连流程恢复。
        runInAction(() => {
          if (state !== "disconnected" && state !== "reconnecting" && state !== "connecting") this.phase = this.desktop ? "entry" : "loading";
          this.error = store.local_connection.error_message ?? "连接已中断，请重试。";
        });
      } else if (state === "connected") { runInAction(() => { this.phase = "workspace"; this.error = null; }); connected(); }
    });
    runInAction(() => { this.current = store; });
    await Promise.race([store.connect({ ...bootstrap, session }), ready]);
    if (generation !== this.#generation || this.update_required) { store.dispose(); return; }
    runInAction(() => {
      if (store.local_connection.state === "connected") {
        if (this.desktop) { if (this.#return_to_settings) store.settings.open("runtime"); else store.settings.close(); }
        this.#return_to_settings = false; this.phase = "workspace"; this.connected_target = bootstrap.target_kind ?? null; this.error = null;
      }
      else { this.error = store.local_connection.error_message ?? "初始化失败，请重试。"; }
    });
  }
}

function displayError(error: unknown): string {
  return typeof error === "object" && error !== null && "message" in error && typeof error.message === "string" ? error.message : "无法连接 Host。";
}
