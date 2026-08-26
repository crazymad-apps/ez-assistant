import { runInAction } from "mobx";
import type {
  ChildTaskId,
  GetApplicationSnapshotResult,
  GetChildTaskViewResult,
  GetSessionViewResult,
  RuntimeEvent,
  RuntimeEventEnvelope,
  SessionId,
} from "../generated/assistant-protocol";
import {
  bootstrapRuntime,
  type RuntimeBootstrap,
  type RuntimeBootstrapFailure,
} from "../native-bridge/runtimeBootstrap";
import { RuntimeClient, RuntimeClientError } from "../runtime-client/RuntimeClient";
import type { ConnectionStore } from "./ConnectionStore";
import type { LiveExecutionStore } from "./LiveExecutionStore";
import type { NavigationStore } from "./NavigationStore";
import type { RuntimeProjectionStore } from "./RuntimeProjectionStore";

const MAX_AUTOMATIC_RECONNECTS = 3;

type RuntimeLifecycleDependencies = Readonly<{
  connection: ConnectionStore;
  live_execution: LiveExecutionStore;
  navigation: NavigationStore;
  projection: RuntimeProjectionStore;
  report_interaction_error: (message: string) => void;
}>;

/** Owns Runtime transport, snapshot/event synchronization, and reconnect timers. */
export class RuntimeLifecycleCoordinator {
  #client: RuntimeClient | null = null;
  #event_abort: AbortController | null = null;
  #connect_promise: Promise<void> | null = null;
  #refresh_timer: number | null = null;
  #refresh_application_pending = false;
  #reconnect_timer: number | null = null;
  #reconnect_attempt = 0;
  #disposed = false;
  #lifecycle_generation = 0;

  constructor(private readonly dependencies: RuntimeLifecycleDependencies) {}

  get client(): RuntimeClient | null {
    return this.#client;
  }

