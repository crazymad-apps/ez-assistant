import { refreshRuntimeConnection } from "../native-bridge/runtimeConnection";
import { reaction, runInAction } from "mobx";
import type {
  ChildTaskId,
  ConversationOwner,
  GetApplicationSnapshotRequest,
  GetApplicationSnapshotResult,
  GetChildTaskViewResult,
  GetSessionViewResult,
  RuntimeCommand,
  RuntimeCommandResult,
  RuntimeEvent,
  RuntimeEventEnvelope,
  RuntimeErrorInfo,
  SessionId,
} from "@ez-assistant/protocol";
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
import { workspaceForDraftKey } from "./NewSessionDraftStore";

const MAX_AUTOMATIC_RECONNECTS = 3;
const SNAPSHOT_BUDGET_MS = 15_000;
const SNAPSHOT_RETRY_DELAYS = [250, 750] as const;
const MAX_SNAPSHOT_EVENTS = 2048;
const MAX_SNAPSHOT_EVENT_BYTES = 2 * 1024 * 1024;

type SnapshotQueryType = "get_application_snapshot" | "get_session_view" | "get_child_task_view" | "get_session";
type SnapshotQuery = <TType extends SnapshotQueryType>(
  command: Extract<RuntimeCommand, { readonly type: TType }>,
) => Promise<Extract<RuntimeCommandResult, { readonly type: TType }>>;

type SnapshotOwner = Readonly<{ session_id: SessionId; child_task_id: ChildTaskId | null }>;
type SnapshotRead = {
  controller: AbortController;
  client: RuntimeClient;
  owner: SnapshotOwner | null;
  promise: Promise<unknown>;
  refresh_pending: boolean;
  events: RuntimeEventEnvelope[];
  event_bytes: number;
  overflow: boolean;
};

type RuntimeLifecycleDependencies = Readonly<{
  connection: ConnectionStore;
  target_device?: string;
  create_client?: () => Promise<RuntimeClient>;
  on_event?: (event: RuntimeEventEnvelope) => void;
  require_selected_target?: boolean;
  live_execution: LiveExecutionStore;
  navigation: NavigationStore;
  projection: RuntimeProjectionStore;
  report_interaction_error: (message: string) => void;
  refresh_device_gateway: () => void;
  mark_device_gateway_stale: () => void;
  on_title_generation_finished: (
    session_id: SessionId,
    trigger: "automatic" | "manual",
    outcome: "succeeded" | "failed" | "cancelled",
    error: RuntimeErrorInfo | null | undefined,
  ) => void;
}>;

/** Owns Runtime transport, snapshot/event synchronization, and reconnect timers. */
export class RuntimeLifecycleCoordinator {
  #selected_bootstrap: RuntimeBootstrap | undefined;
  #client: RuntimeClient | null = null;
  #event_abort: AbortController | null = null;
  #connect_promise: Promise<void> | null = null;
  #refresh_timer: number | null = null;
  #refresh_application_pending = false;
  #refresh_session_pending = false;
  #reconnect_timer: number | null = null;
  #reconnect_attempt = 0;
  #disposed = false;
  #lifecycle_generation = 0;
  #snapshot_reads = new Map<string, SnapshotRead>();
  #selection_disposer: (() => void) | null = null;
  #rebase_application_pending = false;

  constructor(private readonly dependencies: RuntimeLifecycleDependencies) {
    this.#observeSelection();
  }

  get client(): RuntimeClient | null {
    return this.#client;
  }

  connect(bootstrap?: RuntimeBootstrap): Promise<void> {
    if (this.#disposed) {
      this.#disposed = false;
      this.#connect_promise = null;
      this.#observeSelection();
    }
    if (this.#connect_promise) {
      return this.#connect_promise;
    }
    if (bootstrap) this.#selected_bootstrap = bootstrap;
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
    void this.#establishConnection(true, ++this.#lifecycle_generation);
  }

  prepareForNativeRuntimeMutation(kind: "stop" | "restart"): void {
    this.#cancelSnapshotReads();
    this.#lifecycle_generation += 1;
    this.#connect_promise = null;
    this.#event_abort?.abort();
    this.#event_abort = null;
    this.#client?.dispose();
    this.#client = null;
    this.#clearReconnectTimer();
    this.dependencies.connection.beginRuntimeMutation(kind);
    this.dependencies.projection.markStale();
    this.dependencies.mark_device_gateway_stale();
  }

