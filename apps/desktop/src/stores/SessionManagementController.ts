import { runInAction } from "mobx";
import type {
  AgentVariant,
  ApprovalMode,
  MessageFeedback,
  MessageId,
  ModelKey,
  PrepareDeleteSessionResult,
  SessionId,
  WorkspaceId,
} from "../generated/assistant-protocol";
import { exportSessionMarkdown } from "../native-bridge/nativeResource";
import { openWorkspaceDirectory } from "../native-bridge/openWorkspaceDirectory";
import { chooseWorkspaceDirectory } from "../native-bridge/workspaceDirectory";
import type { ConnectionStore } from "./ConnectionStore";
import type { NavigationStore } from "./NavigationStore";
import type { RuntimeLifecycleCoordinator } from "./RuntimeLifecycleCoordinator";

type SessionManagementState = {
  composer_pending: boolean;
  interaction_error: string | null;
  pending_session_action: boolean;
  pending_workspace_action: boolean;
};

type SessionManagementDependencies = Readonly<{
  connection: ConnectionStore;
  navigation: NavigationStore;
  runtime: RuntimeLifecycleCoordinator;
  save_preferences: () => void;
  select_session: (session_id: SessionId) => Promise<void>;
  state: SessionManagementState;
}>;

/** Handles session and workspace commands while RootStore remains the UI-facing facade. */
export class SessionManagementController {
  constructor(private readonly dependencies: SessionManagementDependencies) {}

  async createSession(workspace_id?: WorkspaceId | null): Promise<void> {
    const { connection, runtime, state } = this.dependencies;
    if (
      !runtime.client
      || connection.state !== "connected"
      || state.pending_session_action
      || state.pending_workspace_action
    ) {
      return;
    }
    state.pending_session_action = true;
    try {
      const result = await runtime.client.command({
        type: "create_session",
        payload: { title: null, model_key: null, workspace_id: workspace_id ?? null },
      });
      await runtime.loadApplication();
      await this.dependencies.select_session(result.payload.session.session_id);
    } catch (error: unknown) {
      connection.markDisconnected(displayError(error));
    } finally {
      runInAction(() => {
        state.pending_session_action = false;
      });
    }
  }

  async forkSession(
    session_id: SessionId,
    fork_point: MessageId,
    expected_generation: number,
  ): Promise<SessionId | null> {
    const { connection, runtime, state } = this.dependencies;
    const client = runtime.client;
    if (!client || connection.state !== "connected" || state.pending_session_action || state.composer_pending) {
      return null;
    }
    state.pending_session_action = true;
    state.interaction_error = null;
    try {
      const result = await client.command({
        type: "fork_session",
        payload: { session_id, fork_point, expected_generation },
      });
      const forked_session_id = result.payload.session.session_id;
      await runtime.loadApplication();
      await this.dependencies.select_session(forked_session_id);
      return forked_session_id;
    } catch (error: unknown) {
      runInAction(() => {
        state.interaction_error = displayError(error);
      });
      return null;
    } finally {
      runInAction(() => {
        state.pending_session_action = false;
      });
    }
  }

  async prepareDeleteSession(session_id: SessionId): Promise<PrepareDeleteSessionResult | null> {
    const { connection, runtime, state } = this.dependencies;
    const client = runtime.client;
    if (!client || connection.state !== "connected" || state.pending_session_action || state.composer_pending) {
      return null;
    }
    state.pending_session_action = true;
    state.interaction_error = null;
    try {
      const result = await client.command({ type: "prepare_delete_session", payload: { session_id } });
      return result.payload;
    } catch (error: unknown) {
      runInAction(() => {
        state.interaction_error = displayError(error);
      });
      return null;
    } finally {
      runInAction(() => {
        state.pending_session_action = false;
      });
    }
  }

  async deleteSession(prepared: PrepareDeleteSessionResult): Promise<boolean> {
    const { connection, navigation, runtime, state } = this.dependencies;
    const client = runtime.client;
    if (!client || connection.state !== "connected" || state.pending_session_action || state.composer_pending) {
      return false;
    }
    state.pending_session_action = true;
    state.interaction_error = null;
    try {
      await client.command({
        type: "delete_session",
        payload: {
          session_id: prepared.session.session_id,
          confirmation_token: prepared.confirmation_token,
        },
      });
      if (navigation.selected_session_id === prepared.session.session_id) {
        navigation.selectSession(null, false);
      }
      await runtime.loadApplication();
      await runtime.selectInitialSession();
      return true;
    } catch (error: unknown) {
      runInAction(() => {
        state.interaction_error = displayError(error);
      });
      return false;
    } finally {
      runInAction(() => {
        state.pending_session_action = false;
      });
    }
  }

