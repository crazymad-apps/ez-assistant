import { DesktopPinsStore, pinReference, type DesktopPins, type DesktopPin } from "./DesktopPinsStore";
import { parseDesktopWorkspace, type DesktopWorkspaceSnapshot } from "../features/resource-workspace/desktopWorkspaceSnapshot";
import { ClientResources } from "../runtime-client/ClientResources";
import type { ResourceWorkspaceSnapshot } from "../features/resource-workspace/resourceWorkspaceSnapshot";
import { action, makeObservable, observable, reaction, runInAction, type IReactionDisposer } from "mobx";
import type {
  AgentShellSettings,
  AgentVariant,
  AttachmentId,
  ApprovalDecision,
  ApprovalId,
  ApprovalMode,
  ChildTaskId,
  ClearSessionResult,
  CompactSessionOutcome,
  ConversationOwner,
  ConversationHistoryHit,
  GoalId,
  InputId,
  ListMcpServerOptionsRequest,
  SkillManagementSnapshot,
  McpServerKey,
  McpServerOptionSnapshot,
  MessageId,
  MessageFeedback,
  ModelSelection,
  QuotedTextSnapshot,
  PrepareDeleteSessionResult,
  RecallNavigationTarget,
  RunId,
  SessionMaterializationManifest,
  SessionMaterializationResult,
  SessionId,
  SessionResourceLocator,
  SessionCommand,
  ToolCallId,
  ToolDetailSnapshot,
  SystemContextSnapshot,
  SubmitInputMode,
  WorkspaceId,
  WorkspaceSummary,
} from "@ez-assistant/protocol";
import { viewingSnapshot, loadDesktopPreferences, saveDesktopPreferences } from "../native-bridge/desktopPreferences";
import {
  copySessionResourcePath as copyNativeSessionResourcePath,
  NativeResourceFailure,
  openSessionResourceInSystem as openNativeSessionResourceInSystem,
} from "../native-bridge/nativeResource";
import { RuntimeClientError, type RuntimeClient } from "../runtime-client/RuntimeClient";
import type { RuntimeBootstrap } from "../native-bridge/runtimeBootstrap";
import { NetworkDevicesStore } from "./NetworkDevicesStore";
import { ConnectionStore } from "./ConnectionStore";
import { ComposerQuoteStore } from "./ComposerQuoteStore";
import { ConversationSearchStore } from "./ConversationSearchStore";
import { DesktopLifecycleStore } from "./DesktopLifecycleStore";
import { DeviceGatewayStore } from "./DeviceGatewayStore";
import { LiveExecutionStore } from "./LiveExecutionStore";
import { MemorySettingsStore } from "./MemorySettingsStore";
import { NavigationStore, type ConversationLocation } from "./NavigationStore";
import {
  isDraftCustomized,
  draftKeyForWorkspace,
  NewSessionDraftStore,
  type NewSessionDraft,
  type NewSessionDraftKey,
} from "./NewSessionDraftStore";
import { RuntimeLifecycleCoordinator } from "./RuntimeLifecycleCoordinator";
import { RuntimeProjectionStore } from "./RuntimeProjectionStore";
import { RunInteractionController } from "./RunInteractionController";
import { SessionManagementController } from "./SessionManagementController";
import { SettingsStore } from "./SettingsStore";
import { TransientFocusStore } from "./TransientFocusStore";
import { ResourceWorkspaceStore } from "../features/resource-workspace/ResourceWorkspaceStore";

import { FeedbackCenterStore } from "../features/feedback-center/FeedbackCenterStore";
import { FeedbackStore } from "../features/feedback/FeedbackStore";
import { NotificationStore, PendingNotificationStore, resolveNotificationContent, type NotificationTarget } from "../features/notifications";

