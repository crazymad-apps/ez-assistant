import type { ResourceWorkspaceStore } from "../features/resource-workspace/ResourceWorkspaceStore";
import { action, computed, makeObservable, observable, runInAction } from "mobx";
import type { ApplicationSnapshot } from "../generated/assistant-protocol";
import {
  listenDesktopLifecycleIntents,
  listenNativeRuntimeMutations,
  quitDesktopClient,
  restartNativeRuntime,
  stopNativeRuntime,
  takePendingDesktopLifecycleIntent,
  updateNativeRuntimeState,
  type DesktopLifecycleIntent,
  type NativeRuntimeMutationEvent,
  type NativeRuntimeState,
} from "../native-bridge/desktopLifecycle";
import type { DesktopCloseBehavior, DesktopPreferences } from "../native-bridge/desktopPreferences";
import type { RuntimeBootstrap } from "../native-bridge/runtimeBootstrap";
import type { RuntimeConnectionState } from "./ConnectionStore";

type Dependencies = Readonly<{
  resources: ResourceWorkspaceStore;
  is_local_target?: () => boolean;
  get_application: () => ApplicationSnapshot | null;
  prepare_runtime_mutation: (kind: "stop" | "restart") => void;
  reconnect_runtime: (bootstrap?: RuntimeBootstrap) => Promise<void>;
  mark_runtime_stopped: () => void;
  save_preferences: () => void;
  flush_preferences?: () => Promise<void>;
}>;

export type RuntimeImpact = Readonly<{
  active_runs: number;
  queued_inputs: number;
  pending_approvals: number;
}>;

