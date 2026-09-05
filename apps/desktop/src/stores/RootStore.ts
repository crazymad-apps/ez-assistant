import { parseResourceSnapshot, type ResourceWorkspaceSnapshot } from "../features/resource-workspace/resourceWorkspaceSnapshot";
import { action, makeObservable, observable, reaction, runInAction, type IReactionDisposer } from "mobx";
import type {
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
  McpServerKey,
  McpServerOptionSnapshot,
  MessageId,
  MessageFeedback,
  ModelKey,
  QuotedTextSnapshot,
  PrepareDeleteSessionResult,
  RecallNavigationTarget,
  RunId,
  SessionMaterializationManifest,
  SessionId,
  SessionResourceLocator,
  SessionCommand,
  ToolCallId,
  ToolDetailSnapshot,
  SystemContextSnapshot,
  SubmitInputMode,
  WorkspaceId,
} from "../generated/assistant-protocol";
import { loadDesktopPreferences, saveDesktopPreferences } from "../native-bridge/desktopPreferences";
import {
  copySessionResourcePath as copyNativeSessionResourcePath,
  materializeNewSession,
  NativeResourceFailure,
  openSessionResourceInSystem as openNativeSessionResourceInSystem,
  releaseAttachmentSelection,
} from "../native-bridge/nativeResource";
import { RuntimeClientError } from "../runtime-client/RuntimeClient";
import { ConnectionStore } from "./ConnectionStore";
import { ComposerQuoteStore } from "./ComposerQuoteStore";
import { ConversationSearchStore } from "./ConversationSearchStore";
import { DesktopLifecycleStore } from "./DesktopLifecycleStore";
import { DeviceGatewayStore } from "./DeviceGatewayStore";
import { LiveExecutionStore } from "./LiveExecutionStore";
import { MemorySettingsStore } from "./MemorySettingsStore";
import { NavigationStore, type ConversationLocation } from "./NavigationStore";
import {
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

export class RootStore {
  readonly connection = new ConnectionStore();
  readonly composer_quotes = new ComposerQuoteStore();
  readonly transient_focus = new TransientFocusStore();
  readonly projection = new RuntimeProjectionStore();
  readonly live_execution = new LiveExecutionStore();
  readonly navigation = new NavigationStore();
  readonly resource_workspace = new ResourceWorkspaceStore();
  readonly new_session_drafts = new NewSessionDraftStore();
  readonly conversation_search = new ConversationSearchStore();
  readonly settings: SettingsStore;
  readonly memory_settings: MemorySettingsStore;
  readonly device_gateway: DeviceGatewayStore;
  readonly desktop_lifecycle: DesktopLifecycleStore;
  pending_session_action = false;
  pending_workspace_action = false;
  workspace_editor: Readonly<
    | { mode: "create"; primary_directory: string }
    | { mode: "edit"; workspace_id: WorkspaceId }
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

  readonly #runtime: RuntimeLifecycleCoordinator;
  readonly #run_interaction: RunInteractionController;
  readonly #session_management: SessionManagementController;
  #disposed = false;
  #preferences_save_timer: number | null = null;
  #preferences_initialization: Promise<void> | null = null;
  #initial_connection: Promise<void> | null = null;
  #pending_snapshot: ResourceWorkspaceSnapshot | null = null;
  #preferences_ready = false;
  #preferences_pending: Promise<void> = Promise.resolve();
  #last_saved_preferences = "";
  #resource_snapshot_disposer: IReactionDisposer;
  readonly #save_view_state = () => this.#schedulePreferencesSave();
  readonly #flush_view_state = () => { void this.flushPreferences().catch(() => undefined); };
  #conversation_search_revision = 0;
  #title_notice_timer: number | null = null;
  #runtime_state_disposer: IReactionDisposer;
  #resource_scope_disposer: IReactionDisposer;

  constructor() {
    this.#runtime = new RuntimeLifecycleCoordinator({
      connection: this.connection,
      live_execution: this.live_execution,
      navigation: this.navigation,
      projection: this.projection,
      report_interaction_error: (message) => {
        this.interaction_error = message;
      },
      refresh_device_gateway: () => this.device_gateway.scheduleRefresh(),
      mark_device_gateway_stale: () => this.device_gateway.markStale(),
      on_title_generation_finished: (session_id, trigger, outcome) => {
        if (trigger !== "manual") return;
        if (this.#title_notice_timer !== null) {
          window.clearTimeout(this.#title_notice_timer);
          this.#title_notice_timer = null;
        }
        runInAction(() => {
          this.session_notice = outcome === "succeeded"
            ? { session_id, tone: "success", message: "标题已更新。" }
            : outcome === "failed"
              ? { session_id, tone: "warning", message: "标题生成失败。", action: "retry_title" }
              : null;
        });
        if (outcome === "succeeded") {
          this.#title_notice_timer = window.setTimeout(() => {
            this.#title_notice_timer = null;
            runInAction(() => this.clearSessionNotice(session_id));
          }, 1600);
        }
      },
    });
    this.device_gateway = new DeviceGatewayStore({
      get_client: () => this.#runtime.client,
      refresh_application: () => this.#runtime.loadApplication(),
    });
    this.desktop_lifecycle = new DesktopLifecycleStore({
      resources: this.resource_workspace,
      get_application: () => this.projection.application,
      prepare_runtime_mutation: (kind) => this.#runtime.prepareForNativeRuntimeMutation(kind),
      reconnect_runtime: (bootstrap) => this.#runtime.reconnectAfterNativeRuntimeMutation(bootstrap),
      mark_runtime_stopped: () => this.connection.markRuntimeStopped(),
      save_preferences: () => this.#schedulePreferencesSave(),
      flush_preferences: () => this.flushPreferences(),
    });
    this.#resource_scope_disposer = reaction(
      () => this.navigation.selected_session_id ? `session:${this.navigation.selected_session_id}`
        : `draft:${this.navigation.selected_draft_key ?? "unbound"}`,
      (scope) => this.resource_workspace.selectScope(scope),
      { fireImmediately: true },
    );
    this.#resource_snapshot_disposer = reaction(
      () => this.resource_workspace.captureSnapshot(),
      () => this.#schedulePreferencesSave(),
    );
    // 原生滚动事件以 capture 接住嵌套查看器；查看状态仍由各页面持有。
    window.addEventListener("scroll", this.#save_view_state, true);
    window.addEventListener("pointerup", this.#save_view_state);
    window.addEventListener("wheel", this.#save_view_state, { capture: true, passive: true });
    window.addEventListener("keyup", this.#save_view_state);
    window.addEventListener("blur", this.#flush_view_state);
    window.addEventListener("pagehide", this.#flush_view_state);
    this.desktop_lifecycle.start();
    this.#runtime_state_disposer = reaction(
      () => this.connection.state,
      (state) => this.desktop_lifecycle.syncRuntimeState(state),
      { fireImmediately: true },
    );
    this.settings = new SettingsStore({
      get_client: () => this.#runtime.client,
      get_permission_context: () => {
        const session_id = this.navigation.selected_session_id;
        const session = this.projection.application?.active_sessions.find(
          (candidate) => candidate.session_id === session_id,
        );
        return {
          session_id,
          workspace_id: session?.workspace_id ?? null,
        };
      },
      refresh_application: () => this.#runtime.loadApplication(),
    });
    this.memory_settings = new MemorySettingsStore({
      get_client: () => this.#runtime.client,
    });
    this.#session_management = new SessionManagementController({
      resources: this.resource_workspace,
      connection: this.connection,
      navigation: this.navigation,
      runtime: this.#runtime,
      save_preferences: () => this.#schedulePreferencesSave(),
      select_draft: (workspace_id) => this.openNewSessionDraft(workspace_id),
      select_session: (session_id) => this.selectSession(session_id),
      state: this,
    });
    this.#run_interaction = new RunInteractionController({
      connection: this.connection,
      runtime: this.#runtime,
      state: this,
    });
    makeObservable(this, {
      pending_session_action: observable,
      pending_workspace_action: observable,
      workspace_editor: observable,
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
      setSessionVariant: action,
      setSessionApprovalMode: action,
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

  connect(): Promise<void> {
    this.#initial_connection ??= this.#connectAndRestore();
    return this.#initial_connection;
  }

  async #connectAndRestore(): Promise<void> {
    this.#disposed = false;
    await this.initializePreferences();
    if (this.#disposed) return;
    await this.#runtime.connect();
    await this.#restoreResourceSnapshot();
  }

  retryConnection(): void {
    this.#runtime.retryConnection();
  }

  initializePreferences(): Promise<void> {
    this.#preferences_initialization ??= this.#loadPreferences();
    return this.#preferences_initialization;
  }

  async #loadPreferences(): Promise<void> {
    try {
      const preferences = await loadDesktopPreferences();
      if (this.#disposed) return;
      runInAction(() => {
        this.navigation.applyPreferences(preferences);
        this.desktop_lifecycle.applyPreferences(preferences);
      });
      this.#pending_snapshot = parseResourceSnapshot(preferences.resource_workspace);
      const scope = this.navigation.selected_session_id || this.navigation.selected_draft_key
        ? null : this.#pending_snapshot?.current_scope_key;
      if (scope?.startsWith("session:")) this.navigation.selectSession(scope.slice(8), false);
      else if (scope?.startsWith("draft:")) {
        const workspace = scope.startsWith("draft:workspace:") ? scope.slice(16) : null;
        this.openNewSessionDraft(workspace);
      }
    } catch (failure) {
      runInAction(() => { this.interaction_error = `桌面状态读取失败：${displayError(failure)}`; });
    }
  }

  async #restoreResourceSnapshot(): Promise<void> {
    if (this.#disposed) return;
    try {
      if (this.#pending_snapshot) await this.resource_workspace.restoreSnapshot(this.#pending_snapshot);
    } catch (failure) {
      runInAction(() => { this.interaction_error = `右栏恢复失败：${displayError(failure)}`; });
    } finally {
      this.#pending_snapshot = null;
      this.#preferences_ready = !this.#disposed;
    }
  }

  toggleWorkspace(workspace_id: WorkspaceId): void {
    this.navigation.toggleWorkspace(workspace_id);
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

  async selectSession(session_id: SessionId): Promise<void> {
    this.transient_focus.clear();
    this.navigation.selectSession(session_id);
    await this.#runtime.loadSession(session_id);
  }

  async openChildTask(session_id: SessionId, child_task_id: ChildTaskId): Promise<void> {
    this.transient_focus.clear();
    if (this.navigation.selected_session_id !== session_id) {
      await this.selectSession(session_id);
    }
    this.navigation.openChildTask(child_task_id);
    await this.#runtime.loadChildTask(session_id, child_task_id);
  }

  closeChildTask(): void {
    this.transient_focus.clear();
    this.navigation.closeChildTask();
  }

  async navigateBack(): Promise<void> {
    this.transient_focus.clear();
    const location = this.navigation.goBack();
    if (location) {
      const loaded = await this.#loadConversationLocation(location);
      if (!loaded) {
        this.navigation.goForward();
      }
    }
  }

  async navigateForward(): Promise<void> {
    this.transient_focus.clear();
    const location = this.navigation.goForward();
    if (location) {
      const loaded = await this.#loadConversationLocation(location);
      if (!loaded) {
        this.navigation.goBack();
      }
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
        revision !== this.#conversation_search_revision
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
      if (revision === this.#conversation_search_revision) {
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
      runInAction(() => this.conversation_search.applyRecall(result.payload));
    } catch (error: unknown) {
      runInAction(() => this.conversation_search.failRecall(displayError(error)));
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

  openNewSessionDraft(workspace_id: WorkspaceId | null = null): void {
    this.transient_focus.clear();
    if (workspace_id) {
      const workspace = this.projection.application?.workspaces.find((item) => (
        item.workspace_id === workspace_id && item.lifecycle === "active"
      ));
      if (!workspace) {
        this.interaction_error = "工作空间当前不可用，请选择其他工作空间。";
        return;
      }
    }
    const key = draftKeyForWorkspace(workspace_id);
    this.new_session_drafts.open(key, this.projection.application?.configuration.default_model ?? null);
    this.navigation.selectDraft(key);
    this.interaction_error = null;
    void this.#loadNewSessionDraftSkills(key);
  }

  async clearNewSessionDraft(key: NewSessionDraftKey): Promise<void> {
    const removed = this.new_session_drafts.remove(key);
    if (removed) await releaseDraftSelections(removed);
    if (this.navigation.selected_draft_key === key) {
      this.new_session_drafts.open(key, this.projection.application?.configuration.default_model ?? null);
    }
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
    const manifest = draft.materialization_attempt ?? materializationManifest(draft);
    if (!draft.materialization_attempt) {
      this.new_session_drafts.beginMaterialization(key, manifest);
    }
    this.composer_pending = true;
    this.interaction_error = null;
    this.new_session_drafts.setAttachmentTransferState(key, "uploading");
    try {
      const result = await materializeNewSession(manifest, createOperationId("materialize"));
      runInAction(() => {
        // 物化已可靠成功，先转移标签 owner 再改导航；活跃 PTY/浏览器不重新创建。
        this.resource_workspace.transferDraft(key, result.session.session_id);
        this.new_session_drafts.remove(key);
        this.navigation.selectSession(result.session.session_id, false);
        if (result.session.workspace_id) {
          this.navigation.ensureWorkspaceExpanded(result.session.workspace_id);
        }
      });
      try {
        await this.#runtime.loadApplication();
        await this.#runtime.loadSession(result.session.session_id);
      } catch (error: unknown) {
        runInAction(() => this.connection.markDisconnected(displayError(error)));
      }
      this.#schedulePreferencesSave();
      return true;
    } catch (error: unknown) {
      const message = displayError(error);
      runInAction(() => {
        if (!(error instanceof NativeResourceFailure && error.code === "materialization_response_unknown")) {
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

  async addWorkspace(): Promise<void> {
    if (this.pending_workspace_action || this.pending_session_action) return;
    const primary_directory = await this.#session_management.chooseWorkspaceDirectory();
    if (primary_directory) {
      runInAction(() => {
        this.workspace_editor = { mode: "create", primary_directory };
      });
    }
  }

  openWorkspaceEditor(workspace_id: WorkspaceId): void {
    this.workspace_editor = { mode: "edit", workspace_id };
  }

  closeWorkspaceEditor(): void {
    if (!this.pending_workspace_action) this.workspace_editor = null;
  }

  async chooseWorkspaceDirectory(): Promise<string | null> {
    return this.#session_management.chooseWorkspaceDirectory();
  }

  async saveWorkspaceEditor(input: Readonly<{
    label: string;
    primary_directory: string;
    additional_directories: string[];
  }>): Promise<boolean> {
    const editor = this.workspace_editor;
    if (!editor) return false;
    const saved = editor.mode === "create"
      ? await this.#session_management.registerWorkspace(input)
      : await this.#session_management.updateWorkspace({ workspace_id: editor.workspace_id, ...input }) !== null;
    if (saved) {
      runInAction(() => {
        this.workspace_editor = null;
      });
    }
    return saved;
  }

  async removeWorkspace(workspace_id: WorkspaceId): Promise<boolean> {
    const removed = await this.#session_management.removeWorkspace(workspace_id);
    if (removed) {
      const draft = this.new_session_drafts.remove(draftKeyForWorkspace(workspace_id));
      if (draft) await releaseDraftSelections(draft);
    }
    return removed;
  }

  async openWorkspace(workspace_id: WorkspaceId): Promise<void> {
    await this.#session_management.openWorkspace(workspace_id);
  }

  async openSessionWorkspaceDirectory(session_id: SessionId, directory_index: number): Promise<void> {
    await this.#session_management.openSessionWorkspaceDirectory(session_id, directory_index);
  }

  async openSessionResourceInSystem(
    session_id: SessionId,
    locator: SessionResourceLocator,
  ): Promise<void> {
    this.interaction_error = null;
    try {
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
    if (this.projection.session_views.get(session_id)?.session.role === "controller") {
      this.interaction_error = "请在普通会话中加入刷新队列";
      return false;
    }
    return this.#run_interaction.submitSessionCommand(session_id, command);
  }

  async setSessionModel(session_id: SessionId, model_key: ModelKey): Promise<boolean> {
    return this.#session_management.setSessionModel(session_id, model_key);
  }

  async setSessionVariant(session_id: SessionId, variant: AgentVariant): Promise<boolean> {
    return this.#session_management.setSessionVariant(session_id, variant);
  }

  async setSessionApprovalMode(session_id: SessionId, approval_mode: ApprovalMode): Promise<boolean> {
    return this.#session_management.setSessionApprovalMode(session_id, approval_mode);
  }

  async renameSession(session_id: SessionId, title: string): Promise<boolean> {
    return this.#session_management.renameSession(session_id, title);
  }

  async setSessionPinned(session_id: SessionId, is_pinned: boolean): Promise<boolean> {
    return this.#session_management.setSessionPinned(session_id, is_pinned);
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

  async setSessionReasoningEffort(session_id: SessionId, effort: import("../generated/assistant-protocol").ReasoningEffortKey | null): Promise<boolean> {
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
    const history = child_task_id
      ? this.projection.child_conversation_histories.get(child_task_id)
      : this.projection.conversation_histories.get(session_id);
    const began = child_task_id
      ? this.projection.beginLoadingPreviousChild(child_task_id)
      : this.projection.beginLoadingPrevious(session_id);
    if (!client || !history || !began) {
      return false;
    }
    try {
      const result = await client.command({
        type: "list_conversation_page",
        payload: {
          owner: history.owner,
          cursor: history.previous_cursor,
          limit: 30,
        },
      });
      return runInAction(() => child_task_id
        ? this.projection.applyPreviousChildConversationPage(child_task_id, result.payload.snapshot)
        : this.projection.applyPreviousConversationPage(session_id, result.payload.snapshot));
    } catch (error: unknown) {
      if (error instanceof RuntimeClientError && error.code === "snapshot_stale") {
        if (child_task_id) {
          await this.#runtime.loadChildTask(session_id, child_task_id);
        } else {
          await this.#runtime.loadSession(session_id);
        }
        return false;
      }
      runInAction(() => child_task_id
        ? this.projection.failLoadingPreviousChild(child_task_id, displayError(error))
        : this.projection.failLoadingPrevious(session_id, displayError(error)));
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
    if (!this.#runtime.client) {
      return false;
    }
    try {
      const result = await this.#runtime.client.command({
        type: "get_conversation_page_around_run",
        payload: { session_id, run_id, limit: 30 },
      });
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
      runInAction(() => this.projection.failLoadingPrevious(session_id, displayError(error)));
      return false;
    }
  }

  async #loadConversationLocation(
    location: ConversationLocation,
    expected_generation?: number,
  ): Promise<boolean> {
    if (!this.#runtime.client) {
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
      if (location.child_task_id) {
        await this.#runtime.loadChildTask(location.session_id, location.child_task_id);
      }

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

  async reloadSelectedConversation(): Promise<void> {
    const session_id = this.navigation.selected_session_id;
    if (session_id) {
      await this.#runtime.loadSession(session_id);
    }
  }

  dispose(): void {
    this.#flush_view_state();
    this.#disposed = true;
    this.#resource_snapshot_disposer();
    window.removeEventListener("scroll", this.#save_view_state, true);
    window.removeEventListener("pointerup", this.#save_view_state);
    window.removeEventListener("wheel", this.#save_view_state, true);
    window.removeEventListener("keyup", this.#save_view_state);
    window.removeEventListener("blur", this.#flush_view_state);
    window.removeEventListener("pagehide", this.#flush_view_state);
    this.transient_focus.clear();
    this.#resource_scope_disposer();
    this.resource_workspace.dispose();
    this.#runtime.dispose();
    this.device_gateway.dispose();
    this.settings.mcp.dispose();
    this.desktop_lifecycle.dispose();
    this.#runtime_state_disposer();
    for (const draft of this.new_session_drafts.clear()) {
      void releaseDraftSelections(draft);
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

  async flushPreferences(): Promise<void> {
    if (this.#preferences_save_timer !== null) window.clearTimeout(this.#preferences_save_timer);
    this.#preferences_save_timer = null;
    if (!this.#preferences_ready || this.#disposed || this.resource_workspace.shutting_down) return this.#preferences_pending;
    try {
      const preferences = {
        left_sidebar_open: this.navigation.left_sidebar_open,
        right_sidebar_open: this.navigation.right_sidebar_open,
        left_sidebar_width: this.navigation.left_sidebar_width,
        right_sidebar_width: this.navigation.right_sidebar_width,
        expanded_workspace_ids: [...this.navigation.expanded_workspaces],
        close_behavior: this.desktop_lifecycle.close_behavior,
        resource_workspace: parseResourceSnapshot(this.resource_workspace.captureSnapshot()),
      };
      const serialized = JSON.stringify(preferences);
      // 同一个 staging 文件只允许串行写入，后提交的快照不能被旧任务覆盖。
      const pending = this.#preferences_pending.catch(() => undefined).then(async () => {
        if (serialized === this.#last_saved_preferences) return;
        await saveDesktopPreferences(preferences);
        this.#last_saved_preferences = serialized;
      });
      this.#preferences_pending = pending;
      await pending;
    } catch (failure) {
      runInAction(() => { this.interaction_error = `桌面状态保存失败：${displayError(failure)}`; });
      throw failure;
    }
  }

  async #loadNewSessionDraftSkills(key: NewSessionDraftKey): Promise<void> {
    const client = this.#runtime.client;
    const draft = this.new_session_drafts.get(key);
    if (!client || !draft || !this.new_session_drafts.beginSkillLoad(key)) return;
    try {
      const result = await client.command({
        type: "list_skills",
        payload: draft.workspace_id ? { workspace_id: draft.workspace_id } : {},
      });
      runInAction(() => {
        this.new_session_drafts.applySkillOptions(key, result.payload.snapshot.skills);
      });
    } catch {
      runInAction(() => this.new_session_drafts.failSkillLoad(key));
    }
  }
}

function materializationManifest(draft: NewSessionDraft): SessionMaterializationManifest {
  return {
    idempotency_key: createOperationId("new-session"),
    ...(draft.workspace_id ? { workspace_id: draft.workspace_id } : {}),
    ...(draft.model_key ? { model_key: draft.model_key } : {}),
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
    ...(draft.selected_mcp ? { mcp_server_key: draft.selected_mcp.server_key } : {}),
  };
}

async function releaseDraftSelections(draft: NewSessionDraft): Promise<void> {
  await Promise.allSettled(
    draft.attachments.map((attachment) => releaseAttachmentSelection(attachment.selection_id)),
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