export class RootStore {
  readonly notifications = new NotificationStore({
    scope: () => this.notificationScope(),
    navigate: (target) => this.openNotificationTarget(target),
    acceptsScope: (scope) => this.notificationDevice(scope) !== undefined,
    resolveContent: (notification) => resolveNotificationContent(this.notificationClient(notification.target.scope), notification),
  });
  readonly feedback_center = new FeedbackCenterStore({ client: () => this.#local_runtime.client, scope: () => this.notificationScopeFor(this.#local_runtime.client) });
  readonly feedback = new FeedbackStore({ client: () => this.#runtime.client, scope: () => this.notificationScope() });
  readonly pending_notifications = new PendingNotificationStore({
    client: () => this.#runtime.client,
    selectedSession: () => this.navigation.selected_session_id,
    title: (id) => [...(this.projection.application?.active_sessions ?? []), ...(this.projection.application?.archived_sessions ?? [])]
      .find((session) => session.session_id === id)?.title,
    feedback: this.feedback,
  });
  readonly local_connection = new ConnectionStore();
  readonly local_composer_quotes = new ComposerQuoteStore();
  readonly transient_focus = new TransientFocusStore();
  readonly local_projection = new RuntimeProjectionStore();
  readonly local_live_execution = new LiveExecutionStore();
  readonly navigation = new NavigationStore();
  readonly local_resource_workspace = new ResourceWorkspaceStore(() => this.#local_runtime.client, (id) => this.network.devices.some((device) => device.id === id && device.desktop_control));
  readonly local_files: ClientResources;
  readonly network: NetworkDevicesStore;
  directory_picker: Readonly<{ files: ClientResources; initial_path: string | null; resolve: (path: string | null) => void }> | null = null;
  readonly local_new_session_drafts = new NewSessionDraftStore();
  readonly conversation_search = new ConversationSearchStore();
  readonly settings: SettingsStore;
  readonly memory_settings: MemorySettingsStore;
  readonly device_gateway: DeviceGatewayStore;
  readonly desktop_lifecycle: DesktopLifecycleStore;
  pending_session_action = false;
  pending_workspace_action = false;
  workspace_editor: Readonly<
    | { device: string | null; mode: "create"; primary_directory: string | null }
    | { device: string | null; mode: "edit"; workspace_id: WorkspaceId }
  > | null = null;
  composer_pending = false;
  interaction_error: string | null = null;
  pending_queue_input_id: InputId | null = null;
  pending_approval_id: ApprovalId | null = null;
  pending_proxy_session_id: SessionId | null = null;
  pending_compaction_session_id: SessionId | null = null;
  pending_compaction_cancel_session_id: SessionId | null = null;
  session_notice: Readonly<{
    session_id: SessionId;
    tone: "success" | "warning" | "neutral";
    message: string;
    action?: "retry_title";
  }> | null = null;

  readonly #local_runtime: RuntimeLifecycleCoordinator;
  readonly #run_interaction: RunInteractionController;
  readonly #session_management: SessionManagementController;
  #disposed = false;
  readonly #draft_skill_requests = new Map<NewSessionDraftKey, object>();
  readonly #draft_mcp_requests = new Map<NewSessionDraftKey, object>();
  #preferences_save_timer: number | null = null;
  #preferences_initialization: Promise<void> | null = null;
  #initial_connection: Promise<void> | null = null;
  #pending_snapshot: ResourceWorkspaceSnapshot | null = null;
  readonly pins = new DesktopPinsStore((pins) => this.flushPreferences(pins));
  #workspace_snapshot: DesktopWorkspaceSnapshot | null = null;
  #preferences_read_failed = false;
  #preferences_ready = false;
  #preferences_pending: Promise<void> = Promise.resolve();
  #last_saved_preferences = "";
  #resource_snapshot_disposer: IReactionDisposer;
  #resource_restore: Promise<void> | null = null;
  readonly #save_view_state = () => this.#schedulePreferencesSave();
  readonly #flush_view_state = () => { void this.flushPreferences().catch(() => undefined); };
  #conversation_search_revision = 0;
  #title_notice_timer: number | null = null;
  #runtime_state_disposer: IReactionDisposer;
  #resource_scope_disposer: IReactionDisposer;
  #provider_defaults_disposer: IReactionDisposer;
  #pending_notifications_disposer: IReactionDisposer;
  #feedback_disposer: IReactionDisposer;
  #device_navigation_disposer: IReactionDisposer;
  #feedback_navigator_disposer: () => void;

  constructor(private readonly target_options: Readonly<{ target_kind?: "local" | "remote"; target_address?: string; restore_resources?: boolean; flush_application_preferences?: () => Promise<void> }> = {}) {
    this.#local_runtime = new RuntimeLifecycleCoordinator({
      on_event: (event) => {
        this.notifications.ingest(event, this.notificationScopeFor(this.#local_runtime.client));
        if (this.navigation.target_device) return;
        this.feedback.setScope(this.notificationScope());
        this.feedback.ingest(event);
        this.pending_notifications.setConnection(this.notificationScope(), this.local_connection.state === "connected" && !this.local_projection.is_stale);
        this.pending_notifications.ingest(event);
      },
      require_selected_target: this.target_options.target_kind !== undefined,
      connection: this.local_connection,
      live_execution: this.local_live_execution,
      navigation: this.navigation,
      projection: this.local_projection,
      report_interaction_error: (message) => {
        this.interaction_error = message;
      },
      refresh_device_gateway: () => this.device_gateway.scheduleRefresh(),
      mark_device_gateway_stale: () => this.device_gateway.markStale(),
      on_title_generation_finished: (session_id, trigger, outcome, error) => {
        if (trigger !== "manual") return;
        if (this.#title_notice_timer !== null) {
          window.clearTimeout(this.#title_notice_timer);
          this.#title_notice_timer = null;
        }
        let notice: typeof this.session_notice = null;
        if (outcome === "succeeded") {
          notice = { session_id, tone: "success", message: "标题已更新。" };
        } else if (outcome === "failed") {
          let message = "标题生成失败。";
          if (error?.code === "timeout") {
            message = "标题生成超时，请重试。";
          } else if (error?.code === "model_execution_failed") {
            message = "标题模型调用失败，请重试。";
          }
          notice = { session_id, tone: "warning", message, action: "retry_title" };
        }
        runInAction(() => {
          this.session_notice = notice;
        });
        if (outcome === "succeeded") {
          this.#title_notice_timer = window.setTimeout(() => {
            this.#title_notice_timer = null;
            runInAction(() => this.clearSessionNotice(session_id));
          }, 1600);
        }
      },
    });
    this.network = new NetworkDevicesStore(() => this.#local_runtime.client, this.navigation,
      (message) => this.showInteractionError(message), () => this.local_files.desktop,
      (device, event) => {
        this.notifications.ingest(event, this.notificationScopeFor(this.network.contexts.get(device)?.runtime.client ?? null));
        if (this.navigation.target_device !== device) return;
        this.feedback.setScope(this.notificationScope());
        this.feedback.ingest(event);
        this.pending_notifications.setConnection(this.notificationScope(), this.connection.state === "connected" && !this.projection.is_stale);
        this.pending_notifications.ingest(event);
      }, () => this.workspace_editor?.device ?? null, {
        restore: (device, context) => {
          const saved = this.#workspace_snapshot?.devices.find((item) => item.device === device);
          if (!saved) return;
          runInAction(() => { context.sidebar.expanded_workspaces.replace(saved.expanded); });
          if (saved.defaults) context.drafts.applyDefaults(saved.defaults.approval, saved.defaults.model);
        },
        retain: () => { this.#workspace_snapshot = this.captureDesktopWorkspace(); },
      });
    this.#device_navigation_disposer = reaction(() => this.navigation.target_device, () => {
      this.conversation_search.reset();
      this.#conversation_search_revision++;
      runInAction(() => { this.interaction_error = null; this.session_notice = null; });
    });
    this.#pending_notifications_disposer = reaction(
      () => ({ state: this.connection.state, scope: this.notificationScope(), stale: this.projection.is_stale }),
      ({ state, scope, stale }) => {
        this.feedback.setScope(scope);
        this.feedback_center.setScope(this.notificationScopeFor(this.#local_runtime.client));
        this.pending_notifications.setConnection(scope, state === "connected" && !stale);
      },
      { fireImmediately: true },
    );
    this.#feedback_disposer = reaction(
      () => {
        const session_id = this.navigation.selected_session_id;
        return { state: this.connection.state, scope: this.notificationScope(), session_id,
          view: session_id ? this.projection.session_views.get(session_id) : undefined };
      },
      ({ state, scope, session_id }) => {
        this.feedback.setScope(scope);
        this.feedback_center.setScope(this.notificationScopeFor(this.#local_runtime.client));
        if (state === "connected" && session_id) void this.feedback.refresh(session_id);
      },
      { fireImmediately: true },
    );
    this.#feedback_navigator_disposer = this.notifications.registerFeedbackNavigator(async (target) => {
      if (!(await this.openNotificationTarget(target))) return;
      await this.feedback.refresh(target.owner.session_id);
      if (this.feedback.errors.has(target.owner.session_id)) throw new Error("问题尚未同步，请重试。");
      if (target.scope !== this.notificationScope() || this.navigation.selected_session_id !== target.owner.session_id) return;
      const request = this.feedback.requests.get(target.request_id);
      if (request?.status === "pending") this.feedback.open(request);
      else this.showInteractionError("这个问题已处理或已失效，当前显示会话最新状态。");
    });
    this.local_files = new ClientResources(() => this.#local_runtime.client, this.target_options.target_kind !== "remote");
    this.device_gateway = new DeviceGatewayStore({
      get_client: () => this.#local_runtime.client,
      refresh_application: () => this.#local_runtime.loadApplication(),
    });
    this.desktop_lifecycle = new DesktopLifecycleStore({
      resources: this.local_resource_workspace,
      get_application: () => this.target_options.target_kind === "remote" ? null : this.local_projection.application,
      is_local_target: () => this.target_options.target_kind !== "remote",
      prepare_runtime_mutation: (kind) => this.#local_runtime.prepareForNativeRuntimeMutation(kind),
      reconnect_runtime: (bootstrap) => this.#local_runtime.reconnectAfterNativeRuntimeMutation(bootstrap),
      mark_runtime_stopped: () => this.local_connection.markRuntimeStopped(),
      save_preferences: () => this.#schedulePreferencesSave(),
      flush_preferences: async () => {
        await this.flushPreferences();
        await this.target_options.flush_application_preferences?.();
      },
    });
    this.#resource_scope_disposer = reaction(
      () => ({ resources: this.reading_resources, scope: this.reading_scope }),
      ({ resources, scope }) => resources.selectScope(scope),
      { fireImmediately: true },
    );
    this.#resource_snapshot_disposer = reaction(
      () => this.captureDesktopWorkspace(),
      () => this.#schedulePreferencesSave(),
    );
    // 原生滚动事件以 capture 接住嵌套查看器；查看状态仍由各页面持有。
    window.addEventListener("scroll", this.#save_view_state, true);
    window.addEventListener("pointerup", this.#save_view_state);
    window.addEventListener("wheel", this.#save_view_state, { capture: true, passive: true });
    window.addEventListener("keyup", this.#save_view_state);
    window.addEventListener("blur", this.#flush_view_state);
    window.addEventListener("pagehide", this.#flush_view_state);
    window.addEventListener("pagehide", this.local_resource_workspace.disconnectTerminals);
    this.desktop_lifecycle.start();
    this.#runtime_state_disposer = reaction(
      () => this.local_connection.state,
      (state) => {
        this.desktop_lifecycle.syncRuntimeState(state);
        if (state === "connected") void this.#restoreResourceSnapshot();
      },
      { fireImmediately: true },
    );
    this.settings = new SettingsStore({
      get_client: () => this.#local_runtime.client,
      get_permission_context: () => {
        const session_id = this.navigation.target_device ? null : this.navigation.selected_session_id;
        const session = this.local_projection.application?.active_sessions.find(
          (candidate) => candidate.session_id === session_id,
        );
        return {
          session_id,
          workspace_id: session?.workspace_id ?? null,
        };
      },
      refresh_application: () => this.#local_runtime.loadApplication(),
    });
    this.memory_settings = new MemorySettingsStore({
      get_client: () => this.#local_runtime.client,
    });
    const current_resources = () => this.resource_workspace;
    const current_files = () => this.files;
    const current_connection = () => this.connection;
    const current_runtime = () => this.#runtime;
    this.#session_management = new SessionManagementController({
      get resources() { return current_resources(); },
      get files() { return current_files(); },
      get connection() { return current_connection(); },
      navigation: this.navigation,
      get runtime() { return current_runtime(); },
      save_preferences: () => this.#schedulePreferencesSave(),
      select_draft: (workspace_id) => this.openNewSessionDraft(workspace_id, this.navigation.target_device),
      select_session: (session_id) => this.selectSession(session_id),
      state: this,
    });
    this.#run_interaction = new RunInteractionController({
      get connection() { return current_connection(); },
      get runtime() { return current_runtime(); },
      state: this,
    });
    this.#provider_defaults_disposer = reaction(
      () => this.local_projection.application?.providers.map((provider) => provider.provider_instance_id).join("\n") ?? null,
      () => this.#discardUnavailableDefaultModel(),
    );
    makeObservable(this, {
      pending_session_action: observable,
      pending_workspace_action: observable,
      workspace_editor: observable,
      directory_picker: observable,
      finishDirectorySelection: action,
      composer_pending: observable,
      interaction_error: observable,
      pending_queue_input_id: observable,
      pending_approval_id: observable,
      pending_proxy_session_id: observable,
      pending_compaction_session_id: observable,
      pending_compaction_cancel_session_id: observable,
      session_notice: observable,
      connect: action,
      retryConnection: action,
      initializePreferences: action,
      selectSession: action,
      openChildTask: action,
      closeChildTask: action,
      navigateBack: action,
      navigateForward: action,
      searchConversationHistory: action,
      selectConversationHistoryHit: action,
      openConversationHistoryHit: action,
      openRecallNavigationTarget: action,
      locateTextQuoteSource: action,
      cancelChildTask: action,
      openNewSessionDraft: action,
      switchDraftDevice: action,
      moveNewSessionDraft: action,
      clearNewSessionDraft: action,
      materializeNewSessionDraft: action,
      forkSession: action,
      prepareDeleteSession: action,
      deleteSession: action,
      clearSession: action,
      compactSession: action,
      generateSessionTitle: action,
      cancelSessionCompaction: action,
      setSessionProxy: action,
      clearSessionNotice: action,
      addWorkspace: action,
      openWorkspaceEditor: action,
      closeWorkspaceEditor: action,
      saveWorkspaceEditor: action,
      removeWorkspace: action,
      openWorkspace: action,
      openSessionWorkspaceDirectory: action,
      openSessionResourceInSystem: action,
      copySessionResourcePath: action,
      copyWorkspacePath: action,
      submitInput: action,
      submitSessionCommand: action,
      exportSession: action,
      setSessionModel: action,
      setNewSessionDraftModel: action,
      setSessionVariant: action,
      setSessionApprovalMode: action,
      setNewSessionDraftApprovalMode: action,
      setSessionReasoningEffort: action,
      renameSession: action,
      setSessionPinned: action,
      setMessageFeedback: action,
      archiveSession: action,
      prioritizeQueuedInput: action,
      cancelQueuedInput: action,
      resumeQueuedInput: action,
      resumeAllQueuedInputs: action,
      interruptRun: action,
      clearWorkPlan: action,
      stopGoal: action,
      resumeGoal: action,
      clearGoal: action,
      decideApproval: action,
      rejectApprovalAndStopRun: action,
      restoreSession: action,
      clearInteractionError: action,
      showInteractionError: action,
      loadPreviousConversationPage: action,
      locateConversationRun: action,
      getSystemContext: action,
      toggleWorkspace: action,
      toggleLeftSidebar: action,
      toggleRightSidebar: action,
      setSidebarWidth: action,
      resetSidebarWidth: action,
      resetSidebarLayout: action,
      dispose: action,
    });
  }

  get device_context() { return this.navigation.target_device ? this.network.contexts.get(this.navigation.target_device) : undefined; }
  get connection() { return this.device_context?.connection ?? this.local_connection; }
  get projection() { return this.device_context?.projection ?? this.local_projection; }
  get live_execution() { return this.device_context?.live_execution ?? this.local_live_execution; }
  get composer_quotes() { return this.device_context?.quotes ?? this.local_composer_quotes; }
  get resource_workspace() { return this.device_context?.resources ?? this.local_resource_workspace; }
  deviceLabel(device: string | null): string {
    if (device === null && this.target_options.target_kind === "remote") return this.target_options.target_address?.split("/users/")[0] ?? "当前 Host";
    return this.network.label(device);
  }

  get reading_device() { return this.navigation.target_device; }

  get reading_context() { return this.reading_device ? this.network.contexts.get(this.reading_device) : undefined; }

  get reading_resources() { return this.reading_context?.resources ?? this.local_resource_workspace; }

  get reading_files() { return this.reading_context?.files ?? this.local_files; }

  get reading_projection() { return this.reading_context?.projection ?? this.local_projection; }

  get reading_connection() { return this.reading_context?.connection ?? this.local_connection; }

  get reading_drafts() { return this.reading_context?.drafts ?? this.local_new_session_drafts; }

  get reading_scope() {
    return this.navigation.selected_session_id
      ? `session:${this.navigation.selected_session_id}` : `draft:${this.navigation.selected_draft_key ?? "unbound"}`;
  }

  async connectReadingResources(): Promise<void> {
    const device = this.reading_device;
    const scope = this.reading_scope;
    try {
      if (device) await this.network.ensureConnected(device);
      if (this.#disposed || this.reading_device !== device || this.reading_scope !== scope) return;
      const runtime = device ? this.network.context(device).runtime : this.#local_runtime;
      const client = runtime.client;
      if (client && scope.startsWith("session:")) {
        const result = await client.command({ type: "get_session_view", payload: { session_id: scope.slice(8) } });
        if (this.#disposed || client !== runtime.client || this.reading_device !== device || this.reading_scope !== scope) return;
        if (result.payload.snapshot.value.session.session_id !== scope.slice(8)) throw new Error("资源会话归属不匹配。");
        runInAction(() => this.reading_projection.applySessionSnapshot(result.payload.snapshot));
      }
    } catch (failure) { if (!this.#disposed) this.showInteractionError(displayError(failure)); }
  }

  captureDesktopWorkspace(pins: DesktopPins = this.pins.value): DesktopWorkspaceSnapshot {
    const retained = new Map(this.#workspace_snapshot?.devices.map((item) => [item.device, item]));
    retained.set(null, { device: null, expanded: [...this.navigation.expanded_workspaces], resources: viewingSnapshot(this.local_resource_workspace.captureSnapshot(), this.local_files.desktop, this.local_files.native_host) });
    for (const [device, context] of this.network.contexts) retained.set(device, {
      device, expanded: [...context.sidebar.expanded_workspaces], defaults: { approval: context.drafts.default_approval_mode, model: context.drafts.default_model_selection }, resources: viewingSnapshot(context.resources.captureSnapshot(), this.local_files.desktop, false),
    });
    const named_pins = { ...pins, items: pins.items.map((pin) => {
      const projection = pin.device ? this.network.contexts.get(pin.device)?.projection : this.local_projection;
      const current = projection?.application?.active_sessions.find((session) => session.session_id === pin.session_id);
      const device_name = pin.device ? this.network.devices.find((device) => device.id === pin.device)?.name ?? pin.device_name : pin.device_name;
      return { ...pin, title: current?.title ?? pin.title, device_name };
    }) };
    return { version: 1, owner: this.target_options.target_address ?? "local", pins: named_pins,
      devices: [...retained.values()] };
  }

  get new_session_drafts() { return this.device_context?.drafts ?? this.local_new_session_drafts; }
  get files() { return this.device_context?.files ?? this.local_files; }
  get #runtime() { return this.device_context?.runtime ?? this.#local_runtime; }
  get sidebar_context() { return this.network.browsing_device ? this.network.contexts.get(this.network.browsing_device) : undefined; }
  get sidebar_projection() { return this.sidebar_context?.projection ?? this.local_projection; }
  get sidebar_navigation() { return this.sidebar_context?.sidebar ?? this.navigation; }
  get sidebar_connection() { return this.sidebar_context?.connection ?? this.local_connection; }
  get remote_device() { return this.navigation.target_device !== null; }

  get local_runtime_client() { return this.#local_runtime.client; }
  get runtime_client() { return this.#runtime.client; }

  connect(bootstrap?: RuntimeBootstrap): Promise<void> {
    this.#initial_connection ??= this.#connectAndRestore(bootstrap);
    return this.#initial_connection;
  }

  async #connectAndRestore(bootstrap?: RuntimeBootstrap): Promise<void> {
    this.#disposed = false;
    await this.initializePreferences();
    if (this.#disposed) return;
    if (!this.navigation.selected_session_id && !this.navigation.selected_draft_key) {
      this.new_session_drafts.open("unbound");
      this.navigation.selectDraft("unbound");
    }
    await this.#local_runtime.connect(bootstrap);
    if (bootstrap?.session?.identity && this.target_options.target_kind !== "remote") {
      if (!this.#disposed) void this.network.refresh();
    }
    if (this.#disposed || this.connection.state !== "connected") return;
    const draft_key = this.navigation.selected_draft_key;
    if (draft_key) void this.#loadNewSessionDraftSkills(draft_key);
    await this.#restoreResourceSnapshot();
    await this.retryPinImport();
  }

  retryLocalConnection(): void { this.#local_runtime.retryConnection(); }

  retryConnection(): void {
    this.#runtime.retryConnection();
  }

  initializePreferences(): Promise<void> {
    this.#preferences_initialization ??= this.#loadPreferences();
    return this.#preferences_initialization;
  }

  async #loadPreferences(): Promise<void> {
    try {
      const preferences = await loadDesktopPreferences(this.target_options.target_address);
      if (this.#disposed) return;
      runInAction(() => {
        this.navigation.applyPreferences(preferences);
        this.desktop_lifecycle.applyPreferences(preferences);
        this.new_session_drafts.applyDefaults(
          this.files.desktop ? preferences.default_approval_mode : "auto",
          this.files.desktop ? preferences.last_model_selection : null,
        );
      });
      this.#workspace_snapshot = this.local_files.desktop ? parseDesktopWorkspace(preferences.desktop_workspace, this.target_options.target_address ?? "local") : null;
      if (this.#workspace_snapshot) this.pins.load(this.#workspace_snapshot.pins);
      this.#pending_snapshot = this.target_options.restore_resources === false ? null : viewingSnapshot(preferences.resource_workspace, this.files.desktop, this.files.native_host);
    } catch (failure) {
      this.#preferences_read_failed = true;
      runInAction(() => { this.interaction_error = `桌面状态读取失败：${displayError(failure)}`; });
    }
  }

  #restoreResourceSnapshot(): Promise<void> {
    return this.#resource_restore ??= this.#restoreConnectedResources();
  }

  async #restoreConnectedResources(): Promise<void> {
    if (this.#disposed) return;
    try {
      if (this.#workspace_snapshot && this.target_options.restore_resources !== false) {
        for (const saved of this.#workspace_snapshot.devices) {
          if (this.#disposed) return;
          const context = saved.device ? this.network.context(saved.device) : null;
          runInAction(() => { (context?.sidebar ?? this.navigation).expanded_workspaces.replace(saved.expanded); });
          const snapshot = viewingSnapshot(saved.resources, this.local_files.desktop, !saved.device && this.local_files.native_host);
          if (snapshot) await (context?.resources ?? this.local_resource_workspace).restoreSnapshot(snapshot);
        }
        await this.connectReadingResources();
      } else if (this.#pending_snapshot) await this.local_resource_workspace.restoreSnapshot(this.#pending_snapshot);
    } catch (failure) {
      runInAction(() => { this.interaction_error = `右栏恢复失败：${displayError(failure)}`; });
    } finally {
      this.#pending_snapshot = null;
      this.#preferences_ready = !this.#disposed && !this.#preferences_read_failed;
      this.#discardUnavailableDefaultModel();
    }
  }

  toggleWorkspace(workspace_id: WorkspaceId): void {
    this.sidebar_navigation.toggleWorkspace(workspace_id);
    this.#schedulePreferencesSave();
  }

  toggleLeftSidebar(): void {
    this.navigation.toggleLeftSidebar();
    this.#schedulePreferencesSave();
  }

  toggleRightSidebar(): void {
    this.navigation.toggleRightSidebar();
    this.#schedulePreferencesSave();
  }

  setSidebarWidth(side: "left" | "right", width: number, persist = false): void {
    this.navigation.setSidebarWidth(side, width);
    if (persist) this.#schedulePreferencesSave();
  }

  resetSidebarWidth(side: "left" | "right"): void {
    this.navigation.resetSidebarWidth(side);
    this.#schedulePreferencesSave();
  }

  resetSidebarLayout(): void {
    this.navigation.resetSidebarLayout();
    this.#schedulePreferencesSave();
  }

  #notification_navigation = 0;

  notificationScopeFor(client: RuntimeClient | null): string | null {
    return client ? JSON.stringify([client.address, client.instance_id, client.login_context, client.target_device]) : null;
  }

  notificationScope(): string | null { return this.notificationScopeFor(this.#runtime?.client); }

  notificationDevice(scope: string): string | null | undefined {
    if (this.#disposed || !this.#local_runtime.client) return undefined;
    try {
      const value: unknown = JSON.parse(scope);
      const local = this.#local_runtime.client;
      if (!Array.isArray(value) || value.length !== 4 || value[0] !== local.address
        || value[1] !== local.instance_id || value[2] !== local.login_context) return undefined;
      const device: unknown = value[3];
      return device === null || (typeof device === "string" && this.network.devices.some((item) => item.id === device && item.enabled))
        ? device : undefined;
    } catch { return undefined; }
  }

  notificationClient(scope: string): RuntimeClient | null {
    if (this.#disposed) return null;
    const clients = [this.#local_runtime.client, ...[...this.network.contexts.values()].map((context) => context.runtime.client)];
    return clients.find((client) => this.notificationScopeFor(client) === scope) ?? null;
  }

  async openNotificationTarget(target: NotificationTarget): Promise<boolean> {
    if (this.composer_pending || this.pending_session_action) return false;
    const device = this.notificationDevice(target.scope);
    if (device === undefined) return false;
    const generation = ++this.#notification_navigation;
    const selection = () => JSON.stringify([this.navigation.target_device, this.navigation.selected_session_id, this.navigation.selected_draft_key, this.navigation.selected_child_task_id]);
    const previous_selection = selection();
    if (device && !this.notificationClient(target.scope)) await this.network.ensureConnected(device);
    const client = this.notificationClient(target.scope);
    if (!client || this.#disposed || generation !== this.#notification_navigation || selection() !== previous_selection) return false;
    const projection = device ? this.network.context(device).projection : this.local_projection;
    const result = await client.command({ type: "get_session_view", payload: { session_id: target.owner.session_id } });
    if (this.#disposed || client !== this.notificationClient(target.scope)
      || generation !== this.#notification_navigation || selection() !== previous_selection) return false;
    const view = result.payload.snapshot.value;
    if (view.session.session_id !== target.owner.session_id) throw new Error("通知来源会话不匹配。");
    runInAction(() => {
      projection.applySessionSnapshot(result.payload.snapshot);
      this.transient_focus.clear();
      this.interaction_error = null;
      this.navigation.setListMode(view.session.lifecycle === "archived" ? "archived" : "active");
      this.navigation.selectSession(target.owner.session_id, true, device);
      this.settings.close();
      if (target.kind === "approval" && !view.approvals.items.some((item) => item.approval_id === target.approval_id)) {
        this.interaction_error = "这项审批已处理或已失效，当前显示会话最新状态。";
      }
    });
    // 子任务审批仍位于父会话唯一的审批区，不能创建第二份等待列表。
    if (target.kind !== "approval" && target.owner.type === "child_task") {
      await this.openChildTask(target.owner.session_id, target.owner.child_task_id);
    }
    return true;
  }

  async selectSession(session_id: SessionId, target_device: string | null = this.navigation.target_device): Promise<void> {
    if (target_device !== this.navigation.target_device && (this.pending_session_action || this.composer_pending)) { this.showInteractionError("请等待当前操作完成后切换会话设备。"); return; }
    if (target_device) this.network.context(target_device);
    this.transient_focus.clear();
    this.interaction_error = null;
    this.conversation_search.reset();
    this.#conversation_search_revision++;
    this.navigation.selectSession(session_id, true, target_device);
    if (target_device) await this.network.ensureConnected(target_device);
    if (this.navigation.target_device !== target_device || this.navigation.selected_session_id !== session_id) return;
    await this.#runtime.loadSession(session_id);
  }

  async openChildTask(session_id: SessionId, child_task_id: ChildTaskId): Promise<void> {
    const device = this.navigation.target_device;
    this.transient_focus.clear();
    if (this.navigation.selected_session_id !== session_id) {
      await this.selectSession(session_id);
    }
    if (this.navigation.target_device !== device || this.navigation.selected_session_id !== session_id) return;
    this.navigation.openChildTask(child_task_id);
    await this.#runtime.loadChildTask(session_id, child_task_id);
  }

  closeChildTask(): void {
    this.transient_focus.clear();
    this.navigation.closeChildTask();
  }

  async navigateBack(): Promise<void> {
    if (this.pending_session_action || this.composer_pending) return;
    this.transient_focus.clear();
    const previous = this.navigation.conversation_history[this.navigation.conversation_history_index - 1];
    if (previous?.target_device) this.network.context(previous.target_device);
    const location = this.navigation.goBack();
    if (location) {
      await this.#loadConversationLocation(location);
    }
  }

  async navigateForward(): Promise<void> {
    if (this.pending_session_action || this.composer_pending) return;
    this.transient_focus.clear();
    const next = this.navigation.conversation_history[this.navigation.conversation_history_index + 1];
    if (next?.target_device) this.network.context(next.target_device);
    const location = this.navigation.goForward();
    if (location) {
      await this.#loadConversationLocation(location);
    }
  }

  async searchConversationHistory(reset = true): Promise<void> {
    const client = this.#runtime.client;
    const session_id = this.navigation.selected_session_id;
    const query = this.conversation_search.query.trim();
    if (!client || !session_id || !query) {
      return;
    }
    const offset = reset ? 0 : this.conversation_search.next_offset;
    if (offset === null) {
      return;
    }
    const revision = ++this.#conversation_search_revision;
    const scope = this.conversation_search.scope;
    this.conversation_search.beginSearch(reset);
    try {
      const result = await client.command({
        type: "search_conversation_history",
        payload: { session_id, query, scope, offset, limit: 40 },
      });
      if (
        revision !== this.#conversation_search_revision || client !== this.#runtime.client
        || query !== this.conversation_search.query.trim()
        || scope !== this.conversation_search.scope
      ) {
        return;
      }
      runInAction(() => this.conversation_search.applySearch(
        result.payload.items,
        result.payload.next_offset,
        result.payload.partial,
        reset,
      ));
    } catch (error: unknown) {
      if (revision === this.#conversation_search_revision && client === this.#runtime.client) {
        runInAction(() => this.conversation_search.failSearch(displayError(error)));
      }
    }
  }

  async selectConversationHistoryHit(hit: ConversationHistoryHit): Promise<void> {
    this.conversation_search.selectHit(hit);
    if (!hit.message_id) {
      await this.openConversationHistoryHit(hit);
      return;
    }
    const client = this.#runtime.client;
    const session_id = this.navigation.selected_session_id;
    if (!client || !session_id) {
      this.conversation_search.failRecall("Runtime 当前不可用。");
      return;
    }
    this.conversation_search.beginRecall();
    try {
      const result = await client.command({
        type: "get_conversation_recall_window",
        payload: {
          session_id,
          owner: hit.owner,
          message_id: hit.message_id,
          before: 3,
          after: 3,
        },
      });
      if (client === this.#runtime.client && this.conversation_search.selected_hit === hit) runInAction(() => this.conversation_search.applyRecall(result.payload));
    } catch (error: unknown) {
      if (client === this.#runtime.client) runInAction(() => this.conversation_search.failRecall(displayError(error)));
    }
  }

  async openConversationHistoryHit(hit: ConversationHistoryHit): Promise<void> {
    const message_id = hit.message_id;
    const location: ConversationLocation = {
      session_id: hit.owner.session_id,
      child_task_id: hit.owner.type === "child_task" ? hit.owner.child_task_id : null,
      anchor_message_id: message_id,
      scroll_offset: null,
    };
    // 先确认来源仍可读取，再提交 UI 导航，避免失效结果把用户带离当前会话。
    if (!await this.#loadConversationLocation(location)) {
      return;
    }
    this.navigation.setListMode(hit.lifecycle === "archived" ? "archived" : "active");
    this.navigation.navigateTo(location);
    this.conversation_search.closeRecall();
  }

  /** 打开 Runtime 已校验的 Recall 来源，并将当前位置压入 UI 导航历史栈。 */
  async openRecallNavigationTarget(target: RecallNavigationTarget): Promise<void> {
    const location: ConversationLocation = {
      session_id: target.owner.session_id,
      child_task_id: target.owner.type === "child_task" ? target.owner.child_task_id : null,
      anchor_message_id: target.message_id,
      scroll_offset: null,
    };
    if (!await this.#loadConversationLocation(location)) {
      return;
    }
    this.navigation.setListMode(target.lifecycle === "archived" ? "archived" : "active");
    this.navigation.navigateTo(location);
  }

  async locateTextQuoteSource(session_id: SessionId, quote: QuotedTextSnapshot): Promise<boolean> {
    const source_session_id = quote.source_owner.session_id;
    if (!quote.source_available || source_session_id !== session_id) return false;
    this.transient_focus.clear();
    const location: ConversationLocation = {
      session_id: source_session_id,
      child_task_id: quote.source_owner.type === "child_task"
        ? quote.source_owner.child_task_id
        : null,
      anchor_message_id: quote.source_message_id,
      scroll_offset: null,
    };
    if (!await this.#loadConversationLocation(location, quote.source_generation)) return false;
    runInAction(() => {
      this.navigation.navigateTo(location);
      this.transient_focus.focus(quote);
    });
    return true;
  }

  async cancelChildTask(session_id: SessionId, child_task_id: ChildTaskId): Promise<void> {
    await this.#run_interaction.cancelChildTask(session_id, child_task_id);
  }

  openNewSessionDraft(workspace_id: WorkspaceId | null = null, device: string | null = null): void {
    if (this.composer_pending || this.pending_session_action) return;
    const context = device ? this.network.context(device) : undefined;
    const drafts = context?.drafts ?? this.local_new_session_drafts;
    const projection = context?.projection ?? this.local_projection;
    this.transient_focus.clear();
    if (workspace_id) {
      const workspace = projection.application?.workspaces.find((item) => (
        item.workspace_id === workspace_id && item.lifecycle === "active"
      ));
      if (!workspace) {
        this.interaction_error = "项目当前不可用，请选择其他项目。";
        return;
      }
    }
    const key = draftKeyForWorkspace(workspace_id);
    drafts.open(key);
    this.navigation.selectDraft(key, device);
    this.interaction_error = null;
    if (device) void this.network.ensureConnected(device).then(() => {
      if (this.navigation.target_device === device && this.navigation.selected_draft_key === key) void this.#loadNewSessionDraftSkills(key);
    });
    else void this.#loadNewSessionDraftSkills(key);
  }

  switchDraftDevice(device: string | null): void {
    if (device === this.network.self_device) device = null;
    if (device === this.navigation.target_device || this.composer_pending || this.pending_session_action) return;
    if (device && (!this.network.available || !this.network.devices.some((item) => item.id === device && item.enabled))) return;
    const source_store = this.new_session_drafts;
    const source_files = this.files;
    const source = source_store.get(this.navigation.selected_draft_key);
    if (source?.quotes.length) { this.showInteractionError("引用属于当前设备，请移除引用后再切换设备；原草稿会保留。"); return; }
    if (source?.materialization_attempt || source?.attachments.some((item) => item.state === "uploading" || item.attachment_id)) return;
    const context = device ? this.network.context(device) : undefined;
    const destination = context?.drafts ?? this.local_new_session_drafts;
    const files = context?.files ?? this.local_files;
    const existing = destination.get("unbound");
    if (source && (!existing || !isDraftCustomized(existing, destination.default_approval_mode, destination.default_model_selection))) {
      try {
        source_files.transferSelectionsTo(files, source.attachments.map((item) => item.selection_id));
        destination.takeContentFrom("unbound", source);
        source_store.remove(source.key);
      } catch (error) { this.showInteractionError(displayError(error)); return; }
    }
    this.openNewSessionDraft(null, device);
  }

  async clearNewSessionDraft(key: NewSessionDraftKey): Promise<void> {
    if (this.composer_pending || this.new_session_drafts.get(key)?.materialization_attempt) {
      this.interaction_error = "首次发送结果待确认，暂不能清空草稿。";
      return;
    }
    this.#draft_skill_requests.delete(key);
    this.#draft_mcp_requests.delete(key);
    const removed = this.new_session_drafts.remove(key);
    if (removed) await releaseDraftSelections(removed, this.files);
    if (this.navigation.selected_draft_key === key) {
      this.new_session_drafts.open(key);
    }
  }

  moveNewSessionDraft(source: NewSessionDraftKey, workspace_id: WorkspaceId | null, picker_pending = false): boolean {
    const target = draftKeyForWorkspace(workspace_id);
    if (source === target) return true;
    if (workspace_id && !this.projection.application?.workspaces.some((workspace) =>
      workspace.workspace_id === workspace_id && workspace.lifecycle === "active")) {
      this.interaction_error = "目标项目当前不可用。";
      return false;
    }
    if (this.navigation.selected_draft_key !== source || this.composer_pending || picker_pending
      || this.new_session_drafts.get(source)?.attachments.some((item) => item.state === "uploading")
      || !this.resource_workspace.canMoveDraft(source, target)
      || !this.new_session_drafts.move(source, target)) {
      this.interaction_error = "无法移动草稿：目标已有内容或资源，或当前有选择、上传、发送操作。可取消移动并打开目标草稿。";
      return false;
    }
    this.#draft_skill_requests.delete(source);
    this.#draft_mcp_requests.delete(source);
    this.resource_workspace.moveDraft(source, target);
    this.navigation.selectDraft(target);
    this.interaction_error = null;
    void this.#loadNewSessionDraftSkills(target);
    void this.#verifyNewSessionDraftMcp(target);
    return true;
  }

  async materializeNewSessionDraft(key: NewSessionDraftKey): Promise<boolean> {
    this.transient_focus.clear();
    const draft = this.new_session_drafts.get(key);
    if (
      !draft
      || this.navigation.selected_draft_key !== key
      || this.connection.state !== "connected"
      || this.composer_pending
    ) {
      return false;
    }
    if (!this.connection.capabilities?.features?.includes("session_materialization")) {
      this.interaction_error = "当前 Runtime 不支持新会话首次发送，请重启或完成应用更新。";
      return false;
    }
    if (!draft.text.trim() && draft.attachments.length === 0 && draft.quotes.length === 0) {
      return false;
    }
    const previous_attempt = draft.materialization_attempt;
    let active_attempt = previous_attempt;
    const client = this.#runtime.client;
    const instance_id = this.connection.instance_id;
    if (!client || !instance_id) return false;
    const sameOwner = () => client === this.#runtime.client && instance_id === this.connection.instance_id
      && !this.#disposed;
    this.composer_pending = true;
    this.interaction_error = null;
    try {
      let result: SessionMaterializationResult | null = null;
      if (previous_attempt) {
        const recovered = await client.command({
          type: "get_session_materialization",
          payload: { idempotency_key: previous_attempt.idempotency_key },
        });
        if (!sameOwner() || this.new_session_drafts.get(key)?.materialization_attempt !== previous_attempt) return false;
        result = recovered.payload.materialization;
      }
      if (!result) {
        if (!previous_attempt) {
          if (draft.selected_skill_name && (draft.skill_status !== "ready"
            || !draft.skill_options.some((skill) => skill.name === draft.selected_skill_name))) {
            throw new Error("所选技能在当前项目不可用，请移除或重新选择。");
          }
          if (draft.selected_mcp) {
            const available = await this.listMcpServerOptions({
              context: { type: "new_session", payload: { workspace_id: draft.workspace_id ?? undefined } },
              variant: draft.variant,
            });
            const current_draft = this.new_session_drafts.get(key);
            if (!sameOwner() || !current_draft || current_draft.workspace_id !== draft.workspace_id
              || current_draft.selected_mcp?.server_key !== draft.selected_mcp.server_key
              || current_draft.text !== draft.text || current_draft.variant !== draft.variant) return false;
            const mcp_available = available.some((server) => server.server_key === draft.selected_mcp?.server_key);
            this.new_session_drafts.setSelectedMcpAvailability(key, draft.selected_mcp.server_key, mcp_available);
            if (!mcp_available) {
              throw new Error("所选 MCP 服务在当前项目不可用，请移除或重新选择。");
            }
          }
        }
        const manifest = previous_attempt ?? materializationManifest(draft);
        active_attempt = manifest;
        if (!previous_attempt) this.new_session_drafts.beginMaterialization(key, manifest);
        if (!sameOwner() || this.new_session_drafts.get(key)?.materialization_attempt !== manifest) return false;
        this.new_session_drafts.setAttachmentTransferState(key, "uploading");
        result = await this.files.materializeNewSession(manifest, createOperationId("materialize"));
      }
      if (!sameOwner() || this.new_session_drafts.get(key)?.materialization_attempt !== active_attempt) return false;
      runInAction(() => {
        // 物化已可靠成功，先转移标签 owner 再改导航；活跃 PTY/浏览器不重新创建。
        this.resource_workspace.transferDraft(key, result.session.session_id);
        this.new_session_drafts.remove(key);
        if (this.navigation.selected_draft_key === key) this.navigation.selectSession(result.session.session_id, false);
        if (result.session.workspace_id) {
          this.navigation.ensureWorkspaceExpanded(result.session.workspace_id);
        }
      });
      try {
        if (sameOwner()) await this.#runtime.loadApplication();
        if (sameOwner()) await this.#runtime.loadSession(result.session.session_id);
      } catch (error: unknown) {
        if (sameOwner()) runInAction(() => this.connection.markDisconnected(displayError(error)));
      }
      this.#schedulePreferencesSave();
      return true;
    } catch (error: unknown) {
      const message = displayError(error);
      runInAction(() => {
        if (!sameOwner()) return;
        if (!previous_attempt && !(error instanceof NativeResourceFailure && (error.code === "materialization_response_unknown" || error.outcome === "unknown"))) {
          this.new_session_drafts.clearMaterializationAttempt(key);
        }
        this.new_session_drafts.setAttachmentTransferState(key, "failed", message);
        this.interaction_error = message;
      });
      return false;
    } finally {
      runInAction(() => {
        this.composer_pending = false;
      });
    }
  }

  async forkSession(
    session_id: SessionId,
    fork_point: MessageId,
    expected_generation: number,
  ): Promise<SessionId | null> {
    return this.#session_management.forkSession(session_id, fork_point, expected_generation);
  }

  async prepareDeleteSession(session_id: SessionId): Promise<PrepareDeleteSessionResult | null> {
    return this.#session_management.prepareDeleteSession(session_id);
  }

  async deleteSession(prepared: PrepareDeleteSessionResult): Promise<boolean> {
    return this.#session_management.deleteSession(prepared);
  }

  clearSession(session_id: SessionId, expected_generation: number): Promise<ClearSessionResult | null> {
    return this.#session_management.clearSession(session_id, expected_generation);
  }

  compactSession(session_id: SessionId, expected_generation: number): Promise<CompactSessionOutcome | null> {
    return this.#session_management.compactSession(session_id, expected_generation);
  }

  generateSessionTitle(session_id: SessionId): Promise<boolean> {
    return this.#session_management.generateSessionTitle(session_id);
  }

  cancelSessionCompaction(session_id: SessionId, operation_id: string): Promise<boolean> {
    return this.#session_management.cancelSessionCompaction(session_id, operation_id);
  }

  setSessionProxy(session_id: SessionId, enabled: boolean): Promise<boolean> {
    return this.#session_management.setSessionProxy(session_id, enabled);
  }

  clearSessionNotice(session_id?: SessionId): void {
    if (!session_id || this.session_notice?.session_id === session_id) {
      this.session_notice = null;
    }
  }

  #workspaceManagement(device: string | null): SessionManagementController {
    const context = device ? this.network.context(device) : undefined;
    return new SessionManagementController({
      connection: context?.connection ?? this.local_connection, runtime: context?.runtime ?? this.#local_runtime,
      resources: context?.resources ?? this.local_resource_workspace, files: context?.files ?? this.local_files,
      navigation: this.navigation, state: this, save_preferences: () => this.#schedulePreferencesSave(),
      select_draft: (id) => this.openNewSessionDraft(id, device),
      select_session: (id) => this.selectSession(id, device),
    });
  }

  workspaceProjection(device: string | null) { return device ? this.network.context(device).projection : this.local_projection; }

  addWorkspace(device: string | null = this.navigation.target_device): void {
    if (this.pending_workspace_action || this.pending_session_action) return;
    this.workspace_editor = { device, mode: "create", primary_directory: null };
  }

  openWorkspaceEditor(workspace_id: WorkspaceId, device: string | null = this.navigation.target_device): void {
    this.workspace_editor = { device, mode: "edit", workspace_id };
  }

  closeWorkspaceEditor(): void {
    if (!this.pending_workspace_action) this.workspace_editor = null;
  }

  async chooseWorkspaceDirectory(initial_path: string | null = null): Promise<string | null> {
    const device = this.workspace_editor ? this.workspace_editor.device : this.navigation.target_device;
    const files = device ? this.network.context(device).files : this.local_files;
    if (files.native_host) return this.#session_management.chooseWorkspaceDirectory();
    if (this.directory_picker || this.#disposed) return null;
    return new Promise((resolve) => runInAction(() => { this.directory_picker = {files, initial_path, resolve}; }));
  }

  finishDirectorySelection(path: string | null): void {
    const picker = this.directory_picker; this.directory_picker = null;
    picker?.resolve(path);
  }

  async saveWorkspaceEditor(input: Readonly<{
    label: string;
    primary_directory: string | null;
    additional_directories: string[];
  }>): Promise<boolean> {
    const editor = this.workspace_editor;
    if (!editor) return false;
    const management = this.#workspaceManagement(editor.device);
    const saved = editor.mode === "create"
      ? await management.registerWorkspace(input)
      : await management.updateWorkspace({ workspace_id: editor.workspace_id, ...input }) !== null;
    if (saved) {
      runInAction(() => {
        this.workspace_editor = null;
      });
    }
    return saved;
  }

  async removeWorkspace(workspace_id: WorkspaceId, device: string | null = this.network.browsing_device): Promise<boolean> {
    const context = device ? this.network.context(device) : undefined;
    const drafts = context?.drafts ?? this.local_new_session_drafts;
    const files = context?.files ?? this.local_files;
    const key = draftKeyForWorkspace(workspace_id);
    if (this.composer_pending || drafts.get(key)?.materialization_attempt) {
      this.interaction_error = "该项目仍有待确认的首次发送，暂不能移除。";
      return false;
    }
    const removed = await this.#workspaceManagement(device).removeWorkspace(workspace_id);
    if (removed) {
      const draft = drafts.get(key)?.materialization_attempt
        ? null : drafts.remove(key);
      if (draft) await releaseDraftSelections(draft, files);
    }
    return removed;
  }

  listRemovedWorkspaces(device: string | null = this.network.browsing_device): Promise<WorkspaceSummary[] | null> {
    return this.#workspaceManagement(device).listRemovedWorkspaces();
  }

  restoreWorkspace(workspace_id: WorkspaceId, device: string | null = this.network.browsing_device): Promise<boolean> {
    return this.#workspaceManagement(device).restoreWorkspace(workspace_id);
  }

  async openWorkspace(workspace_id: WorkspaceId): Promise<void> {
    if (this.network.browsing_device) { this.showInteractionError("远端路径不能在本机直接打开，请先下载文件。"); return; }
    await this.#workspaceManagement(null).openWorkspace(workspace_id);
  }

  async openSessionWorkspaceDirectory(session_id: SessionId, directory_index: number): Promise<void> {
    if (this.remote_device) { this.showInteractionError("远端路径不能在本机直接打开，请先下载文件。"); return; }
    await this.#session_management.openSessionWorkspaceDirectory(session_id, directory_index);
  }

  async openSessionResourceInSystem(
    session_id: SessionId,
    locator: SessionResourceLocator,
  ): Promise<void> {
    this.interaction_error = null;
    try {
      if (this.remote_device) throw new Error("远端路径不能在本机直接打开，请先下载文件。");
      await openNativeSessionResourceInSystem(session_id, locator);
    } catch (error: unknown) {
      runInAction(() => {
        this.interaction_error = displayError(error);
      });
    }
  }

  async copySessionResourcePath(
    session_id: SessionId,
    locator: SessionResourceLocator,
  ): Promise<void> {
    this.interaction_error = null;
    try {
      if (this.remote_device) throw new Error("远端路径不能在本机直接打开，请先下载文件。");
      await copyNativeSessionResourcePath(session_id, locator);
    } catch (error: unknown) {
      runInAction(() => {
        this.interaction_error = displayError(error);
      });
    }
  }

  async copyWorkspacePath(path: string): Promise<void> {
    await this.#session_management.copyWorkspacePath(path);
  }

  clearInteractionError(): void {
    this.interaction_error = null;
  }

  showInteractionError(message: string): void {
    this.interaction_error = message;
  }

  async submitInput(
    session_id: SessionId,
    message: string,
    variant: AgentVariant,
    attachment_ids: readonly AttachmentId[] = [],
    mode: SubmitInputMode = "normal",
    skill_name: string | null = null,
    quotes: readonly QuotedTextSnapshot[] = [],
    mcp_server_key: McpServerKey | null = null,
  ): Promise<boolean> {
    this.transient_focus.clear();
    return this.#run_interaction.submitInput(
      session_id,
      message,
      variant,
      attachment_ids,
      mode,
      skill_name,
      quotes,
      mcp_server_key,
    );
  }

  async exportSession(session_id: SessionId, title: string): Promise<boolean> {
    return this.#session_management.exportSession(session_id, title);
  }

  async listSessionPage(filter: "active" | "archived", offset: number, query?: string, target_device: string | null = this.network.browsing_device) {
    const runtime = target_device ? this.network.context(target_device).runtime : this.#local_runtime;
    const client = runtime.client;
    if (!client || this.sidebar_connection.state !== "connected") throw new Error("Runtime 未连接");
    const result = await client.command({ type: "list_sessions", payload: { filter, offset, limit: 100, query: query ?? null } });
    if (client !== runtime.client) throw new Error("Runtime 连接已变化，请重试");
    return result.payload;
  }

  async loadMoreSessions(filter: "active" | "archived"): Promise<void> {
    const projection = this.sidebar_projection;
    const application = projection.application;
    if (!application) return;
    const offset = filter === "active" ? application.active_sessions_next_offset : application.archived_sessions_next_offset;
    if (offset === null || offset === undefined) return;
    const page = await this.listSessionPage(filter, offset);
    if (application !== projection.application) return;
    projection.appendSessionPage(filter, page.sessions, page.has_more ? offset + 100 : null);
  }

  async listSkills(workspace_id?: WorkspaceId | null): Promise<SkillManagementSnapshot> {
    const client = this.#runtime.client;
    if (!client || this.connection.state !== "connected") throw new Error("Runtime 未连接");
    const result = await client.command({ type: "list_skills", payload: { include_plugins: true, ...(workspace_id ? { workspace_id } : {}) } });
    if (client !== this.#runtime.client || this.connection.state !== "connected") throw new Error("Runtime 连接已变化，请重试");
    return result.payload.snapshot;
  }

  async listMcpServerOptions(request: ListMcpServerOptionsRequest): Promise<readonly McpServerOptionSnapshot[]> {
    const client = this.#runtime.client;
    if (!client || this.connection.state !== "connected") throw new Error("Runtime 未连接");
    const result = await client.command({ type: "list_mcp_server_options", payload: request });
    if (client !== this.#runtime.client || this.connection.state !== "connected") throw new Error("Runtime 连接已变化，请重试");
    return result.payload.servers;
  }

  async submitSessionCommand(session_id: SessionId, command: SessionCommand): Promise<boolean> {
    if (!this.projection.application?.capabilities.session_commands) {
      this.interaction_error = "当前 Runtime 不支持会话控制指令";
      return false;
    }
    return this.#run_interaction.submitSessionCommand(session_id, command);
  }

  async setSessionModel(session_id: SessionId, model_selection: ModelSelection | null): Promise<boolean> {
    const succeeded = await this.#session_management.setSessionModel(session_id, model_selection);
    if (succeeded && this.files.desktop) {
      runInAction(() => this.new_session_drafts.applyDefaults(
        this.new_session_drafts.default_approval_mode,
        model_selection,
      ));
      this.#schedulePreferencesSave();
    }
    return succeeded;
  }

  setNewSessionDraftModel(key: NewSessionDraftKey, model_selection: ModelSelection | null): void {
    this.new_session_drafts.updateModel(key, model_selection);
    if (!this.files.desktop) return;
    this.new_session_drafts.applyDefaults(
      this.new_session_drafts.default_approval_mode,
      model_selection,
    );
    this.#schedulePreferencesSave();
  }

  async setSessionVariant(session_id: SessionId, variant: AgentVariant): Promise<boolean> {
    return this.#session_management.setSessionVariant(session_id, variant);
  }

  async setSessionApprovalMode(session_id: SessionId, approval_mode: ApprovalMode): Promise<boolean> {
    const succeeded = await this.#session_management.setSessionApprovalMode(session_id, approval_mode);
    if (succeeded && this.files.desktop) {
      runInAction(() => this.new_session_drafts.applyDefaults(
        approval_mode,
        this.new_session_drafts.default_model_selection,
      ));
      this.#schedulePreferencesSave();
    }
    return succeeded;
  }

  setNewSessionDraftApprovalMode(key: NewSessionDraftKey, approval_mode: ApprovalMode): void {
    this.new_session_drafts.updateApprovalMode(key, approval_mode);
    if (!this.files.desktop) return;
    this.new_session_drafts.applyDefaults(
      approval_mode,
      this.new_session_drafts.default_model_selection,
    );
    this.#schedulePreferencesSave();
  }

  async renameSession(session_id: SessionId, title: string): Promise<boolean> {
    return this.#session_management.renameSession(session_id, title);
  }

  isSessionPinned(session: { session_id: string; is_pinned: boolean }, device: string | null): boolean {
    return this.local_files.desktop ? this.pins.has(session.session_id, device) : session.is_pinned;
  }

  async retryPinImport(): Promise<void> {
    if (this.local_files.desktop && this.#preferences_ready && this.local_runtime_client) {
      await this.pins.importLegacy(this.local_runtime_client, this.target_options.target_kind !== "remote");
    }
  }

  async removePin(pin: DesktopPin): Promise<void> { await this.pins.set(pin, false); }

  async setSessionPinned(session_id: SessionId, is_pinned: boolean, device: string | null = this.navigation.target_device): Promise<boolean> {
    if (!this.local_files.desktop) return this.#session_management.setSessionPinned(session_id, is_pinned);
    if (!this.#preferences_ready || !this.pins.value.imported) return false;
    const projection = device ? this.network.contexts.get(device)?.projection : this.local_projection;
    const session = projection?.session_views.get(session_id)?.session ?? projection?.application?.active_sessions.find((item) => item.session_id === session_id);
    if (!session) return false;
    return this.pins.set(pinReference(session, device, this.deviceLabel(device)), is_pinned);
  }

  async setMessageFeedback(
    session_id: SessionId,
    message_id: MessageId,
    feedback: MessageFeedback | null,
  ): Promise<boolean> {
    return this.#session_management.setMessageFeedback(session_id, message_id, feedback);
  }

  async archiveSession(session_id: SessionId): Promise<boolean> {
    return this.#session_management.archiveSession(session_id);
  }

  async prioritizeQueuedInput(session_id: SessionId, input_id: InputId, revision: number): Promise<void> {
    await this.#run_interaction.prioritizeQueuedInput(session_id, input_id, revision);
  }

  async cancelQueuedInput(session_id: SessionId, input_id: InputId): Promise<void> {
    await this.#run_interaction.cancelQueuedInput(session_id, input_id);
  }

  async resumeQueuedInput(session_id: SessionId, input_id: InputId, revision: number): Promise<void> {
    await this.#run_interaction.resumeQueuedInput(session_id, input_id, revision);
  }

  async resumeAllQueuedInputs(session_id: SessionId, revision: number): Promise<void> {
    await this.#run_interaction.resumeAllQueuedInputs(session_id, revision);
  }

  async interruptRun(session_id: SessionId, run_id: RunId): Promise<void> {
    await this.#run_interaction.interruptRun(session_id, run_id);
  }

  async clearWorkPlan(session_id: SessionId, expected_revision: number): Promise<boolean> {
    return this.#run_interaction.clearWorkPlan(session_id, expected_revision);
  }

  async stopGoal(session_id: SessionId, goal_id: GoalId, expected_generation: number): Promise<boolean> {
    return this.#run_interaction.stopGoal(session_id, goal_id, expected_generation);
  }

  async resumeGoal(
    session_id: SessionId,
    goal_id: GoalId,
    expected_generation: number,
    input_id: InputId | null = null,
  ): Promise<boolean> {
    return this.#run_interaction.resumeGoal(session_id, goal_id, expected_generation, input_id);
  }

  async clearGoal(session_id: SessionId, goal_id: GoalId, expected_generation: number): Promise<boolean> {
    return this.#run_interaction.clearGoal(session_id, goal_id, expected_generation);
  }

  async setSessionReasoningEffort(session_id: SessionId, effort: import("@ez-assistant/protocol").ReasoningEffortKey | null): Promise<boolean> {
    return this.#run_interaction.setSessionReasoningEffort(session_id, effort);
  }

  async decideApproval(session_id: SessionId, approval_id: ApprovalId, decision: ApprovalDecision): Promise<void> {
    await this.#run_interaction.decideApproval(session_id, approval_id, decision);
  }

  async rejectApprovalAndStopRun(
    session_id: SessionId,
    approval_id: ApprovalId,
    queue_revision: number,
  ): Promise<void> {
    await this.#run_interaction.rejectApprovalAndStopRun(session_id, approval_id, queue_revision);
  }

  async restoreSession(session_id: SessionId): Promise<void> {
    await this.#session_management.restoreSession(session_id);
  }

  async loadPreviousConversationPage(
    session_id: SessionId,
    child_task_id: ChildTaskId | null = null,
  ): Promise<boolean> {
    const client = this.#runtime.client;
    if (!client) return false;
    const instance_id = client.instance_id;
    const currentHistory = () => child_task_id
      ? this.projection.child_conversation_histories.get(child_task_id)
      : this.projection.conversation_histories.get(session_id);
    const history = child_task_id
      ? this.projection.child_conversation_histories.get(child_task_id)
      : this.projection.conversation_histories.get(session_id);
    const began = child_task_id
      ? this.projection.beginLoadingPreviousChild(child_task_id)
      : this.projection.beginLoadingPrevious(session_id);
    if (!client || !history || !began) {
      return false;
    }
    const pending = currentHistory()?.previous_request;
    const is_current = () => this.#runtime.client === client && client.instance_id === instance_id
      && currentHistory()?.previous_request === pending
      && currentHistory()?.generation === history.generation
      && currentHistory()?.previous_cursor === history.previous_cursor
      && (currentHistory()?.items[0]?.message_id ?? null) === pending?.first_id;
    try {
      const result = await client.command({
        type: "list_conversation_page",
        payload: {
          owner: history.owner,
          cursor: history.previous_cursor,
          limit: 30,
        },
      });
      if (!is_current()) return false;
      const page = result.payload.snapshot.value;
      if (page.owner.type !== history.owner.type || page.owner.session_id !== session_id
        || (page.owner.type === "child_task" && page.owner.child_task_id !== child_task_id)
        || page.generation !== history.generation) {
        throw new RuntimeClientError("snapshot_stale", "历史已更新，请重新加载。");
      }
      return runInAction(() => child_task_id
        ? this.projection.applyPreviousChildConversationPage(child_task_id, result.payload.snapshot)
        : this.projection.applyPreviousConversationPage(session_id, result.payload.snapshot));
    } catch (error: unknown) {
      if (!is_current()) return false;
      const message = error instanceof RuntimeClientError && error.code === "snapshot_busy"
        ? "历史正在更新，请重试。" : "历史加载失败，请重试。";
      runInAction(() => child_task_id
        ? this.projection.failLoadingPreviousChild(child_task_id, message)
        : this.projection.failLoadingPrevious(session_id, message));
      if (error instanceof RuntimeClientError && error.code === "snapshot_stale") {
        if (child_task_id) {
          await this.#runtime.loadChildTask(session_id, child_task_id);
        } else {
          await this.#runtime.loadSession(session_id);
        }
        return false;
      }
      return false;
    }
  }

  async getToolDetail(
    owner: ConversationOwner,
    message_id: MessageId,
    call_id: ToolCallId,
  ): Promise<ToolDetailSnapshot> {
    if (!this.#runtime.client) {
      throw new RuntimeClientError("runtime_unavailable", "Runtime 当前不可用。");
    }
    const result = await this.#runtime.client.command({
      type: "get_tool_detail",
      payload: { owner, message_id, call_id },
    });
    return result.payload.snapshot.value;
  }

  async locateConversationRun(session_id: SessionId, run_id: RunId): Promise<boolean> {
    const client = this.#runtime.client;
    if (!client) {
      return false;
    }
    try {
      const result = await client.command({
        type: "get_conversation_page_around_run",
        payload: { session_id, run_id, limit: 30 },
      });
      if (client !== this.#runtime.client) return false;
      const location: ConversationLocation = {
        session_id,
        child_task_id: null,
        anchor_message_id: result.payload.anchor_message_id,
        scroll_offset: null,
      };
      const loaded = await this.#loadConversationLocation(
        location,
        result.payload.snapshot.value.generation,
      );
      if (loaded) {
        runInAction(() => this.navigation.requestConversationAnchor(
          result.payload.anchor_message_id,
        ));
      }
      return loaded;
    } catch (error: unknown) {
      if (client !== this.#runtime.client) return false;
      runInAction(() => this.projection.failLoadingPrevious(session_id, displayError(error)));
      return false;
    }
  }

  /** 仅补齐当前阅读锚点，不改变导航或业务事件水位。 */
  async restoreReadingAnchor(session_id: SessionId, child_task_id: ChildTaskId | null, message_id: MessageId): Promise<boolean> {
    const client = this.#runtime.client;
    const index = this.navigation.conversation_history_index;
    const is_current = () => this.#runtime.client === client
      && this.navigation.conversation_history_index === index
      && this.navigation.selected_session_id === session_id
      && this.navigation.selected_child_task_id === child_task_id;
    if (!client || !is_current()) return false;
    try {
      // 先确认消息仍存在，避免为了已删除的锚点自动翻完整份历史。
      const located = await client.command({ type: "get_conversation_page_around_message", payload: {
        owner: child_task_id ? { type: "child_task", session_id, child_task_id } : { type: "main_session", session_id },
        message_id, limit: 1,
      } });
      if (!is_current()) return false;
      return this.#loadConversationLocation(
        { session_id, child_task_id, anchor_message_id: message_id, scroll_offset: null },
        located.payload.snapshot.value.generation, is_current,
      );
    } catch {
      if (is_current()) this.showInteractionError("原阅读位置暂不可用，已保留当前有效历史。");
      return false;
    }
  }

  async #loadConversationLocation(
    location: ConversationLocation,
    expected_generation?: number,
    is_current: () => boolean = () => true,
  ): Promise<boolean> {
    const device = location.target_device === undefined ? this.navigation.target_device : location.target_device;
    if (device) await this.network.ensureConnected(device);
    const client = this.#runtime.client;
    const prior_guard = is_current;
    is_current = () => prior_guard() && this.#runtime.client === client && this.navigation.target_device === device;
    if (!is_current()) return false;
    if (!client) {
      this.showInteractionError("Runtime 当前不可用。");
      return false;
    }
    try {
      const selected_location = this.navigation.selected_session_id === location.session_id
        && this.navigation.selected_child_task_id === location.child_task_id;
      const current = this.#conversationHistoryForLocation(location);
      if (
        selected_location
        && location.anchor_message_id
        && current
        && (expected_generation === undefined || current.generation === expected_generation)
        && current.items.some((item) => item.message_id === location.anchor_message_id)
      ) {
        return true;
      }

      await this.#runtime.loadSession(location.session_id);
      if (!is_current()) return false;
      if (location.child_task_id) {
        await this.#runtime.loadChildTask(location.session_id, location.child_task_id);
      }

      if (!is_current()) return false;
      let history = this.#conversationHistoryForLocation(location);
      if (!history) {
        throw new RuntimeClientError("conversation_unavailable", "来源会话暂时无法读取。");
      }
      if (expected_generation !== undefined && history.generation !== expected_generation) {
        throw new RuntimeClientError("snapshot_stale", "引用来源已更新。");
      }
      while (
        location.anchor_message_id
        && !history.items.some((item) => item.message_id === location.anchor_message_id)
      ) {
        if (!history.has_more || !history.previous_cursor) {
          throw new RuntimeClientError("message_not_found", "来源消息已不可用。");
        }
        const previous_cursor = history.previous_cursor;
        const previous_item_count = history.items.length;
        const loaded = await this.loadPreviousConversationPage(
          location.session_id,
          location.child_task_id,
        );
        if (!is_current()) return false;
        history = this.#conversationHistoryForLocation(location);
        if (!loaded || !history) {
          throw new RuntimeClientError("conversation_unavailable", "来源会话暂时无法读取。");
        }
        if (expected_generation !== undefined && history.generation !== expected_generation) {
          throw new RuntimeClientError("snapshot_stale", "引用来源已更新。");
        }
        if (
          history.previous_cursor === previous_cursor
          && history.items.length === previous_item_count
        ) {
          throw new RuntimeClientError("conversation_unavailable", "来源会话暂时无法继续加载。");
        }
      }
      return true;
    } catch (error: unknown) {
      if (!is_current()) return false;
      runInAction(() => this.showInteractionError(
        conversationSourceError(error),
      ));
      return false;
    }
  }

  #conversationHistoryForLocation(location: ConversationLocation) {
    return location.child_task_id
      ? this.projection.child_conversation_histories.get(location.child_task_id)
      : this.projection.conversation_histories.get(location.session_id);
  }

  async getSystemContext(session_id: SessionId): Promise<SystemContextSnapshot> {
    if (!this.#runtime.client) {
      throw new RuntimeClientError("runtime_unavailable", "Runtime 当前不可用。");
    }
    const result = await this.#runtime.client.command({
      type: "get_system_context",
      payload: { session_id },
    });
    return result.payload.snapshot;
  }

  async getAgentShellSettings(): Promise<AgentShellSettings> {
    const client = this.#runtime.client;
    if (!client) throw new RuntimeClientError("runtime_unavailable", "Runtime 当前不可用。");
    const result = await client.command({ type: "get_agent_shell_settings", payload: {} });
    if (client !== this.#runtime.client) throw new RuntimeClientError("runtime_unavailable", "Runtime 连接已切换。");
    return result.payload;
  }

  async reloadSelectedConversation(): Promise<void> {
    const session_id = this.navigation.selected_session_id;
    if (session_id) {
      await this.#runtime.loadSession(session_id);
    }
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#flush_view_state();
    this.#disposed = true;
    this.#pending_notifications_disposer();
    this.pending_notifications.dispose();
    this.#feedback_disposer();
    this.#device_navigation_disposer();
    this.#feedback_navigator_disposer();
    this.feedback.dispose();
    this.feedback_center.dispose();
    this.notifications.dispose();
    this.#resource_snapshot_disposer();
    window.removeEventListener("scroll", this.#save_view_state, true);
    window.removeEventListener("pointerup", this.#save_view_state);
    window.removeEventListener("wheel", this.#save_view_state, true);
    window.removeEventListener("keyup", this.#save_view_state);
    window.removeEventListener("blur", this.#flush_view_state);
    window.removeEventListener("pagehide", this.#flush_view_state);
    window.removeEventListener("pagehide", this.local_resource_workspace.disconnectTerminals);
    this.transient_focus.clear();
    this.#resource_scope_disposer();
    this.pins.dispose();
    this.local_resource_workspace.dispose();
    this.local_files.dispose();
    this.finishDirectorySelection(null);
    this.network.dispose();
    this.#local_runtime.dispose();
    this.device_gateway.dispose();
    this.settings.mcp.dispose();
    this.desktop_lifecycle.dispose();
    this.#runtime_state_disposer();
    this.#provider_defaults_disposer();
    for (const draft of this.local_new_session_drafts.clear()) {
      void releaseDraftSelections(draft, this.local_files);
    }
    if (this.#preferences_save_timer !== null) {
      window.clearTimeout(this.#preferences_save_timer);
      this.#preferences_save_timer = null;
    }
    if (this.#title_notice_timer !== null) {
      window.clearTimeout(this.#title_notice_timer);
      this.#title_notice_timer = null;
    }
  }

  #schedulePreferencesSave(): void {
    if (!this.#preferences_ready || this.#disposed || this.resource_workspace.shutting_down) return;
    if (this.#preferences_save_timer !== null) window.clearTimeout(this.#preferences_save_timer);
    this.#preferences_save_timer = window.setTimeout(() => {
      this.#preferences_save_timer = null;
      this.#flush_view_state();
    }, 300);
  }

  async flushPreferences(pins?: DesktopPins): Promise<void> {
    if (this.pins.busy && !pins) return this.#preferences_pending;
    if (pins && (!this.#preferences_ready || this.#disposed)) throw new Error("本机偏好尚不可保存。");
    if (this.#preferences_save_timer !== null) window.clearTimeout(this.#preferences_save_timer);
    this.#preferences_save_timer = null;
    if (!this.#preferences_ready || this.#disposed || this.local_resource_workspace.shutting_down) return this.#preferences_pending;
    try {
      const preferences = {
        left_sidebar_open: this.navigation.left_sidebar_open,
        right_sidebar_open: this.navigation.right_sidebar_open,
        left_sidebar_width: this.navigation.left_sidebar_width,
        right_sidebar_width: this.navigation.right_sidebar_width,
        expanded_workspace_ids: [...this.navigation.expanded_workspaces],
        close_behavior: this.desktop_lifecycle.close_behavior,
        default_approval_mode: this.local_files.desktop
          ? this.local_new_session_drafts.default_approval_mode
          : "auto" as const,
        last_model_selection: this.local_files.desktop
          ? this.local_new_session_drafts.default_model_selection
          : null,
        resource_workspace: null,
        desktop_workspace: this.local_files.desktop ? this.captureDesktopWorkspace(pins) : undefined,
      };
      if (!this.local_files.desktop) {
        await saveDesktopPreferences(preferences);
        return;
      }
      const serialized = JSON.stringify(preferences);
      // 同一个 staging 文件只允许串行写入，后提交的快照不能被旧任务覆盖。
      const pending = this.#preferences_pending.catch(() => undefined).then(async () => {
        if (serialized === this.#last_saved_preferences) return;
        await saveDesktopPreferences(preferences, this.target_options.target_address);
        this.#last_saved_preferences = serialized;
      });
      this.#preferences_pending = pending;
      await pending;
    } catch (failure) {
      runInAction(() => { this.interaction_error = `桌面状态保存失败：${displayError(failure)}`; });
      throw failure;
    }
  }

  #discardUnavailableDefaultModel(): void {
    if (!this.local_files.desktop || !this.#preferences_ready) return;
    const application = this.local_projection.application;
    const selection = this.local_new_session_drafts.default_model_selection;
    if (!application || !selection || application.providers.some(
      (provider) => provider.provider_instance_id === selection.provider_instance_id,
    )) return;
    runInAction(() => {
      this.local_new_session_drafts.discardUnavailableDefaultModel(selection);
      this.interaction_error = "上次使用的模型所属服务商已删除，已改用默认模型。";
    });
    this.#schedulePreferencesSave();
  }

  async #loadNewSessionDraftSkills(key: NewSessionDraftKey): Promise<void> {
    const runtime = this.#runtime;
    const connection = this.connection;
    const drafts = this.new_session_drafts;
    const client = runtime.client;
    const instance_id = connection.instance_id;
    const draft = drafts.get(key);
    if (!client || !draft || !drafts.beginSkillLoad(key)) return;
    const request = {};
    this.#draft_skill_requests.set(key, request);
    try {
      const result = await client.command({
        type: "list_skills",
        payload: { include_plugins: true, ...(draft.workspace_id ? { workspace_id: draft.workspace_id } : {}) },
      });
      runInAction(() => {
        if (client === runtime.client && instance_id === connection.instance_id
          && this.#draft_skill_requests.get(key) === request
          && drafts.get(key)?.workspace_id === draft.workspace_id)
          drafts.applySkillOptions(key, result.payload.snapshot.skills);
      });
    } catch {
      runInAction(() => {
        if (client === runtime.client && instance_id === connection.instance_id
          && this.#draft_skill_requests.get(key) === request
          && drafts.get(key)?.workspace_id === draft.workspace_id)
          drafts.failSkillLoad(key);
      });
    }
  }

  async #verifyNewSessionDraftMcp(key: NewSessionDraftKey): Promise<void> {
    const runtime = this.#runtime;
    const connection = this.connection;
    const drafts = this.new_session_drafts;
    const client = runtime.client;
    const instance_id = connection.instance_id;
    const draft = drafts.get(key);
    if (!client || !draft?.selected_mcp) return;
    const request = {};
    const server_key = draft.selected_mcp.server_key;
    this.#draft_mcp_requests.set(key, request);
    try {
      const available = await this.listMcpServerOptions({
        context: { type: "new_session", payload: { workspace_id: draft.workspace_id ?? undefined } },
        variant: draft.variant,
      });
      runInAction(() => {
        if (client === runtime.client && instance_id === connection.instance_id
          && this.#draft_mcp_requests.get(key) === request
          && drafts.get(key)?.workspace_id === draft.workspace_id) {
          drafts.setSelectedMcpAvailability(
            key, server_key, available.some((server) => server.server_key === server_key),
          );
        }
      });
    } catch {
      // 查询失败保留未验证标签；发送前仍重新校验，不能信任旧工作空间缓存。
    }
  }
}

function materializationManifest(draft: NewSessionDraft): SessionMaterializationManifest {
  return {
    idempotency_key: createOperationId("new-session"),
    ...(draft.workspace_id ? { workspace_id: draft.workspace_id } : {}),
    ...(draft.model_selection ? { model_selection: draft.model_selection } : {}),
    ...(draft.reasoning_effort ? { reasoning_effort: draft.reasoning_effort } : {}),
    variant: draft.variant,
    approval_mode: draft.approval_mode,
    message: draft.text,
    mode: draft.goal_armed ? "start_goal" : "normal",
    attachments: draft.attachments.map((attachment) => ({
      selection_key: attachment.selection_id,
      original_name: attachment.original_name,
      size_bytes: attachment.size_bytes,
    })),
    quotes: [...draft.quotes],
    ...(draft.selected_skill_name ? { skill_name: draft.selected_skill_name } : {}),
    ...(draft.selected_agent ? { custom_agent: draft.selected_agent } : {}),
    ...(draft.selected_mcp ? { mcp_server_key: draft.selected_mcp.server_key } : {}),
  };
}

async function releaseDraftSelections(draft: NewSessionDraft, files: ClientResources): Promise<void> {
  await Promise.allSettled(
    draft.attachments.map((attachment) => files.releaseAttachmentSelection(attachment.selection_id)),
  );
}

function createOperationId(prefix: string): string {
  return typeof crypto.randomUUID === "function"
    ? `${prefix}-${crypto.randomUUID()}`
    : `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function displayError(error: unknown): string {
  return error instanceof Error ? error.message : "无法连接本地 Runtime。";
}

/** 将来源导航错误转换成用户可行动的提示，同时保留未知 Runtime 错误详情。 */
function conversationSourceError(error: unknown): string {
  if (error instanceof RuntimeClientError) {
    if (error.code === "snapshot_stale") {
      return "来源会话已更新，请重新搜索后再打开。";
    }
    if (error.code === "session_not_found" || error.code === "child_task_not_found") {
      return "来源会话已不存在，当前页面已保留。";
    }
  }
  return displayError(error);
}
