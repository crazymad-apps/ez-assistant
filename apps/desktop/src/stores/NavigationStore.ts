import { action, computed, makeObservable, observable } from "mobx";
import type { ChildTaskId, MessageId, SessionId, WorkspaceId } from "../generated/assistant-protocol";
import type { NewSessionDraftKey } from "./NewSessionDraftStore";

export type ConversationLocation = Readonly<{
  session_id: SessionId;
  child_task_id: ChildTaskId | null;
  anchor_message_id: MessageId | null;
  scroll_offset: number | null;
}>;

export const LEFT_SIDEBAR_DEFAULT_WIDTH = 286;
export const LEFT_SIDEBAR_MIN_WIDTH = 220;
export const LEFT_SIDEBAR_MAX_WIDTH = 420;
export const RIGHT_SIDEBAR_DEFAULT_WIDTH = 326;
export const RIGHT_SIDEBAR_MIN_WIDTH = 220;
export const RIGHT_SIDEBAR_MAX_WIDTH = 800;
export const CONVERSATION_MIN_WIDTH = 500;

export class NavigationStore {
  selected_session_id: SessionId | null = null;
  selected_draft_key: NewSessionDraftKey | null = null;
  selected_child_task_id: ChildTaskId | null = null;
  search_query = "";
  list_mode: "active" | "archived" = "active";
  left_sidebar_open = true;
  right_sidebar_open = true;
  left_sidebar_width = LEFT_SIDEBAR_DEFAULT_WIDTH;
  right_sidebar_width = RIGHT_SIDEBAR_DEFAULT_WIDTH;
  viewport_width = 1280;
  responsive_priority: "left" | "right" = "left";
  conversation_anchor_message_id: MessageId | null = null;
  readonly conversation_history = observable.array<ConversationLocation>([], { deep: false });
  conversation_history_index = -1;
  workspace_expansion_initialized = false;
  readonly expanded_workspaces = observable.set<WorkspaceId>();

