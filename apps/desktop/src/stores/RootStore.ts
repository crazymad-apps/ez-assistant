import { action, makeObservable, observable, reaction, runInAction, type IReactionDisposer } from "mobx";
import type {
  AgentVariant,
  AttachmentId,
  ApprovalDecision,
  ApprovalId,
  ApprovalMode,
  ChildTaskId,
  ConversationOwner,
  InputId,
  MessageId,
  MessageFeedback,
  ModelKey,
  PrepareDeleteSessionResult,
  RunId,
  SessionId,
  ToolCallId,
  ToolDetailSnapshot,
  WorkspaceId,
} from "../generated/assistant-protocol";
import { loadDesktopPreferences, saveDesktopPreferences } from "../native-bridge/desktopPreferences";
import { RuntimeClientError } from "../runtime-client/RuntimeClient";
import { ConnectionStore } from "./ConnectionStore";
import { DesktopLifecycleStore } from "./DesktopLifecycleStore";
import { LiveExecutionStore } from "./LiveExecutionStore";
import { NavigationStore } from "./NavigationStore";
import { RuntimeLifecycleCoordinator } from "./RuntimeLifecycleCoordinator";
import { RuntimeProjectionStore } from "./RuntimeProjectionStore";
import { RunInteractionController } from "./RunInteractionController";
import { SessionManagementController } from "./SessionManagementController";
import { SettingsStore } from "./SettingsStore";

export class RootStore {
  readonly connection = new ConnectionStore();
  readonly projection = new RuntimeProjectionStore();
  readonly live_execution = new LiveExecutionStore();
  readonly navigation = new NavigationStore();
  readonly settings: SettingsStore;
  readonly desktop_lifecycle: DesktopLifecycleStore;
  pending_session_action = false;
  pending_workspace_action = false;
  composer_pending = false;
  interaction_error: string | null = null;
  pending_queue_input_id: InputId | null = null;
  pending_approval_id: ApprovalId | null = null;

  readonly #runtime: RuntimeLifecycleCoordinator;
  readonly #run_interaction: RunInteractionController;
  readonly #session_management: SessionManagementController;
  #disposed = false;
  #preferences_save_timer: number | null = null;
  #runtime_state_disposer: IReactionDisposer;

