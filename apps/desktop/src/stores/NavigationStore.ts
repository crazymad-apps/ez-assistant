import { action, computed, makeObservable, observable } from "mobx";
import type { ChildTaskId, MessageId, SessionId, WorkspaceId } from "../generated/assistant-protocol";

export type ConversationLocation = Readonly<{
  session_id: SessionId;
  child_task_id: ChildTaskId | null;
  anchor_message_id: MessageId | null;
  scroll_offset: number | null;
}>;

export class NavigationStore {
  selected_session_id: SessionId | null = null;
  selected_child_task_id: ChildTaskId | null = null;
  search_query = "";
  list_mode: "active" | "archived" = "active";
  left_sidebar_open = true;
  right_sidebar_open = true;
  conversation_anchor_message_id: MessageId | null = null;
  readonly conversation_history = observable.array<ConversationLocation>([], { deep: false });
  conversation_history_index = -1;
  workspace_expansion_initialized = false;
  readonly expanded_workspaces = observable.set<WorkspaceId>();

  constructor() {
    makeObservable(this, {
      selected_session_id: observable,
      selected_child_task_id: observable,
      search_query: observable,
      list_mode: observable,
      left_sidebar_open: observable,
      right_sidebar_open: observable,
      conversation_anchor_message_id: observable,
      conversation_history_index: observable,
      can_go_back: computed,
      can_go_forward: computed,
      current_conversation_location: computed,
      workspace_expansion_initialized: observable,
      expanded_workspaces: observable,
      selectSession: action,
      openChildTask: action,
      closeChildTask: action,
      setSearchQuery: action,
      setListMode: action,
      toggleWorkspace: action,
      toggleLeftSidebar: action,
      toggleRightSidebar: action,
      requestConversationAnchor: action,
      consumeConversationAnchor: action,
      goBack: action,
      goForward: action,
      navigateTo: action,
      updateCurrentScrollOffset: action,
      ensureWorkspaceExpanded: action,
      applyPreferences: action,
    });
  }

  get can_go_back(): boolean {
    return this.conversation_history_index > 0;
  }

  get can_go_forward(): boolean {
    return this.conversation_history_index >= 0
      && this.conversation_history_index < this.conversation_history.length - 1;
  }

  get current_conversation_location(): ConversationLocation | null {
    return this.conversation_history[this.conversation_history_index] ?? null;
  }

  selectSession(session_id: SessionId | null, record_history = true): void {
    if (session_id && record_history) {
      this.navigateTo({
        session_id,
        child_task_id: null,
        anchor_message_id: null,
        scroll_offset: null,
      });
      return;
    }
    if (session_id && this.conversation_history.length === 0) {
      this.conversation_history.push({
        session_id,
        child_task_id: null,
        anchor_message_id: null,
        scroll_offset: null,
      });
      this.conversation_history_index = 0;
    }
    this.selected_session_id = session_id;
    this.selected_child_task_id = null;
    this.conversation_anchor_message_id = null;
  }

  openChildTask(child_task_id: ChildTaskId, record_history = true): void {
    if (this.selected_session_id && record_history) {
      this.navigateTo({
        session_id: this.selected_session_id,
        child_task_id,
        anchor_message_id: null,
        scroll_offset: null,
      });
      return;
    }
    this.selected_child_task_id = child_task_id;
    this.conversation_anchor_message_id = null;
  }

  closeChildTask(record_history = true): void {
    if (this.selected_session_id && record_history) {
      this.navigateTo({
        session_id: this.selected_session_id,
        child_task_id: null,
        anchor_message_id: null,
        scroll_offset: null,
      });
      return;
    }
    this.selected_child_task_id = null;
    this.conversation_anchor_message_id = null;
  }

  /**
   * 记录一次 UI 来源跳转。栈只保存可恢复的视图位置，不参与 Runtime 业务状态。
   */
  navigateTo(location: ConversationLocation): void {
    const current = this.conversation_history[this.conversation_history_index];
    if (
      current
      && current.session_id === location.session_id
      && current.child_task_id === location.child_task_id
      && current.anchor_message_id === location.anchor_message_id
    ) {
      this.#applyLocation(location);
      return;
    }
    if (this.conversation_history_index < this.conversation_history.length - 1) {
      this.conversation_history.splice(this.conversation_history_index + 1);
    }
    this.conversation_history.push(location);
    this.conversation_history_index = this.conversation_history.length - 1;
    this.#applyLocation(location);
  }

  goBack(): ConversationLocation | null {
    if (!this.can_go_back) {
      return null;
    }
    this.conversation_history_index -= 1;
    const location = this.conversation_history[this.conversation_history_index] ?? null;
    if (location) {
      this.#applyLocation(location);
    }
    return location;
  }

  goForward(): ConversationLocation | null {
    if (!this.can_go_forward) {
      return null;
    }
    this.conversation_history_index += 1;
    const location = this.conversation_history[this.conversation_history_index] ?? null;
    if (location) {
      this.#applyLocation(location);
    }
    return location;
  }

  updateCurrentScrollOffset(scroll_offset: number): void {
    const current = this.conversation_history[this.conversation_history_index];
    if (!current) {
      return;
    }
    this.conversation_history[this.conversation_history_index] = {
      ...current,
      scroll_offset: Math.max(0, scroll_offset),
    };
  }

  setSearchQuery(query: string): void {
    this.search_query = query;
  }

  setListMode(mode: "active" | "archived"): void {
    this.list_mode = mode;
  }

  toggleWorkspace(workspace_id: WorkspaceId): void {
    this.workspace_expansion_initialized = true;
    if (this.expanded_workspaces.has(workspace_id)) {
      this.expanded_workspaces.delete(workspace_id);
    } else {
      this.expanded_workspaces.add(workspace_id);
    }
  }

  ensureWorkspaceExpanded(workspace_id: WorkspaceId): void {
    this.expanded_workspaces.add(workspace_id);
  }

  toggleLeftSidebar(): void {
    this.left_sidebar_open = !this.left_sidebar_open;
  }

  toggleRightSidebar(): void {
    this.right_sidebar_open = !this.right_sidebar_open;
  }

  requestConversationAnchor(message_id: MessageId): void {
    this.conversation_anchor_message_id = message_id;
  }

  consumeConversationAnchor(message_id: MessageId): void {
    if (this.conversation_anchor_message_id === message_id) {
      this.conversation_anchor_message_id = null;
    }
  }

  applyPreferences(preferences: {
    readonly left_sidebar_open: boolean;
    readonly right_sidebar_open: boolean;
    readonly expanded_workspace_ids: readonly WorkspaceId[] | null;
  }): void {
    this.left_sidebar_open = preferences.left_sidebar_open;
    this.right_sidebar_open = preferences.right_sidebar_open;
    this.expanded_workspaces.clear();
    this.workspace_expansion_initialized = preferences.expanded_workspace_ids !== null;
    if (preferences.expanded_workspace_ids) {
      for (const workspace_id of preferences.expanded_workspace_ids) {
        this.expanded_workspaces.add(workspace_id);
      }
    }
  }

  #applyLocation(location: ConversationLocation): void {
    this.selected_session_id = location.session_id;
    this.selected_child_task_id = location.child_task_id;
    this.conversation_anchor_message_id = location.anchor_message_id;
  }
}