  reconnectAfterNativeRuntimeMutation(bootstrap?: RuntimeBootstrap): Promise<void> {
    // 首页的空业务 Store 也会收到原生重启广播；没有用户选择时不得自动登录或初始化。
    if (!bootstrap && !this.#selected_bootstrap) return Promise.resolve();
    this.#reconnect_attempt = 0;
    return this.connect(this.#selected_bootstrap?.binding_id ? undefined : bootstrap);
  }

  #ownsSelection(): boolean {
    return this.dependencies.navigation.target_device === (this.dependencies.target_device ?? null);
  }

  #observeSelection(): void {
    this.#selection_disposer ??= reaction(
      () => [this.dependencies.navigation.target_device, this.dependencies.navigation.selected_session_id, this.dependencies.navigation.selected_child_task_id],
      () => {
        for (const [key, read] of this.#snapshot_reads) {
          if (read.owner && !this.#ownsSnapshot(read.owner)) this.#cancelSnapshotRead(key, read);
        }
      },
    );
  }

  #ownsSnapshot(owner: SnapshotOwner): boolean {
    return this.#ownsSelection()
      && this.dependencies.navigation.selected_session_id === owner.session_id
      && (owner.child_task_id === null || this.dependencies.navigation.selected_child_task_id === owner.child_task_id);
  }

  #cancelSnapshotRead(key: string, read: SnapshotRead): void {
    if (this.#snapshot_reads.get(key) !== read) return;
    this.#snapshot_reads.delete(key);
    read.events = [];
    if (read.owner) this.dependencies.projection.setConversationLoadState(read.owner.session_id, read.owner.child_task_id, { status: "idle", error: null });
    read.controller.abort(new DOMException("Snapshot selection changed", "AbortError"));
  }

  #cancelSnapshotReads(): void {
    for (const [key, read] of this.#snapshot_reads) this.#cancelSnapshotRead(key, read);
  }

  #snapshotKey(owner: SnapshotOwner | null): string {
    return owner ? JSON.stringify([owner.session_id, owner.child_task_id]) : "application";
  }

  /** 只读流程共用一次预算；取消覆盖 fetch、正文消费和退避，晚到 finally 不拥有新流程。 */
  #readSnapshot<T>(
    client: RuntimeClient,
    owner: SnapshotOwner | null,
    operation: (query: SnapshotQuery) => Promise<T>,
    apply: (value: T, events: readonly RuntimeEventEnvelope[]) => void,
  ): Promise<T> {
    const key = this.#snapshotKey(owner);
    const existing = this.#snapshot_reads.get(key);
    if (existing?.client === client) return existing.promise as Promise<T>;
    if (existing) this.#cancelSnapshotRead(key, existing);
    const read: SnapshotRead = { client, owner, controller: new AbortController(), promise: Promise.resolve(), refresh_pending: false, events: [], event_bytes: 0, overflow: false };
    this.#snapshot_reads.set(key, read);
    const current = () => !this.#disposed && this.#client === client && this.#snapshot_reads.get(key) === read
      && (!owner || this.#ownsSnapshot(owner));
    if (owner) this.dependencies.projection.setConversationLoadState(owner.session_id, owner.child_task_id, { status: "loading", error: null });
    const timeout = window.setTimeout(() => read.controller.abort(new RuntimeClientError("snapshot_timeout", "读取会话超时，请重试")), SNAPSHOT_BUDGET_MS);
    let succeeded = false;
    const requests = new Map<string, number>();
    const query: SnapshotQuery = async <TType extends SnapshotQueryType>(
      command: Extract<RuntimeCommand, { readonly type: TType }>,
    ): Promise<Extract<RuntimeCommandResult, { readonly type: TType }>> => {
      const request_key = JSON.stringify(command);
      for (let retry = 0; ; retry++) {
        read.controller.signal.throwIfAborted();
        const count = requests.get(request_key) ?? 0;
        if (count > SNAPSHOT_RETRY_DELAYS.length) throw new RuntimeClientError("snapshot_stale", "会话更新较多，请重试");
        requests.set(request_key, count + 1);
        try {
          return await waitForSnapshot(client.command<TType>(command, { signal: read.controller.signal }), read.controller.signal);
        } catch (error) {
          if (read.controller.signal.aborted) throw read.controller.signal.reason;
          if (!(error instanceof RuntimeClientError) || error.code !== "snapshot_busy") throw error;
          if (count >= SNAPSHOT_RETRY_DELAYS.length) throw new RuntimeClientError("snapshot_busy", "会话暂时繁忙，请重试");
          await snapshotDelay(SNAPSHOT_RETRY_DELAYS[retry]!, read.controller.signal);
        }
      }
    };
    read.promise = (async () => {
      try {
        for (let attempt = 0; ; attempt++) {
          read.controller.signal.throwIfAborted();
          read.events = [];
          read.event_bytes = 0;
          read.overflow = false;
          try {
            const value = await waitForSnapshot(operation(query), read.controller.signal);
            if (!current()) throw new DOMException("Snapshot selection changed", "AbortError");
            if (read.overflow) throw new RuntimeClientError("snapshot_stale", "会话更新较多，请重试");
            runInAction(() => {
              apply(value, read.events);
              if (owner) this.dependencies.projection.setConversationLoadState(owner.session_id, owner.child_task_id, { status: "idle", error: null });
            });
            succeeded = true;
            return value;
          } catch (error) {
            if (read.controller.signal.aborted) throw read.controller.signal.reason;
            // busy 只重试当前查询；generation／事件换代才重新读取组合基线。
            // 同一具体查询的尝试次数在整个流程内累计，不能层层叠加。
            const stale = error instanceof RuntimeClientError && error.code === "snapshot_stale";
            if (!stale || attempt >= SNAPSHOT_RETRY_DELAYS.length) throw error;
          }
        }
      } catch (error) {
        if (current() && !(error instanceof DOMException && error.name === "AbortError") && owner) {
          runInAction(() => {
            const message = displayError(error);
            this.dependencies.projection.setConversationLoadState(owner.session_id, owner.child_task_id, { status: "error", error: message });
            if (error instanceof RuntimeClientError && ["authentication_required", "login_context_changed"].includes(error.code)) {
              this.dependencies.connection.markDisconnected(message, error.code);
            }
          });
        }
        throw error;
      } finally {
        window.clearTimeout(timeout);
        read.events = [];
        if (this.#snapshot_reads.get(key) === read) {
          this.#snapshot_reads.delete(key);
          if (succeeded && read.refresh_pending) this.#scheduleRefresh(owner === null, owner !== null);
        }
      }
    })();
    return read.promise as Promise<T>;
  }

  async selectInitialSession(): Promise<void> {
    if (!this.#ownsSelection()) return;
    if (this.dependencies.target_device) {
      const selected = this.dependencies.navigation.selected_session_id;
      if (selected) await this.loadSession(selected);
      return;
    }
    const { navigation, projection } = this.dependencies;
    const application = projection.application;
    if (!application) {
      return;
    }
    const draft_key = navigation.selected_draft_key;
    const draft_workspace_id = draft_key ? workspaceForDraftKey(draft_key) : null;
    const draft_is_available = draft_key === "unbound" || Boolean(
      draft_workspace_id
      && application.workspaces.some((workspace) => (
        workspace.workspace_id === draft_workspace_id && workspace.lifecycle === "active"
      )),
    );
    if (draft_key && draft_is_available) {
      if (!navigation.workspace_expansion_initialized) {
        for (const workspace of application.workspaces) {
          navigation.ensureWorkspaceExpanded(workspace.workspace_id);
        }
      }
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
    const client = this.#client;
    if (!client) return;
    this.#rebase_application_pending ||= rebase_event_sequence;
    try {
      await this.#readSnapshot(client, null, (query) => this.#getApplication(client, query), (result, events) => {
        if (this.#rebase_application_pending) {
          const future = applicationEventsAfter(result.snapshot.observed_sequence, events);
          this.dependencies.projection.applyApplicationSnapshot(result.snapshot);
          this.#rebase_application_pending = false;
          for (const event of future) this.#applyEvent(event);
        } else this.dependencies.projection.refreshApplicationSnapshot(result.snapshot);
      });
    } catch (error) {
      if (!this.#disposed && this.#client === client && !(error instanceof DOMException && error.name === "AbortError")) {
        runInAction(() => this.dependencies.report_interaction_error(displayError(error)));
        if (this.#rebase_application_pending) {
          this.#cancelSnapshotReads();
          this.#event_abort?.abort();
          client.dispose();
          this.#client = null;
          runInAction(() => this.dependencies.connection.markDisconnected(displayError(error), error instanceof RuntimeClientError ? error.code : "transport_error"));
        }
      }
    }
  }

  async loadSession(session_id: SessionId): Promise<void> {
    const client = this.#client;
    const owner = { session_id, child_task_id: null };
    if (!client || !this.#ownsSnapshot(owner)) return;
    try {
      await this.#readSnapshot(client, owner,
        (query) => query({ type: "get_session_view", payload: { session_id } }),
        (result, events) => {
          const snapshot = result.payload.snapshot.value;
          if (snapshot.session.session_id !== session_id || snapshot.conversation.owner.type !== "main_session"
            || snapshot.conversation.owner.session_id !== session_id) {
            throw new RuntimeClientError("protocol_mismatch", "Runtime 返回了归属不匹配的会话快照。");
          }
          this.#applySessionResult(result.payload, events);
        },
      );
      const child_task_id = this.dependencies.navigation.selected_child_task_id;
      if (child_task_id && this.#ownsSnapshot(owner)) await this.loadChildTask(session_id, child_task_id);
    } catch {
      // 所属读取流程已发布可重试错误；选择取消只结束等待。
    }
  }

  async loadChildTask(session_id: SessionId, child_task_id: ChildTaskId): Promise<void> {
    const client = this.#client;
    const owner = { session_id, child_task_id };
    if (!client || !this.#ownsSnapshot(owner)) return;
    try {
      await this.#readSnapshot(client, owner,
        (query) => query({ type: "get_child_task_view", payload: owner }),
        (result, events) => {
          const snapshot = result.payload.snapshot.value;
          if (snapshot.task.task.session_id !== session_id || snapshot.task.task.child_task_id !== child_task_id
            || snapshot.conversation.owner.type !== "child_task" || snapshot.conversation.owner.session_id !== session_id
            || snapshot.conversation.owner.child_task_id !== child_task_id) {
            throw new RuntimeClientError("protocol_mismatch", "Runtime 返回了归属不匹配的子任务快照。");
          }
          this.#applyChildTaskResult(result.payload, events);
        },
      );
    } catch {
      // 单次正文读取失败不改变 Host 连接状态。
    }
  }

  dispose(): void {
    this.#cancelSnapshotReads();
    this.#selection_disposer?.();
    this.#selection_disposer = null;
    this.#disposed = true;
    this.#lifecycle_generation += 1;
    this.#connect_promise = null;
    this.#event_abort?.abort();
    this.#event_abort = null;
    this.#client?.dispose();
    this.#client = null;
    this.#clearReconnectTimer();
    if (this.#refresh_timer !== null) {
      window.clearTimeout(this.#refresh_timer);
      this.#refresh_timer = null;
    }
    this.#refresh_application_pending = false;
    this.#refresh_session_pending = false;
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
      const client = this.dependencies.create_client
        ? await this.dependencies.create_client()
        : await this.#createLocalClient(prepared_bootstrap);
      if (!this.#isActiveGeneration(generation)) { client.dispose(); return; }
      if (this.dependencies.target_device) {
        // 代理流没有目标 Host epoch；每次重建独立清空水位并获取该设备的新快照。
        this.dependencies.projection.resetForInstance();
        this.dependencies.live_execution.clear();
      }
      if (
        this.dependencies.connection.instance_id
        && this.dependencies.connection.instance_id !== client.instance_id
      ) {
        this.dependencies.projection.resetForInstance();
        if (this.#ownsSelection()) this.dependencies.navigation.clearConversationLocations();
        this.dependencies.live_execution.clear();
      }
      this.#cancelSnapshotReads();
      this.#event_abort?.abort();
      this.#client?.dispose();
      this.#client = client;
      this.#rebase_application_pending = false;
      try {
        await client.waitUntilReady((health) => {
          if (this.#isActiveGeneration(generation) && this.#client === client) this.dependencies.connection.markStartup(health);
        });
      } catch (error) {
        const failure = normalizeFailure(error);
        if (["authentication_required", "login_context_changed", "component_mismatch", "center_protocol_incompatible"].includes(failure.code)) throw error;
        // 初始化失败只由用户显式重试，不能让通用断线重连反复执行初始化。
        throw new RuntimeClientError("runtime_startup_failed", failure.message);
      }
      if (!this.#isActiveGeneration(generation) || this.#client !== client) { client.dispose(); return; }
      const event_abort = new AbortController();
      this.#event_abort = event_abort;
      const buffered_events: RuntimeEventEnvelope[] = [];
      let buffered_bytes = 0;
      let snapshot_loaded = false;
      let buffer_overflow = false;
      const stream = await client.connectEvents(
        {
          onEvent: (event) => {
            if (!this.#isActiveGeneration(generation) || this.#client !== client) return;
            if (!snapshot_loaded) {
              if (!buffer_overflow) {
                const bytes = new TextEncoder().encode(JSON.stringify(event)).byteLength;
                if (buffered_events.length < MAX_SNAPSHOT_EVENTS && buffered_bytes + bytes <= MAX_SNAPSHOT_EVENT_BYTES) {
                  buffered_events.push(event);
                  buffered_bytes += bytes;
                } else { buffer_overflow = true; buffered_events.length = 0; }
              }
            } else {
              this.#applyEvent(event);
            }
          },
          onDeviceGatewayEvent: () => { if (this.#isActiveGeneration(generation) && this.#client === client) this.dependencies.refresh_device_gateway(); },
          onGap: () => {
            if (this.#isActiveGeneration(generation) && this.#client === client) {
              if (snapshot_loaded) this.#handleStreamGap();
              else buffer_overflow = true;
            }
          },
        },
        event_abort.signal,
      );
      if (!this.#isActiveGeneration(generation) || this.#client !== client) { event_abort.abort(); client.dispose(); return; }
      let stream_closed = false;
      void stream.closed.then(() => { stream_closed = true; }, () => { stream_closed = true; });
      const application = await this.#readSnapshot(client, null, async (query) => {
        buffered_events.length = 0;
        buffered_bytes = 0;
        buffer_overflow = false;
        const result = await this.#getApplication(client, query);
        if (buffer_overflow) throw new RuntimeClientError("snapshot_stale", "事件同步积压，请重试");
        applicationEventsAfter(result.snapshot.observed_sequence, buffered_events);
        return result;
      }, () => {});
      if (stream_closed) throw new Error("事件连接已中断。");
      if (buffer_overflow) throw new Error("事件同步积压，请重新连接。");
      if (!this.#isActiveGeneration(generation) || this.#client !== client) { event_abort.abort(); client.dispose(); return; }
      runInAction(() => this.dependencies.projection.applyApplicationSnapshot(application.snapshot));
      snapshot_loaded = true;
      for (const event of applicationEventsAfter(application.snapshot.observed_sequence, buffered_events)) this.#applyEvent(event);
      runInAction(() => {
        this.dependencies.connection.markConnected(
          client.instance_id,
          client.capabilities,
          client.address,
        );
        this.#reconnect_attempt = 0;
      });
      this.dependencies.refresh_device_gateway();
      await this.selectInitialSession();
      void stream.closed.then(
        () => this.#handleStreamClosed(event_abort),
        () => this.#handleStreamClosed(event_abort),
      );
    } catch (error: unknown) {
      if (!this.#isActiveGeneration(generation)) {
        return;
      }
      this.#event_abort?.abort();
      this.#client?.dispose();
      this.#client = null;
      const failure = normalizeFailure(error);
      runInAction(() => {
        if (failure.code === "component_mismatch") {
          this.dependencies.connection.markComponentMismatch(failure.message, failure.code);
        } else {
          this.dependencies.connection.markDisconnected(failure.message, failure.code);
        }
        if (this.dependencies.target_device) { this.dependencies.projection.resetForInstance(); this.dependencies.live_execution.clear(); }
        this.dependencies.projection.markStale();
        this.dependencies.mark_device_gateway_stale();
      });
      this.#scheduleReconnect();
    }
  }

  async #createLocalClient(prepared_bootstrap?: RuntimeBootstrap): Promise<RuntimeClient> {
    const selected = this.#selected_bootstrap;
    if (!selected && this.dependencies.require_selected_target) throw new RuntimeClientError("authentication_required", "请在 Runtime 连接设置中选择目标并登录。");
    const bootstrap = prepared_bootstrap ?? await (selected?.binding_id ? refreshRuntimeConnection(selected) : bootstrapRuntime());
    // Cookie 可能被另一标签页替换；新身份必须由应用入口重建整棵 Store。
    if (this.#client && (this.#client.login_context ?? null) !== (bootstrap.login_context ?? null)) {
      throw new RuntimeClientError("login_context_changed", "登录账号已变化，正在重新确认身份。");
    }
    const client = new RuntimeClient(bootstrap, (code) => {
      if (this.#client !== client) return;
      runInAction(() => {
        this.dependencies.connection.markDisconnected("登录状态已变化，请重新确认身份。", code);
        this.dependencies.projection.resetForInstance();
        this.dependencies.navigation.clearConversationLocations();
        this.dependencies.live_execution.clear();
      });
    });
    return client;
  }


  async #getApplication(client: RuntimeClient, query: SnapshotQuery): Promise<GetApplicationSnapshotResult> {
    // 普通刷新、gap 和同实例重连都一次读取已加载前缀，排序与事件水位来自同一快照。
    const previous = this.dependencies.projection.application;
    const request: GetApplicationSnapshotRequest = previous ? {
      active_sessions_limit: Math.max(100, previous.active_sessions_next_offset ?? previous.active_sessions.length),
      archived_sessions_limit: Math.max(100, previous.archived_sessions_next_offset ?? previous.archived_sessions.length),
    } : {};
    const result = await query({ type: "get_application_snapshot", payload: request });
    if (this.#disposed || this.#client !== client) return result.payload;
    const selected_id = this.#ownsSelection() ? this.dependencies.navigation.selected_session_id : null;
    const application = result.payload.snapshot.value;
    if (selected_id && !application.active_sessions.some((session) => session.session_id === selected_id)) {
      try {
        const selected = await query({ type: "get_session", payload: { session_id: selected_id } });
        if (this.#disposed || this.#client !== client) return result.payload;
        const session = selected.payload.session;
        if (session.lifecycle === "active" && (!session.workspace_id || application.workspaces.some(
          (workspace) => workspace.workspace_id === session.workspace_id && workspace.lifecycle === "active",
        ))) application.active_sessions.push(session);
      } catch (error: unknown) {
        if (!(error instanceof RuntimeClientError && error.code === "session_not_found")) throw error;
      }
    }
    return result.payload;
  }

  #applySessionResult(result: GetSessionViewResult, events: readonly RuntimeEventEnvelope[] = []): void {
    const future = events.filter((event) => event.sequence > result.snapshot.observed_sequence);
    assertSnapshotGeneration(result.snapshot.value.conversation.owner, result.snapshot.value.conversation.generation, future);
    if (this.dependencies.projection.applySessionSnapshot(result.snapshot)) {
      this.dependencies.live_execution.rebaseSession(result.snapshot.value, result.snapshot.observed_at_ms, result.snapshot.observed_sequence, future);
    }
  }

  #applyChildTaskResult(result: GetChildTaskViewResult, events: readonly RuntimeEventEnvelope[] = []): void {
    const future = events.filter((event) => event.sequence > result.snapshot.observed_sequence);
    assertSnapshotGeneration(result.snapshot.value.conversation.owner, result.snapshot.value.conversation.generation, future);
    if (this.dependencies.projection.applyChildTaskSnapshot(result.snapshot)) {
      this.dependencies.live_execution.rebaseChildTask(result.snapshot.value, result.snapshot.observed_at_ms, future);
    }
  }

  #bufferSnapshotEvent(envelope: RuntimeEventEnvelope, application_only = false): void {
    let bytes: number | undefined;
    for (const read of this.#snapshot_reads.values()) {
      if (read.overflow || (application_only ? read.owner !== null : !read.owner || !snapshotEventMatches(envelope.event, read.owner))) continue;
      bytes ??= new TextEncoder().encode(JSON.stringify(envelope)).byteLength;
      if (read.events.length >= MAX_SNAPSHOT_EVENTS || read.event_bytes + bytes > MAX_SNAPSHOT_EVENT_BYTES) {
        read.overflow = true;
        read.events = [];
      } else {
        read.event_bytes += bytes;
        read.events.push(envelope);
      }
    }
  }

  #eventIsInOwnerSnapshot({ event, sequence }: RuntimeEventEnvelope): boolean {
    const projection = this.dependencies.projection;
    if (event.type === "child_task_event") {
      return ["created", "started", "finished"].includes(event.event.type)
        && sequence <= Math.max(projection.sessionSnapshotSequence(event.session_id), projection.childSnapshotSequence(event.session_id, event.child_task_id));
    }
    if (event.type === "step_committed" || event.type === "conversation_committed") {
      return sequence <= (event.owner.type === "main_session" ? projection.sessionSnapshotSequence(event.owner.session_id)
        : projection.childSnapshotSequence(event.owner.session_id, event.owner.child_task_id));
    }
    return "run_id" in event && "session_id" in event && sequence <= projection.sessionSnapshotSequence(event.session_id);
  }

  #applyEvent(envelope: RuntimeEventEnvelope): void {
    if (this.#rebase_application_pending) {
      this.#bufferSnapshotEvent(envelope, true);
      return;
    }
    const accepted = this.dependencies.projection.acceptEvent(envelope);
    if (accepted === "ignored") {
      return;
    }
    if (accepted === "gap") {
      this.#handleStreamGap();
      return;
    }
    this.#bufferSnapshotEvent(envelope);
    if (!this.#eventIsInOwnerSnapshot(envelope)) this.dependencies.live_execution.buffer(envelope);
    this.dependencies.on_event?.(envelope);
    const event = envelope.event;
    if (event.type === "session_title_generation_finished") {
      this.dependencies.on_title_generation_finished(
        event.session_id,
        event.trigger,
        event.outcome,
        event.error,
      );
    }
    if (event.type === "session_deleted") {
      const deleted_selection = this.#ownsSelection() && this.dependencies.navigation.selected_session_id === event.session_id;
      this.dependencies.navigation.clearConversationLocations(event.session_id, this.dependencies.target_device ?? null);
      if (deleted_selection) {
        this.dependencies.navigation.selectSession(null, false);
      }
      void this.loadApplication().then(() => deleted_selection ? this.selectInitialSession() : undefined);
      return;
    }
    if (
      event.type === "config_changed"
      || event.type === "mcp_registry_changed"
      || event.type === "skill_settings_changed"
      || event.type === "workspace_changed"
      || event.type === "session_created"
      || event.type === "session_changed"
      || event.type === "session_compaction_started"
      || event.type === "session_compaction_finished"
      || event.type === "session_title_generation_started"
      || event.type === "session_title_generation_finished"
      || event.type === "conversation_committed"
      || event.type === "run_started"
      || event.type === "run_finished"
      || (event.type === "child_task_event" && ["created", "started", "finished"].includes(event.event.type))
    ) {
      const session_id = sessionIdForRefresh(event);
      const selected = this.dependencies.navigation.selected_session_id;
      const workspace_id = selected ? this.dependencies.projection.session_views.get(selected)?.session.workspace_id
        ?? this.dependencies.projection.application?.active_sessions.find((session) => session.session_id === selected)?.workspace_id : null;
      const shared = event.type === "config_changed" || event.type === "mcp_registry_changed"
        || event.type === "skill_settings_changed" || (event.type === "workspace_changed" && event.workspace_id === workspace_id);
      this.#scheduleRefresh(true, shared || session_id === this.dependencies.navigation.selected_session_id);
    } else {
      const refresh_session_id = sessionIdForRefresh(event);
      if (refresh_session_id && refresh_session_id === this.dependencies.navigation.selected_session_id) {
        this.#scheduleRefresh(false, true);
      }
    }
  }

  #scheduleRefresh(include_application: boolean, include_session: boolean): void {
    const application_read = this.#snapshot_reads.get("application");
    if (include_application && application_read) {
      application_read.refresh_pending = true;
      include_application = false;
    }
    const session_id = this.#ownsSelection() ? this.dependencies.navigation.selected_session_id : null;
    const session_read = session_id ? this.#snapshot_reads.get(this.#snapshotKey({ session_id, child_task_id: null })) : null;
    if (include_session && session_read) {
      session_read.refresh_pending = true;
      include_session = false;
    }
    this.#refresh_application_pending ||= include_application;
    this.#refresh_session_pending ||= include_session;
    if (this.#refresh_timer !== null || (!this.#refresh_application_pending && !this.#refresh_session_pending)) return;
    this.#refresh_timer = window.setTimeout(() => {
      this.#refresh_timer = null;
      const refresh_application = this.#refresh_application_pending;
      const refresh_session = this.#refresh_session_pending;
      this.#refresh_application_pending = false;
      this.#refresh_session_pending = false;
      if (refresh_application) {
        void this.loadApplication();
      }
      const session_id = this.#ownsSelection() ? this.dependencies.navigation.selected_session_id : null;
      if (refresh_session && session_id) {
        void this.loadSession(session_id);
      }
    }, 80);
  }

  #handleStreamGap(): void {
    if (this.dependencies.target_device) { this.retryConnection(); return; }
    if (this.#rebase_application_pending) return;
    this.#rebase_application_pending = true;
    for (const read of this.#snapshot_reads.values()) { read.overflow = true; read.events = []; }
    this.dependencies.live_execution.invalidateForGap();
    this.dependencies.projection.markStale();
    this.dependencies.mark_device_gateway_stale();
    this.dependencies.refresh_device_gateway();
    void this.loadApplication(true);
    const session_id = this.#ownsSelection() ? this.dependencies.navigation.selected_session_id : null;
    if (session_id) {
      void this.loadSession(session_id);
    }
  }

  #handleStreamClosed(owner: AbortController): void {
    if (this.#disposed || owner.signal.aborted || this.#event_abort !== owner) {
      return;
    }
    this.#cancelSnapshotReads();
    runInAction(() => {
      this.dependencies.connection.markDisconnected("Runtime 连接已中断，正在尝试恢复。");
      if (this.dependencies.target_device) { this.dependencies.projection.resetForInstance(); this.dependencies.live_execution.clear(); }
      this.dependencies.projection.markStale();
      this.dependencies.mark_device_gateway_stale();
    });
    this.#scheduleReconnect();
  }

  #scheduleReconnect(): void {
    if (
      this.#disposed
      || this.#reconnect_attempt >= MAX_AUTOMATIC_RECONNECTS
      || this.dependencies.connection.state === "component_mismatch"
      || ["authentication_required", "login_context_changed", "runtime_initialization_failed", "storage_unavailable", "center_protocol_incompatible"].includes(this.dependencies.connection.last_error_code ?? "")
      || this.dependencies.connection.last_error_code === "runtime_startup_failed"
      || ["snapshot_busy", "snapshot_stale", "snapshot_timeout"].includes(this.dependencies.connection.last_error_code ?? "")
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

function waitForSnapshot<T>(operation: Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const cancel = () => reject(signal.reason);
    signal.addEventListener("abort", cancel, { once: true });
    if (signal.aborted) cancel();
    operation.then(resolve, reject).finally(() => signal.removeEventListener("abort", cancel));
  });
}

function snapshotEventMatches(event: RuntimeEvent, owner: SnapshotOwner): boolean {
  if (owner.child_task_id) {
    if (event.type === "child_task_event") return event.session_id === owner.session_id && event.child_task_id === owner.child_task_id;
    if (event.type === "conversation_committed" || event.type === "step_committed") return event.owner.type === "child_task"
      && event.owner.session_id === owner.session_id && event.owner.child_task_id === owner.child_task_id;
    if (event.type === "approval_requested") return event.approval.session_id === owner.session_id && event.approval.child_task_id === owner.child_task_id;
    return false;
  }
  if ("session_id" in event) return event.session_id === owner.session_id;
  if ("session" in event) return event.session.session_id === owner.session_id;
  if (event.type === "conversation_committed" || event.type === "step_committed") return event.owner.session_id === owner.session_id;
  return event.type === "approval_requested" && event.approval.session_id === owner.session_id;
}

function applicationEventsAfter(sequence: number, events: readonly RuntimeEventEnvelope[]): RuntimeEventEnvelope[] {
  const future: RuntimeEventEnvelope[] = [];
  for (const event of events) {
    if (event.sequence <= sequence) continue;
    if (event.sequence !== sequence + 1) throw new RuntimeClientError("snapshot_stale", "事件同步存在缺口，请重试");
    sequence = event.sequence;
    future.push(event);
  }
  return future;
}

function assertSnapshotGeneration(owner: ConversationOwner, generation: number, future: readonly RuntimeEventEnvelope[]): void {
  for (const { event } of future) {
    if ((event.type === "conversation_committed" || event.type === "step_committed") && event.generation !== generation
      && event.owner.session_id === owner.session_id && event.owner.type === owner.type
      && (event.owner.type !== "child_task" || (owner.type === "child_task" && event.owner.child_task_id === owner.child_task_id))) {
      throw new RuntimeClientError("snapshot_stale", "会话内容已变化，请重试");
    }
  }
}

function snapshotDelay(delay_ms: number, signal: AbortSignal): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const cancel = () => { window.clearTimeout(timer); reject(signal.reason); };
    const timer = window.setTimeout(() => { signal.removeEventListener("abort", cancel); resolve(); }, delay_ms);
    signal.addEventListener("abort", cancel, { once: true });
    if (signal.aborted) cancel();
  });
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
  return error instanceof Error ? error.message : "无法连接当前 Runtime。";
}

function sessionIdForRefresh(event: RuntimeEvent): SessionId | null {
  switch (event.type) {
    case "conversation_committed":
    case "step_committed":
      return event.owner.session_id;
    case "child_task_event":
      return ["created", "started", "finished", "usage_updated"].includes(event.event.type) ? event.session_id : null;
    case "session_variant_changed":
    case "session_approval_mode_changed":
      return event.session.session_id;
    case "session_created":
      return event.session.session_id;
    case "session_changed":
      return event.session_id;
    case "approval_requested":
      return event.approval.session_id;
    case "queue_changed":
    case "session_compaction_started":
    case "session_compaction_finished":
    case "session_title_generation_started":
    case "session_title_generation_finished":
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