  async addWorkspace(): Promise<void> {
    const { connection, navigation, runtime, state } = this.dependencies;
    const client = runtime.client;
    if (!client || connection.state !== "connected" || state.pending_workspace_action || state.pending_session_action) {
      return;
    }
    state.pending_workspace_action = true;
    state.interaction_error = null;
    try {
      const path = await chooseWorkspaceDirectory();
      if (!path) {
        return;
      }
      const workspace_result = await client.command({ type: "register_workspace", payload: { path } });
      const workspace_id = workspace_result.payload.workspace.workspace_id;
      await runtime.loadApplication();
      navigation.ensureWorkspaceExpanded(workspace_id);
      this.dependencies.save_preferences();
      const session_result = await client.command({
        type: "create_session",
        payload: { title: null, model_key: null, workspace_id },
      });
      await runtime.loadApplication();
      await this.dependencies.select_session(session_result.payload.session.session_id);
    } catch (error: unknown) {
      runInAction(() => {
        state.interaction_error = displayError(error);
      });
    } finally {
      runInAction(() => {
        state.pending_workspace_action = false;
      });
    }
  }

  async openWorkspace(workspace_id: WorkspaceId): Promise<void> {
    this.dependencies.state.interaction_error = null;
    try {
      await openWorkspaceDirectory(workspace_id);
    } catch (error: unknown) {
      runInAction(() => {
        this.dependencies.state.interaction_error = displayError(error);
      });
    }
  }

  async copyWorkspacePath(path: string): Promise<void> {
    this.dependencies.state.interaction_error = null;
    try {
      await navigator.clipboard.writeText(path);
    } catch (error: unknown) {
      runInAction(() => {
        this.dependencies.state.interaction_error = displayError(error);
      });
    }
  }

  async exportSession(session_id: SessionId, title: string): Promise<boolean> {
    const state = this.dependencies.state;
    if (state.pending_session_action) {
      return false;
    }
    state.pending_session_action = true;
    state.interaction_error = null;
    try {
      return await exportSessionMarkdown(session_id, title);
    } catch (error: unknown) {
      runInAction(() => {
        state.interaction_error = displayError(error);
      });
      return false;
    } finally {
      runInAction(() => {
        state.pending_session_action = false;
      });
    }
  }

  setSessionModel(session_id: SessionId, model_key: ModelKey): Promise<boolean> {
    return this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "set_session_model",
      payload: { session_id, model_key },
    }));
  }

  setSessionVariant(session_id: SessionId, variant: AgentVariant): Promise<boolean> {
    return this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "set_session_variant",
      payload: { session_id, variant },
    }));
  }

  setSessionApprovalMode(session_id: SessionId, approval_mode: ApprovalMode): Promise<boolean> {
    return this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "set_session_approval_mode",
      payload: { session_id, approval_mode },
    }));
  }

  renameSession(session_id: SessionId, title: string): Promise<boolean> {
    return this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "rename_session",
      payload: { session_id, title },
    }));
  }

  setSessionPinned(session_id: SessionId, is_pinned: boolean): Promise<boolean> {
    return this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "set_session_pinned",
      payload: { session_id, is_pinned },
    }));
  }

  setMessageFeedback(
    session_id: SessionId,
    message_id: MessageId,
    feedback: MessageFeedback | null,
  ): Promise<boolean> {
    return this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "set_message_feedback",
      payload: { session_id, message_id, feedback },
    }));
  }

  async archiveSession(session_id: SessionId): Promise<boolean> {
    const archived = await this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "archive_session",
      payload: { session_id },
    }));
    if (archived) {
      this.dependencies.navigation.setListMode("archived");
    }
    return archived;
  }

  async restoreSession(session_id: SessionId): Promise<void> {
    const restored = await this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "restore_session",
      payload: { session_id },
    }));
    if (restored) {
      await this.dependencies.runtime.loadApplication();
    }
  }

  async #runSessionMutation(session_id: SessionId, mutation: () => Promise<unknown>): Promise<boolean> {
    const { connection, runtime, state } = this.dependencies;
    if (!runtime.client || connection.state !== "connected" || state.composer_pending) {
      return false;
    }
    state.composer_pending = true;
    state.interaction_error = null;
    try {
      await mutation();
      await runtime.loadSession(session_id);
      await runtime.loadApplication();
      return true;
    } catch (error: unknown) {
      runInAction(() => {
        state.interaction_error = displayError(error);
      });
      await runtime.loadSession(session_id);
      return false;
    } finally {
      runInAction(() => {
        state.composer_pending = false;
      });
    }
  }
}

function displayError(error: unknown): string {
  return error instanceof Error ? error.message : "无法连接本地 Runtime。";
}