  constructor() {
    makeObservable(this, {
      selected_session_id: observable,
      selected_draft_key: observable,
      selected_child_task_id: observable,
      search_query: observable,
      list_mode: observable,
      left_sidebar_open: observable,
      right_sidebar_open: observable,
      left_sidebar_width: observable,
      right_sidebar_width: observable,
      viewport_width: observable,
      responsive_priority: observable,
      effective_left_sidebar_open: computed,
      effective_right_sidebar_open: computed,
      effective_left_sidebar_width: computed,
      effective_right_sidebar_width: computed,
      left_sidebar_current_max_width: computed,
      right_sidebar_current_max_width: computed,
      conversation_anchor_message_id: observable,
      conversation_history_index: observable,
      can_go_back: computed,
      can_go_forward: computed,
      current_conversation_location: computed,
      workspace_expansion_initialized: observable,
      expanded_workspaces: observable,
      selectSession: action,
      selectDraft: action,
      openChildTask: action,
      closeChildTask: action,
      setSearchQuery: action,
      setListMode: action,
      toggleWorkspace: action,
      toggleLeftSidebar: action,
      toggleRightSidebar: action,
      setSidebarWidth: action,
      resetSidebarWidth: action,
      resetSidebarLayout: action,
      setViewportWidth: action,
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

  get effective_left_sidebar_open(): boolean {
    return this.#effectiveSidebarLayout().left_open;
  }

  get effective_right_sidebar_open(): boolean {
    return this.#effectiveSidebarLayout().right_open;
  }

  get effective_left_sidebar_width(): number {
    return this.#effectiveSidebarLayout().left_width;
  }

  get effective_right_sidebar_width(): number {
    return this.#effectiveSidebarLayout().right_width;
  }

  get left_sidebar_current_max_width(): number {
    const other_width = this.effective_right_sidebar_open ? this.effective_right_sidebar_width : 0;
    return Math.max(
      LEFT_SIDEBAR_MIN_WIDTH,
      Math.min(LEFT_SIDEBAR_MAX_WIDTH, this.viewport_width - other_width - CONVERSATION_MIN_WIDTH),
    );
  }

  get right_sidebar_current_max_width(): number {
    const other_width = this.effective_left_sidebar_open ? this.effective_left_sidebar_width : 0;
    return Math.max(
      RIGHT_SIDEBAR_MIN_WIDTH,
      Math.min(RIGHT_SIDEBAR_MAX_WIDTH, this.viewport_width - other_width - CONVERSATION_MIN_WIDTH),
    );
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
    this.selected_draft_key = null;
    this.selected_child_task_id = null;
    this.conversation_anchor_message_id = null;
  }

  /** 选择一个纯客户端草稿；它不进入 Session 导航历史，也不伪造 Conversation location。 */
  selectDraft(key: NewSessionDraftKey): void {
    this.selected_session_id = null;
    this.selected_draft_key = key;
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
    if (this.effective_left_sidebar_open) {
      this.left_sidebar_open = false;
      return;
    }
    this.left_sidebar_open = true;
    this.responsive_priority = "left";
  }

  toggleRightSidebar(): void {
    if (this.effective_right_sidebar_open) {
      this.right_sidebar_open = false;
      return;
    }
    this.right_sidebar_open = true;
    this.responsive_priority = "right";
  }

  setSidebarWidth(side: "left" | "right", width: number): void {
    const rounded_width = Math.round(width);
    if (side === "left") {
      this.left_sidebar_width = clamp(rounded_width, LEFT_SIDEBAR_MIN_WIDTH, this.left_sidebar_current_max_width);
    } else {
      this.right_sidebar_width = clamp(rounded_width, RIGHT_SIDEBAR_MIN_WIDTH, this.right_sidebar_current_max_width);
    }
  }

  resetSidebarWidth(side: "left" | "right"): void {
    if (side === "left") {
      this.left_sidebar_width = LEFT_SIDEBAR_DEFAULT_WIDTH;
    } else {
      this.right_sidebar_width = RIGHT_SIDEBAR_DEFAULT_WIDTH;
    }
  }

  resetSidebarLayout(): void {
    this.left_sidebar_open = true;
    this.right_sidebar_open = true;
    this.left_sidebar_width = LEFT_SIDEBAR_DEFAULT_WIDTH;
    this.right_sidebar_width = RIGHT_SIDEBAR_DEFAULT_WIDTH;
    this.responsive_priority = "left";
  }

  setViewportWidth(width: number): void {
    this.viewport_width = Math.max(0, Math.round(width));
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
    readonly left_sidebar_width: number;
    readonly right_sidebar_width: number;
    readonly expanded_workspace_ids: readonly WorkspaceId[] | null;
  }): void {
    this.left_sidebar_open = preferences.left_sidebar_open;
    this.right_sidebar_open = preferences.right_sidebar_open;
    this.left_sidebar_width = clamp(
      Number.isFinite(preferences.left_sidebar_width) ? preferences.left_sidebar_width : LEFT_SIDEBAR_DEFAULT_WIDTH,
      LEFT_SIDEBAR_MIN_WIDTH,
      LEFT_SIDEBAR_MAX_WIDTH,
    );
    this.right_sidebar_width = clamp(
      Number.isFinite(preferences.right_sidebar_width) ? preferences.right_sidebar_width : RIGHT_SIDEBAR_DEFAULT_WIDTH,
      RIGHT_SIDEBAR_MIN_WIDTH,
      RIGHT_SIDEBAR_MAX_WIDTH,
    );
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
    this.selected_draft_key = null;
    this.selected_child_task_id = location.child_task_id;
    this.conversation_anchor_message_id = location.anchor_message_id;
  }

  #effectiveSidebarLayout(): Readonly<{
    left_open: boolean;
    left_width: number;
    right_open: boolean;
    right_width: number;
  }> {
    const left = this.left_sidebar_open;
    const right = this.right_sidebar_open;
    const closed = { left_open: false, left_width: 0, right_open: false, right_width: 0 };
    if (!left && !right) return closed;

    const preferred_left_fits = this.viewport_width >= this.left_sidebar_width + CONVERSATION_MIN_WIDTH;
    const preferred_right_fits = this.viewport_width >= this.right_sidebar_width + CONVERSATION_MIN_WIDTH;
    if (left && right) {
      const preferred_widths_fit = this.viewport_width
        >= this.left_sidebar_width + this.right_sidebar_width + CONVERSATION_MIN_WIDTH;
      if (!preferred_widths_fit) {
        if (this.responsive_priority === "right" && preferred_right_fits) {
          return {
            ...closed,
            right_open: true,
            right_width: this.right_sidebar_width,
          };
        }
        if (preferred_left_fits) {
          return {
            ...closed,
            left_open: true,
            left_width: this.left_sidebar_width,
          };
        }
        return closed;
      }
      return {
        left_open: true,
        left_width: this.left_sidebar_width,
        right_open: true,
        right_width: this.right_sidebar_width,
      };
    }
    if (left && preferred_left_fits) {
      return {
        ...closed,
        left_open: true,
        left_width: this.left_sidebar_width,
      };
    }
    if (right && preferred_right_fits) {
      return {
        ...closed,
        right_open: true,
        right_width: this.right_sidebar_width,
      };
    }
    return closed;
  }
}

function clamp(value: number, minimum: number, maximum: number): number {
  if (!Number.isFinite(value)) return minimum;
  return Math.min(maximum, Math.max(minimum, Math.round(value)));
}