  constructor() {
    this.#runtime = new RuntimeLifecycleCoordinator({
      connection: this.connection,
      live_execution: this.live_execution,
      navigation: this.navigation,
      projection: this.projection,
      report_interaction_error: (message) => {
        this.interaction_error = message;
      },
    });
    this.desktop_lifecycle = new DesktopLifecycleStore({
      get_application: () => this.projection.application,
      prepare_runtime_mutation: (kind) => this.#runtime.prepareForNativeRuntimeMutation(kind),
      reconnect_runtime: (bootstrap) => this.#runtime.reconnectAfterNativeRuntimeMutation(bootstrap),
      mark_runtime_stopped: () => this.connection.markRuntimeStopped(),
      save_preferences: () => this.#schedulePreferencesSave(),
    });
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
    this.#session_management = new SessionManagementController({
      connection: this.connection,
      navigation: this.navigation,
      projection: this.projection,
      runtime: this.#runtime,
      save_preferences: () => this.#schedulePreferencesSave(),
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
      composer_pending: observable,
      interaction_error: observable,
      pending_queue_input_id: observable,
      pending_approval_id: observable,
      connect: action,
      retryConnection: action,
      initializePreferences: action,
      selectSession: action,
      openChildTask: action,
      closeChildTask: action,
      cancelChildTask: action,
      createSession: action,
      forkSession: action,
      prepareDeleteSession: action,
      deleteSession: action,
      addWorkspace: action,
      changeSessionWorkspace: action,
      openWorkspace: action,
      copyWorkspacePath: action,
      submitInput: action,
      exportSession: action,
      setSessionModel: action,
      setSessionVariant: action,
      setSessionApprovalMode: action,
      renameSession: action,
      setSessionPinned: action,
      setMessageFeedback: action,
      archiveSession: action,
      prioritizeQueuedInput: action,
      cancelQueuedInput: action,
      resumeQueuedInput: action,
      interruptRun: action,
      decideApproval: action,
      rejectApprovalAndStopRun: action,
      restoreSession: action,
      clearInteractionError: action,
      showInteractionError: action,
      loadPreviousConversationPage: action,
      locateConversationRun: action,
      toggleWorkspace: action,
      toggleLeftSidebar: action,
      toggleRightSidebar: action,
      dispose: action,
    });
  }

  connect(): Promise<void> {
    this.#disposed = false;
    return this.#runtime.connect();
  }

  retryConnection(): void {
    this.#runtime.retryConnection();
  }

  async initializePreferences(): Promise<void> {
    try {
      const preferences = await loadDesktopPreferences();
      if (!this.#disposed) {
        runInAction(() => {
          this.navigation.applyPreferences(preferences);
          this.desktop_lifecycle.applyPreferences(preferences);
        });
      }
    } catch {
      // Missing/corrupt device preferences intentionally fall back to design defaults.
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

  async selectSession(session_id: SessionId): Promise<void> {
    this.navigation.selectSession(session_id);
    await this.#runtime.loadSession(session_id);
  }

  async openChildTask(session_id: SessionId, child_task_id: ChildTaskId): Promise<void> {
    if (this.navigation.selected_session_id !== session_id) {
      await this.selectSession(session_id);
    }
    this.navigation.openChildTask(child_task_id);
    await this.#runtime.loadChildTask(session_id, child_task_id);
  }

  closeChildTask(): void {
    this.navigation.closeChildTask();
  }

  async cancelChildTask(session_id: SessionId, child_task_id: ChildTaskId): Promise<void> {
    await this.#run_interaction.cancelChildTask(session_id, child_task_id);
  }

  async createSession(workspace_id?: WorkspaceId | null): Promise<void> {
    await this.#session_management.createSession(workspace_id);
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

  async addWorkspace(): Promise<void> {
    await this.#session_management.addWorkspace();
  }

  async changeSessionWorkspace(session_id: SessionId): Promise<void> {
    await this.#session_management.changeSessionWorkspace(session_id);
  }

  async openWorkspace(workspace_id: WorkspaceId): Promise<void> {
    await this.#session_management.openWorkspace(workspace_id);
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
  ): Promise<boolean> {
    return this.#run_interaction.submitInput(session_id, message, variant, attachment_ids);
  }

  async exportSession(session_id: SessionId, title: string): Promise<boolean> {
    return this.#session_management.exportSession(session_id, title);
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

  async interruptRun(session_id: SessionId, run_id: RunId): Promise<void> {
    await this.#run_interaction.interruptRun(session_id, run_id);
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
      return runInAction(() => {
        const applied = this.projection.applyLocatedConversationPage(session_id, result.payload.snapshot);
        if (applied) {
          this.navigation.requestConversationAnchor(result.payload.anchor_message_id);
        }
        return applied;
      });
    } catch (error: unknown) {
      runInAction(() => this.projection.failLoadingPrevious(session_id, displayError(error)));
      return false;
    }
  }

  async reloadSelectedConversation(): Promise<void> {
    const session_id = this.navigation.selected_session_id;
    if (session_id) {
      await this.#runtime.loadSession(session_id);
    }
  }

  dispose(): void {
    this.#disposed = true;
    this.#runtime.dispose();
    this.desktop_lifecycle.dispose();
    this.#runtime_state_disposer();
    if (this.#preferences_save_timer !== null) {
      window.clearTimeout(this.#preferences_save_timer);
      this.#preferences_save_timer = null;
    }
  }

  #schedulePreferencesSave(): void {
    if (this.#preferences_save_timer !== null) {
      window.clearTimeout(this.#preferences_save_timer);
    }
    this.#preferences_save_timer = window.setTimeout(() => {
      this.#preferences_save_timer = null;
      void saveDesktopPreferences({
        left_sidebar_open: this.navigation.left_sidebar_open,
        right_sidebar_open: this.navigation.right_sidebar_open,
        expanded_workspace_ids: [...this.navigation.expanded_workspaces],
        close_behavior: this.desktop_lifecycle.close_behavior,
      }).catch(() => undefined);
    }, 120);
  }
}

function displayError(error: unknown): string {
  return error instanceof Error ? error.message : "无法连接本地 Runtime。";
}
