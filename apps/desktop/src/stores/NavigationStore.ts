import { action, makeObservable, observable } from "mobx";
import type { ChildTaskId, MessageId, SessionId, WorkspaceId } from "../generated/assistant-protocol";

export class NavigationStore {
  selected_session_id: SessionId | null = null;
  selected_child_task_id: ChildTaskId | null = null;
  search_query = "";
  list_mode: "active" | "archived" = "active";
  left_sidebar_open = true;
  right_sidebar_open = true;
  conversation_anchor_message_id: MessageId | null = null;
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
      ensureWorkspaceExpanded: action,
      applyPreferences: action,
    });
  }

  selectSession(session_id: SessionId | null): void {
    this.selected_session_id = session_id;
    this.selected_child_task_id = null;
    this.conversation_anchor_message_id = null;
  }

  openChildTask(child_task_id: ChildTaskId): void {
    this.selected_child_task_id = child_task_id;
    this.conversation_anchor_message_id = null;
  }

  closeChildTask(): void {
    this.selected_child_task_id = null;
    this.conversation_anchor_message_id = null;
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
}