export class DesktopLifecycleStore {
  intent: DesktopLifecycleIntent | null = null;
  pending = false;
  stop_runtime_on_quit = false;
  error_message: string | null = null;
  close_behavior: DesktopCloseBehavior = "hide_to_tray";
  #disposed = false;
  #unlisten: (() => void) | null = null;
  #unlisten_runtime_mutation: (() => void) | null = null;
  readonly #claim_pending_intent = () => {
    void this.#claimPendingIntent();
  };
  readonly #claim_visible_intent = () => {
    if (document.visibilityState === "visible") void this.#claimPendingIntent();
  };
  readonly #handle_native_runtime_mutation = (event: NativeRuntimeMutationEvent) => {
    if (this.#disposed || this.dependencies.is_local_target?.() === false) return;
    if (event.phase === "preparing") {
      this.dependencies.prepare_runtime_mutation(event.kind);
      return;
    }
    if (event.kind === "stop" && event.succeeded) {
      this.dependencies.mark_runtime_stopped();
      return;
    }
    void this.dependencies.reconnect_runtime().catch(() => undefined);
  };

  constructor(private readonly dependencies: Dependencies) {
    makeObservable(this, {
      intent: observable,
      pending: observable,
      stop_runtime_on_quit: observable,
      error_message: observable,
      close_behavior: observable,
      impact: computed,
      local_impact_known: computed,
      terminal_count: computed,
      applyPreferences: action,
      request: action,
      dismiss: action,
      setStopRuntimeOnQuit: action,
      setCloseBehavior: action,
      confirm: action,
      dispose: action,
    });
  }

  get local_impact_known(): boolean { return this.dependencies.is_local_target?.() !== false; }

  get terminal_count(): number { return this.dependencies.resources.runningTerminalCount(); }

  get impact(): RuntimeImpact {
    const sessions = this.dependencies.get_application()?.active_sessions ?? [];
    return {
      active_runs: sessions.filter((session) => session.active_run_id !== null).length,
      queued_inputs: sessions.reduce((count, session) => count + session.queued_input_count, 0),
      pending_approvals: sessions.reduce((count, session) => count + session.pending_approval_count, 0),
    };
  }

  start(): void {
    this.#disposed = false;
    window.addEventListener("focus", this.#claim_pending_intent);
    document.addEventListener("visibilitychange", this.#claim_visible_intent);
    void listenDesktopLifecycleIntents(this.#claim_pending_intent).then((unlisten) => {
      if (this.#disposed || this.#unlisten) unlisten();
      else this.#unlisten = unlisten;
      void this.#claimPendingIntent();
    });
    void listenNativeRuntimeMutations(this.#handle_native_runtime_mutation).then((unlisten) => {
      if (this.#disposed || this.#unlisten_runtime_mutation) unlisten();
      else this.#unlisten_runtime_mutation = unlisten;
    });
  }

  async #claimPendingIntent(): Promise<void> {
    const intent = await takePendingDesktopLifecycleIntent().catch(() => null);
    if (intent && !this.#disposed) this.request(intent);
  }

  applyPreferences(preferences: DesktopPreferences): void {
    this.close_behavior = preferences.close_behavior;
  }

  request(intent: DesktopLifecycleIntent): void {
    if (this.pending) return;
    this.intent = intent;
    this.stop_runtime_on_quit = false;
    this.error_message = null;
  }

  dismiss(): void {
    if (this.pending) return;
    this.intent = null;
    this.stop_runtime_on_quit = false;
    this.error_message = null;
  }

  setStopRuntimeOnQuit(value: boolean): void {
    this.stop_runtime_on_quit = value;
  }

  setCloseBehavior(value: DesktopCloseBehavior): void {
    this.close_behavior = value;
    this.dependencies.save_preferences();
  }

  async confirm(): Promise<void> {
    const intent = this.intent;
    if (!intent || this.pending) return;
    this.pending = true;
    this.error_message = null;
    let runtime_mutating = false;
    try {
      if (intent === "quit_desktop") {
        if (this.dependencies.flush_preferences) await this.dependencies.flush_preferences();
        await this.dependencies.resources.shutdownTerminals();
      }
      if (intent === "quit_desktop" && !this.stop_runtime_on_quit) {
        await quitDesktopClient();
        return;
      }
      runtime_mutating = true;
      const local = this.dependencies.is_local_target?.() !== false;
      if (intent === "restart_runtime") {
        if (local) this.dependencies.prepare_runtime_mutation("restart");
        await updateNativeRuntimeState("restarting", this.impact);
        const bootstrap = await restartNativeRuntime();
        if (local) await this.dependencies.reconnect_runtime(bootstrap ?? undefined);
      } else {
        if (local) this.dependencies.prepare_runtime_mutation("stop");
        await updateNativeRuntimeState("stopping", this.impact);
        await stopNativeRuntime();
        if (local) this.dependencies.mark_runtime_stopped();
        await updateNativeRuntimeState("stopped", this.impact);
      }
      if (intent === "quit_desktop") {
        await quitDesktopClient();
        return;
      }
      runInAction(() => {
        this.pending = false;
        this.intent = null;
      });
    } catch (error: unknown) {
      let message = error instanceof Error ? error.message : "桌面生命周期操作失败。";
      if (intent === "quit_desktop") {
        try {
          this.dependencies.resources.resumeCreation();
        } catch {
          message += " 终端服务尚未恢复，请重试退出或重启客户端。";
        }
      }
      runInAction(() => {
        this.pending = false;
        this.error_message = message;
      });
      if (runtime_mutating) await updateNativeRuntimeState("disconnected", this.impact).catch(() => undefined);
    }
  }

  syncRuntimeState(state: RuntimeConnectionState): void {
    if (this.dependencies.is_local_target?.() === false) return;
    const native_state: NativeRuntimeState = ({
      booting: "connecting",
      starting_runtime: "connecting",
      connecting: "connecting",
      connected: "connected",
      reconnecting: "reconnecting",
      disconnected: "disconnected",
      component_mismatch: "disconnected",
      stopping_runtime: "stopping",
      restarting_runtime: "restarting",
      runtime_stopped: "stopped",
    } as const)[state];
    void updateNativeRuntimeState(native_state, this.impact).catch(() => undefined);
  }

  dispose(): void {
    this.#disposed = true;
    window.removeEventListener("focus", this.#claim_pending_intent);
    document.removeEventListener("visibilitychange", this.#claim_visible_intent);
    this.#unlisten?.();
    this.#unlisten = null;
    this.#unlisten_runtime_mutation?.();
    this.#unlisten_runtime_mutation = null;
  }
}