  connect(bootstrap?: RuntimeBootstrap): Promise<void> {
    if (this.#disposed) {
      this.#disposed = false;
      this.#connect_promise = null;
    }
    if (this.#connect_promise) {
      return this.#connect_promise;
    }
    const generation = ++this.#lifecycle_generation;
    this.dependencies.connection.beginInitialConnection();
    const connect_promise = this.#establishConnection(false, generation, bootstrap).finally(() => {
      if (this.#connect_promise === connect_promise) {
        this.#connect_promise = null;
      }
    });
    this.#connect_promise = connect_promise;
    return connect_promise;
  }

  retryConnection(): void {
    this.#clearReconnectTimer();
    this.#reconnect_attempt = 0;
    this.dependencies.connection.beginReconnect();
    void this.#establishConnection(true, this.#lifecycle_generation);
  }

  prepareForNativeRuntimeMutation(kind: "stop" | "restart"): void {
    this.#lifecycle_generation += 1;
    this.#connect_promise = null;
    this.#event_abort?.abort();
    this.#event_abort = null;
    this.#client = null;
    this.#clearReconnectTimer();
    this.dependencies.connection.beginRuntimeMutation(kind);
    this.dependencies.projection.markStale();
  }

  reconnectAfterNativeRuntimeMutation(bootstrap?: RuntimeBootstrap): Promise<void> {
    this.#reconnect_attempt = 0;
    return this.connect(bootstrap);
  }

  async selectInitialSession(): Promise<void> {
    const { navigation, projection } = this.dependencies;
    const application = projection.application;
    if (!application) {
      return;
    }
    const current_id = navigation.selected_session_id;
    const current_exists = application.active_sessions.some((session) => session.session_id === current_id);
    const session_id = current_exists ? current_id : (application.active_sessions[0]?.session_id ?? null);
    navigation.selectSession(session_id, false);
    if (!navigation.workspace_expansion_initialized) {
      for (const workspace of application.workspaces) {
        navigation.ensureWorkspaceExpanded(workspace.workspace_id);
      }
    }
    if (session_id) {
      await this.loadSession(session_id);
    }
  }

  async loadApplication(rebase_event_sequence = false): Promise<void> {
    if (!this.#client) {
      return;
    }
    const result = await this.#getApplication(this.#client);
    runInAction(() => {
      if (rebase_event_sequence) {
        this.dependencies.projection.applyApplicationSnapshot(result.snapshot);
      } else {
        this.dependencies.projection.refreshApplicationSnapshot(result.snapshot);
      }
    });
  }

  async loadSession(session_id: SessionId): Promise<void> {
    const client = this.#client;
    const instance_id = client?.instance_id;
    if (!client || !instance_id) {
      return;
    }
    try {
      const result = await client.command({ type: "get_session_view", payload: { session_id } });
      if (
        this.#client?.instance_id !== instance_id
        || this.dependencies.navigation.selected_session_id !== session_id
      ) {
        return;
      }
      runInAction(() => this.#applySessionResult(result.payload));
      const child_task_id = this.dependencies.navigation.selected_child_task_id;
      if (child_task_id) {
        await this.loadChildTask(session_id, child_task_id);
      }
    } catch (error: unknown) {
      if (error instanceof RuntimeClientError && error.code === "snapshot_busy") {
        return;
      }
      if (this.dependencies.navigation.selected_session_id === session_id) {
        runInAction(() => {
          if (isCommandBusinessFailure(error)) {
            this.dependencies.report_interaction_error(displayError(error));
          } else {
            this.dependencies.connection.markDisconnected(displayError(error));
          }
        });
      }
    }
  }

  async loadChildTask(session_id: SessionId, child_task_id: ChildTaskId): Promise<void> {
    const client = this.#client;
    const instance_id = client?.instance_id;
    if (!client || !instance_id) {
      return;
    }
    try {
      const result = await client.command({
        type: "get_child_task_view",
        payload: { session_id, child_task_id },
      });
      if (
        this.#client?.instance_id !== instance_id
        || this.dependencies.navigation.selected_session_id !== session_id
        || this.dependencies.navigation.selected_child_task_id !== child_task_id
      ) {
        return;
      }
      runInAction(() => this.#applyChildTaskResult(result.payload));
    } catch (error: unknown) {
      if (error instanceof RuntimeClientError && error.code === "snapshot_busy") {
        return;
      }
      if (this.dependencies.navigation.selected_child_task_id === child_task_id) {
        runInAction(() => this.dependencies.report_interaction_error(displayError(error)));
      }
    }
  }

  dispose(): void {
    this.#disposed = true;
    this.#lifecycle_generation += 1;
    this.#connect_promise = null;
    this.#event_abort?.abort();
    this.#event_abort = null;
    this.#clearReconnectTimer();
    if (this.#refresh_timer !== null) {
      window.clearTimeout(this.#refresh_timer);
      this.#refresh_timer = null;
    }
    this.#refresh_application_pending = false;
    this.dependencies.live_execution.dispose();
  }

  async #establishConnection(
    is_reconnect: boolean,
    generation: number,
    prepared_bootstrap?: RuntimeBootstrap,
  ): Promise<void> {
    if (!this.#isActiveGeneration(generation)) {
      return;
    }
    if (is_reconnect) {
      this.dependencies.connection.beginReconnect();
    }
    try {
      const bootstrap = prepared_bootstrap ?? await bootstrapRuntime();
      if (!this.#isActiveGeneration(generation)) {
        return;
      }
      const client = new RuntimeClient(bootstrap);
      if (
        this.dependencies.connection.instance_id
        && this.dependencies.connection.instance_id !== client.instance_id
      ) {
        this.dependencies.projection.resetForInstance();
        this.dependencies.live_execution.clear();
      }
      this.#event_abort?.abort();
      const event_abort = new AbortController();
      this.#event_abort = event_abort;
      const buffered_events: RuntimeEventEnvelope[] = [];
      let snapshot_loaded = false;
      const stream = await client.connectEvents(
        {
          onEvent: (event) => {
            if (!snapshot_loaded) {
              buffered_events.push(event);
            } else {
              this.#applyEvent(event);
            }
          },
          onGap: () => this.#handleStreamGap(),
        },
        event_abort.signal,
      );
      this.#client = client;
      const application = await this.#getApplication(client);
      runInAction(() => this.dependencies.projection.applyApplicationSnapshot(application.snapshot));
      snapshot_loaded = true;
      for (const event of buffered_events) {
        if (event.sequence > application.snapshot.observed_sequence) {
          this.#applyEvent(event);
        }
      }
      runInAction(() => {
        this.dependencies.connection.markConnected(
          client.instance_id,
          client.capabilities,
          client.address,
        );
        this.#reconnect_attempt = 0;
      });
      await this.selectInitialSession();
      void stream.closed.then(
        () => this.#handleStreamClosed(event_abort),
        () => this.#handleStreamClosed(event_abort),
      );
    } catch (error: unknown) {
      if (!this.#isActiveGeneration(generation)) {
        return;
      }
      const failure = normalizeFailure(error);
      runInAction(() => {
        if (failure.code === "component_mismatch") {
          this.dependencies.connection.markComponentMismatch(failure.message, failure.code);
        } else {
          this.dependencies.connection.markDisconnected(failure.message, failure.code);
        }
        this.dependencies.projection.markStale();
      });
      this.#scheduleReconnect();
    }
  }

  async #getApplication(client: RuntimeClient): Promise<GetApplicationSnapshotResult> {
    const result = await client.command({ type: "get_application_snapshot", payload: {} });
    return result.payload;
  }

  #applySessionResult(result: GetSessionViewResult): void {
    this.dependencies.projection.applySessionSnapshot(result.snapshot);
    this.dependencies.live_execution.reconcileSession(result.snapshot.value);
  }

  #applyChildTaskResult(result: GetChildTaskViewResult): void {
    this.dependencies.projection.applyChildTaskSnapshot(result.snapshot);
    this.dependencies.live_execution.reconcileChildTask(result.snapshot.value);
  }

  #applyEvent(envelope: RuntimeEventEnvelope): void {
    const accepted = this.dependencies.projection.acceptEvent(envelope);
    if (accepted === "ignored") {
      return;
    }
    if (accepted === "gap") {
      this.#handleStreamGap();
      return;
    }
    this.dependencies.live_execution.buffer(envelope);
    const event = envelope.event;
    if (event.type === "session_deleted") {
      if (this.dependencies.navigation.selected_session_id === event.session_id) {
        this.dependencies.navigation.selectSession(null, false);
      }
      void this.loadApplication().then(() => this.selectInitialSession());
      return;
    }
    if (
      event.type === "config_changed"
      || event.type === "workspace_changed"
      || event.type === "session_created"
      || event.type === "session_changed"
      || event.type === "session_compaction_started"
      || event.type === "session_compaction_finished"
      || event.type === "conversation_committed"
      || event.type === "run_started"
      || event.type === "run_finished"
    ) {
      this.#scheduleRefresh(true);
    } else {
      const refresh_session_id = sessionIdForRefresh(event);
      if (refresh_session_id && refresh_session_id === this.dependencies.navigation.selected_session_id) {
        this.#scheduleRefresh(false);
      }
    }
  }

  #scheduleRefresh(include_application: boolean): void {
    this.#refresh_application_pending ||= include_application;
    if (this.#refresh_timer !== null) {
      window.clearTimeout(this.#refresh_timer);
    }
    this.#refresh_timer = window.setTimeout(() => {
      this.#refresh_timer = null;
      const refresh_application = this.#refresh_application_pending;
      this.#refresh_application_pending = false;
      if (refresh_application) {
        void this.loadApplication();
      }
      const session_id = this.dependencies.navigation.selected_session_id;
      if (session_id) {
        void this.loadSession(session_id);
      }
    }, 80);
  }

  #handleStreamGap(): void {
    this.dependencies.projection.markStale();
    void this.loadApplication(true);
    const session_id = this.dependencies.navigation.selected_session_id;
    if (session_id) {
      void this.loadSession(session_id);
    }
  }

  #handleStreamClosed(owner: AbortController): void {
    if (this.#disposed || owner.signal.aborted || this.#event_abort !== owner) {
      return;
    }
    runInAction(() => {
      this.dependencies.connection.markDisconnected("Runtime 连接已中断，正在尝试恢复。");
      this.dependencies.projection.markStale();
    });
    this.#scheduleReconnect();
  }

  #scheduleReconnect(): void {
    if (
      this.#disposed
      || this.#reconnect_attempt >= MAX_AUTOMATIC_RECONNECTS
      || this.dependencies.connection.state === "component_mismatch"
    ) {
      return;
    }
    const delay = 500 * 2 ** this.#reconnect_attempt + Math.round(Math.random() * 180);
    this.#reconnect_attempt += 1;
    this.#clearReconnectTimer();
    this.#reconnect_timer = window.setTimeout(() => {
      this.#reconnect_timer = null;
      void this.#establishConnection(true, this.#lifecycle_generation);
    }, delay);
  }

  #isActiveGeneration(generation: number): boolean {
    return !this.#disposed && generation === this.#lifecycle_generation;
  }

  #clearReconnectTimer(): void {
    if (this.#reconnect_timer !== null) {
      window.clearTimeout(this.#reconnect_timer);
      this.#reconnect_timer = null;
    }
  }
}

function normalizeFailure(error: unknown): RuntimeBootstrapFailure {
  if (typeof error === "object" && error !== null) {
    const candidate = error as Record<string, unknown>;
    if (typeof candidate.code === "string" && typeof candidate.message === "string") {
      return { code: candidate.code, message: candidate.message };
    }
  }
  return { code: "runtime_unavailable", message: displayError(error) };
}

function displayError(error: unknown): string {
  return error instanceof Error ? error.message : "无法连接本地 Runtime。";
}

function isCommandBusinessFailure(error: unknown): error is RuntimeClientError {
  return error instanceof RuntimeClientError
    && error.code !== "transport_error"
    && error.code !== "protocol_mismatch";
}

function sessionIdForRefresh(event: RuntimeEvent): SessionId | null {
  switch (event.type) {
    case "conversation_committed":
    case "step_committed":
      return event.owner.session_id;
    case "child_task_event":
      return event.session_id;
    case "session_variant_changed":
    case "session_approval_mode_changed":
      return event.session.session_id;
    case "approval_requested":
      return event.approval.session_id;
    case "queue_changed":
    case "session_compaction_started":
    case "session_compaction_finished":
    case "work_plan_changed":
    case "goal_changed":
    case "permission_reloaded":
    case "approval_resolved":
    case "approval_cancelled":
    case "run_accepted":
    case "run_started":
    case "run_cancelling":
    case "step_started":
    case "usage_updated":
    case "run_finished":
      return event.session_id;
    default:
      return null;
  }
}
