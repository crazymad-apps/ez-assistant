import { childKey } from "./liveExecutionProjection";
import { action, makeObservable, observable, observableRef } from "mobx";
import type {
  ApplicationSnapshot,
  ChildTaskId,
  ChildTaskViewSnapshot,
  ConversationItem,
  ConversationOwner,
  ConversationPage,
  ObservedSnapshot,
  RuntimeEventEnvelope,
  RunSnapshot,
  SessionId,
  SessionSummary,
  SessionGroupSnapshot,
  WorkspaceSummary,
  SessionViewSnapshot,
  SubmitInputResult,
  UserMessageSnapshot,
} from "@ez-assistant/protocol";

type InputReceipt = Readonly<{
  message: { type: "user" } & UserMessageSnapshot;
  run: RunSnapshot;
}>;

export type ConversationHistoryProjection = Readonly<{
  owner: ConversationOwner;
  generation: number;
  items: readonly ConversationItem[];
  previous_cursor: string | null;
  has_more: boolean;
  is_loading_previous: boolean;
  /** 单次前页读取的客户端身份；实时尾页合并保留，分页链重建即丢弃。 */
  previous_request?: Readonly<{ cursor: string; first_id: string | null }>;
  load_error: string | null;
}>;

export type ConversationLoadState = Readonly<{
  status: "idle" | "loading" | "error";
  error: string | null;
}>;
const IDLE_LOAD: ConversationLoadState = { status: "idle", error: null };

export const SIDEBAR_PAGE_SIZE = 10;

export function sessionGroupKey(workspace_id: string | null, filter: "active" | "archived"): string {
  return JSON.stringify([workspace_id, filter]);
}

export class RuntimeProjectionStore {
  /** 每次整份快照替换递增，分页响应不得跨越这一客户端修订。 */
  list_revision = 0;
  /** 整份权威列表替换；不同范围的本地写入不会互相撤销临时显示。 */
  sidebar_snapshot_revision = 0;
  readonly list_loads = observable.map<string, ConversationLoadState>(undefined, { deep: false });
  /** 页外项目的查询投影；不参与侧栏排序或分页。 */
  readonly workspace_details = observable.map<string, WorkspaceSummary>(undefined, { deep: false });
  readonly group_limits = new Map<string, number>();
  message_cards_changed = 0;
  card_sources_changed = 0;
  readonly conversation_loads = observable.map<string, ConversationLoadState>(undefined, { deep: false });
  application: ApplicationSnapshot | null = null;
  readonly session_views = observable.map<SessionId, SessionViewSnapshot>(undefined, {
    deep: false,
  });
  readonly conversation_histories = observable.map<SessionId, ConversationHistoryProjection>(
    undefined,
    { deep: false },
  );
  readonly input_receipts = observable.map<SessionId, readonly InputReceipt[]>(undefined, { deep: false });
  readonly child_task_views = observable.map<string, ChildTaskViewSnapshot>(undefined, {
    deep: false,
  });
  readonly child_conversation_histories = observable.map<string, ConversationHistoryProjection>(
    undefined,
    { deep: false },
  );
  #application_snapshot_sequence = 0;
  // 复用 Runtime 已有事件水位，只隔离同一 owner 的并发读取返回顺序，不建立新的协议 revision。
  #session_snapshot_sequences = new Map<SessionId, number>();
  #minimum_conversation_generations = new Map<SessionId, number>();
  #child_snapshot_sequences = new Map<string, number>();
  observed_sequence = 0;
  is_stale = true;

  constructor() {
    makeObservable(this, {
      message_cards_changed: observable,
      card_sources_changed: observable,
      invalidateMessageCards: action,
      application: observableRef,
      observed_sequence: observable,
      is_stale: observable,
      applyApplicationSnapshot: action,
      cacheWorkspaces: action,
      invalidateWorkspace: action,
      appendSessionGroup: action,
      appendWorkspacePage: action,
      setListLoadState: action,
      list_revision: observable,
      sidebar_snapshot_revision: observable,
      invalidateListPages: action,
      refreshApplicationSnapshot: action,
      applySessionSnapshot: action,
      noteConversationGeneration: action,
      applyInputReceipt: action,
      applyChildTaskSnapshot: action,
      beginLoadingPrevious: action,
      beginLoadingPreviousChild: action,
      applyPreviousConversationPage: action,
      applyPreviousChildConversationPage: action,
      failLoadingPrevious: action,
      failLoadingPreviousChild: action,
      acceptEvent: action,
      markStale: action,
      resetForInstance: action,
      setConversationLoadState: action,
    });
  }

  /** 仅表示查询失效；卡片与快照仍由 Runtime 查询恢复。 */
  invalidateMessageCards(source_changed: boolean): void {
    this.message_cards_changed += 1;
    if (source_changed) this.card_sources_changed += 1;
  }

  conversationLoadState(session_id: SessionId, child_task_id: ChildTaskId | null = null): ConversationLoadState {
    return this.conversation_loads.get(JSON.stringify([session_id, child_task_id])) ?? IDLE_LOAD;
  }

  sessionSnapshotSequence(session_id: SessionId): number {
    return this.#session_snapshot_sequences.get(session_id) ?? 0;
  }

  childSnapshotSequence(session_id: SessionId, child_task_id: ChildTaskId): number {
    return this.#child_snapshot_sequences.get(childKey(session_id, child_task_id)) ?? 0;
  }

  conversationGeneration(session_id: SessionId): number | undefined {
    return this.#minimum_conversation_generations.get(session_id);
  }

  setConversationLoadState(session_id: SessionId, child_task_id: ChildTaskId | null, state: ConversationLoadState): void {
    const key = JSON.stringify([session_id, child_task_id]);
    if (state.status === "idle") this.conversation_loads.delete(key);
    else this.conversation_loads.set(key, state);
  }

  applyApplicationSnapshot(snapshot: ObservedSnapshot<ApplicationSnapshot>): void {
    this.sidebar_snapshot_revision += 1;
    this.list_revision += 1;
    this.list_loads.clear();
    for (const page of snapshot.value.session_groups) {
      if (page.filter === "all") continue;
      const key = sessionGroupKey(page.group.type === "workspace" ? page.group.workspace_id : null, page.filter);
      this.group_limits.set(key, Math.max(SIDEBAR_PAGE_SIZE, page.next_offset ?? page.sessions.length));
    }
    this.application = snapshot.value;
    this.#application_snapshot_sequence = snapshot.observed_sequence;
    this.observed_sequence = snapshot.observed_sequence;
    this.is_stale = false;
  }

  refreshApplicationSnapshot(snapshot: ObservedSnapshot<ApplicationSnapshot>): void {
    if (snapshot.observed_sequence < this.#application_snapshot_sequence) {
      return;
    }
    this.sidebar_snapshot_revision += 1;
    this.list_revision += 1;
    this.list_loads.clear();
    for (const page of snapshot.value.session_groups) {
      if (page.filter === "all") continue;
      const key = sessionGroupKey(page.group.type === "workspace" ? page.group.workspace_id : null, page.filter);
      this.group_limits.set(key, Math.max(SIDEBAR_PAGE_SIZE, page.next_offset ?? page.sessions.length));
    }
    this.application = snapshot.value;
    this.#application_snapshot_sequence = snapshot.observed_sequence;
    this.is_stale = false;
  }

  invalidateWorkspace(workspace_id: string): void {
    this.workspace_details.delete(workspace_id);
  }

  cacheWorkspaces(workspaces: readonly WorkspaceSummary[]): void {
    for (const workspace of workspaces) this.workspace_details.set(workspace.workspace_id, workspace);
  }

  workspaceSummary(workspace_id: string | null | undefined): WorkspaceSummary | undefined {
    if (!workspace_id) return undefined;
    return this.application?.workspaces.find(workspace => workspace.workspace_id === workspace_id)
      ?? this.workspace_details.get(workspace_id);
  }

  sessionGroup(workspace_id: string | null, filter: "active" | "archived"): SessionGroupSnapshot | undefined {
    return this.application?.session_groups.find(page => page.filter === filter
      && (page.group.type === "workspace" ? page.group.workspace_id : null) === workspace_id);
  }

  /** 详情优先，侧栏页与置顶只作为尚未打开会话的摘要来源。 */
  sessionSummary(session_id: SessionId | null): SessionSummary | undefined {
    if (!session_id) return undefined;
    return this.session_views.get(session_id)?.session
      ?? this.application?.session_groups.flatMap(page => page.sessions).find(session => session.session_id === session_id)
      ?? this.application?.pinned_sessions.find(session => session.session_id === session_id);
  }

  loadedSessions(filter?: "active" | "archived"): SessionSummary[] {
    const pages = this.application?.session_groups.filter(page => !filter || page.filter === filter) ?? [];
    return [...new Map(pages.flatMap(page => page.sessions).map(session => [session.session_id, session])).values()];
  }

  invalidateListPages(): void {
    this.list_revision += 1;
    this.list_loads.clear();
  }

  listLoadState(key: string): ConversationLoadState {
    return this.list_loads.get(key) ?? IDLE_LOAD;
  }

  setListLoadState(key: string, state: ConversationLoadState): void {
    if (state.status === "idle") this.list_loads.delete(key);
    else this.list_loads.set(key, state);
  }

  appendSessionGroup(page: SessionGroupSnapshot, offset: number, revision: number): void {
    if (!this.application || revision !== this.list_revision || page.filter === "all") return;
    const workspace_id = page.group.type === "workspace" ? page.group.workspace_id : null;
    const current = this.sessionGroup(workspace_id, page.filter);
    if (offset > 0 && current?.next_offset !== offset) return;
    const sessions = new Map((offset ? current?.sessions ?? [] : []).map(session => [session.session_id, session]));
    for (const session of page.sessions) sessions.set(session.session_id, session);
    const merged = { ...page, sessions: [...sessions.values()] };
    this.group_limits.set(sessionGroupKey(workspace_id, page.filter), Math.max(SIDEBAR_PAGE_SIZE, page.next_offset ?? merged.sessions.length));
    this.application = { ...this.application, session_groups: current ? this.application.session_groups.map(item => item === current ? merged : item) : [...this.application.session_groups, merged] };
  }

  appendWorkspacePage(workspaces: readonly WorkspaceSummary[], offset: number, next_offset: number | null, revision: number): void {
    if (!this.application || revision !== this.list_revision || this.application.workspaces_next_offset !== offset) return;
    const merged = new Map(this.application.workspaces.map(workspace => [workspace.workspace_id, workspace]));
    for (const workspace of workspaces) merged.set(workspace.workspace_id, workspace);
    this.application = { ...this.application, workspaces: [...merged.values()], workspaces_next_offset: next_offset };
  }

  applySessionSnapshot(snapshot: ObservedSnapshot<SessionViewSnapshot>): boolean {
    const session_id = snapshot.value.session.session_id;
    if (!isMainOwner(snapshot.value.conversation.owner, session_id)
      || snapshot.value.conversation.generation < (this.#minimum_conversation_generations.get(session_id) ?? 0)) {
      return false;
    }
    const previous_sequence = this.#session_snapshot_sequences.get(session_id);
    if (previous_sequence !== undefined && snapshot.observed_sequence < previous_sequence) {
      return false;
    }
    this.#session_snapshot_sequences.set(session_id, snapshot.observed_sequence);
    this.noteConversationGeneration(session_id, snapshot.value.conversation.generation);
    this.session_views.set(session_id, snapshot.value);
    this.#applyLatestConversationPage(session_id, snapshot.value.conversation);
    const receipts = this.input_receipts.get(session_id);
    if (receipts) {
      const ids = new Set(snapshot.value.conversation.items.map(conversationItemId));
      const remaining = receipts.filter(receipt => !ids.has(receipt.message.message_id));
      if (remaining.length) this.input_receipts.set(session_id, remaining);
      else this.input_receipts.delete(session_id);
    }
    return true;
  }

  /** 回执中的用户消息已经持久化。独立于分页尾部展示，下一份包含该消息的快照恢复完整排序。 */
  applyInputReceipt(session_id: SessionId, result: SubmitInputResult): boolean {
    const { user_message, run, generation } = result;
    const history = this.conversation_histories.get(session_id);
    if (!user_message || !run || generation === undefined || generation !== history?.generation
      || run.session_id !== session_id || run.input_id !== result.input_id || user_message.input_id !== result.input_id) return false;
    const run_created_at = run.created_at_ms;
    // 断线期间若其他客户端已经启动后续轮次，旧回执只能确认原发送，不能成为新尾页。
    if (run_created_at !== null && this.session_views.get(session_id)?.runs?.some(item =>
      item.created_at_ms !== null && item.created_at_ms > run_created_at)) return false;
    const receipts = this.input_receipts.get(session_id) ?? [];
    if (!history.items.some(item => item.message_id === user_message.message_id)
      && !receipts.some(item => item.message.message_id === user_message.message_id)) {
      this.input_receipts.set(session_id, [...receipts, { message: { ...user_message, type: "user" }, run }]);
    }
    return true;
  }

  /** 收到可靠换代后立即丢弃旧正文和子任务缓存，并拒收较早的会话快照。 */
  noteConversationGeneration(session_id: SessionId, generation: number): void {
    const previous = this.#minimum_conversation_generations.get(session_id)
      ?? this.conversation_histories.get(session_id)?.generation;
    if (previous !== undefined && generation <= previous) return;
    this.#minimum_conversation_generations.set(session_id, generation);
    if (previous === undefined) return;
    this.conversation_histories.delete(session_id);
    this.input_receipts.delete(session_id);
    for (const [key, history] of this.child_conversation_histories) {
      if (history.owner.session_id !== session_id) continue;
      this.child_conversation_histories.delete(key);
      this.child_task_views.delete(key);
    }
    this.invalidateMessageCards(false);
  }

  applyChildTaskSnapshot(snapshot: ObservedSnapshot<ChildTaskViewSnapshot>): boolean {
    const session_id = snapshot.value.task.task.session_id;
    const child_task_id = snapshot.value.task.task.child_task_id;
    if (!isChildOwner(snapshot.value.conversation.owner, session_id, child_task_id)) {
      return false;
    }
    const previous_sequence = this.#child_snapshot_sequences.get(childKey(session_id, child_task_id));
    if (previous_sequence !== undefined && snapshot.observed_sequence < previous_sequence) {
      return false;
    }
    this.#child_snapshot_sequences.set(childKey(session_id, child_task_id), snapshot.observed_sequence);
    this.child_task_views.set(childKey(session_id, child_task_id), snapshot.value);
    this.#applyLatestChildConversationPage(session_id, child_task_id, snapshot.value.conversation);
    return true;
  }

  beginLoadingPrevious(session_id: SessionId): boolean {
    const history = this.conversation_histories.get(session_id);
    if (!history || history.is_loading_previous || !history.has_more || !history.previous_cursor) {
      return false;
    }
    this.conversation_histories.set(session_id, {
      ...history,
      is_loading_previous: true,
      previous_request: { cursor: history.previous_cursor, first_id: history.items[0]?.message_id ?? null },
      load_error: null,
    });
    return true;
  }

  beginLoadingPreviousChild(session_id: SessionId, child_task_id: ChildTaskId): boolean {
    const history = this.child_conversation_histories.get(childKey(session_id, child_task_id));
    if (!history || history.is_loading_previous || !history.has_more || !history.previous_cursor) {
      return false;
    }
    this.child_conversation_histories.set(childKey(session_id, child_task_id), {
      ...history,
      is_loading_previous: true,
      previous_request: { cursor: history.previous_cursor, first_id: history.items[0]?.message_id ?? null },
      load_error: null,
    });
    return true;
  }

  applyPreviousConversationPage(
    session_id: SessionId,
    snapshot: ObservedSnapshot<ConversationPage>,
  ): boolean {
    const current = this.conversation_histories.get(session_id);
    const page = snapshot.value;
    if (!current || current.generation !== page.generation || !isMainOwner(page.owner, session_id)) {
      return false;
    }
    this.conversation_histories.set(session_id, {
      owner: page.owner,
      generation: page.generation,
      items: mergeConversationItems(page.items, current.items),
      previous_cursor: page.previous_cursor,
      has_more: page.has_more,
      is_loading_previous: false,
      load_error: null,
    });
    return true;
  }

  applyPreviousChildConversationPage(
    session_id: SessionId,
    child_task_id: ChildTaskId,
    snapshot: ObservedSnapshot<ConversationPage>,
  ): boolean {
    const current = this.child_conversation_histories.get(childKey(session_id, child_task_id));
    const page = snapshot.value;
    if (
      !current
      || current.generation !== page.generation
      || page.owner.type !== "child_task"
      || page.owner.child_task_id !== child_task_id
      || page.owner.session_id !== session_id
      || page.owner.session_id !== current.owner.session_id
    ) {
      return false;
    }
    this.child_conversation_histories.set(childKey(session_id, child_task_id), {
      owner: page.owner,
      generation: page.generation,
      items: mergeConversationItems(page.items, current.items),
      previous_cursor: page.previous_cursor,
      has_more: page.has_more,
      is_loading_previous: false,
      load_error: null,
    });
    return true;
  }

  failLoadingPrevious(session_id: SessionId, message: string | null): void {
    const current = this.conversation_histories.get(session_id);
    if (!current) {
      return;
    }
    this.conversation_histories.set(session_id, {
      ...current,
      previous_request: undefined,
      is_loading_previous: false,
      load_error: message,
    });
  }

  failLoadingPreviousChild(session_id: SessionId, child_task_id: ChildTaskId, message: string | null): void {
    const current = this.child_conversation_histories.get(childKey(session_id, child_task_id));
    if (!current) {
      return;
    }
    this.child_conversation_histories.set(childKey(session_id, child_task_id), {
      ...current,
      previous_request: undefined,
      is_loading_previous: false,
      load_error: message,
    });
  }

  acceptEvent(envelope: RuntimeEventEnvelope): "ignored" | "accepted" | "gap" {
    if (envelope.sequence <= this.observed_sequence) {
      return "ignored";
    }
    if (envelope.sequence !== this.observed_sequence + 1) {
      this.is_stale = true;
      return "gap";
    }
    this.observed_sequence = envelope.sequence;
    return "accepted";
  }

  markStale(): void {
    this.is_stale = true;
  }

  resetForInstance(): void {
    this.sidebar_snapshot_revision += 1;
    this.list_revision += 1;
    this.list_loads.clear();
    this.group_limits.clear();
    this.workspace_details.clear();
    this.conversation_loads.clear();
    this.application = null;
    this.#application_snapshot_sequence = 0;
    this.#session_snapshot_sequences.clear();
    this.#minimum_conversation_generations.clear();
    this.#child_snapshot_sequences.clear();
    this.session_views.clear();
    this.conversation_histories.clear();
    this.input_receipts.clear();
    this.child_task_views.clear();
    this.child_conversation_histories.clear();
    this.observed_sequence = 0;
    this.is_stale = true;
  }

  #applyLatestConversationPage(session_id: SessionId, page: ConversationPage): void {
    const current = this.conversation_histories.get(session_id);
    if (!current
      || current.generation !== page.generation
      || !isMainOwner(current.owner, session_id)) {
      this.conversation_histories.set(session_id, historyFromPage(page));
      return;
    }
    const first_latest_id = page.items[0] ? conversationItemId(page.items[0]) : null;
    const retained_previous = first_latest_id
      ? current.items.some((item) => conversationItemId(item) === first_latest_id)
      : current.items.length === 0;
    // 最新页与缓存没有连续边界时，服务端页是唯一权威事实；做并集会把失配 owner 或过期页永久留在列表头部。
    if (!retained_previous) {
      this.conversation_histories.set(session_id, historyFromPage(page));
      return;
    }
    this.conversation_histories.set(session_id, {
      owner: page.owner,
      generation: page.generation,
      items: mergeConversationItems(current.items, page.items),
      previous_cursor: retained_previous ? current.previous_cursor : page.previous_cursor,
      has_more: retained_previous ? current.has_more : page.has_more,
      is_loading_previous: current.is_loading_previous,
      previous_request: current.previous_request,
      load_error: current.load_error,
    });
  }

  #applyLatestChildConversationPage(session_id: SessionId, child_task_id: ChildTaskId, page: ConversationPage): void {
    const current = this.child_conversation_histories.get(childKey(session_id, child_task_id));
    if (!current
      || current.generation !== page.generation
      || current.owner.type !== "child_task"
      || current.owner.session_id !== page.owner.session_id
      || current.owner.child_task_id !== child_task_id) {
      this.child_conversation_histories.set(childKey(session_id, child_task_id), historyFromPage(page));
      return;
    }
    const first_latest_id = page.items[0] ? conversationItemId(page.items[0]) : null;
    const retained_previous = first_latest_id
      ? current.items.some((item) => conversationItemId(item) === first_latest_id)
      : current.items.length === 0;
    if (!retained_previous) {
      this.child_conversation_histories.set(childKey(session_id, child_task_id), historyFromPage(page));
      return;
    }
    this.child_conversation_histories.set(childKey(session_id, child_task_id), {
      owner: page.owner,
      generation: page.generation,
      items: mergeConversationItems(current.items, page.items),
      previous_cursor: retained_previous ? current.previous_cursor : page.previous_cursor,
      has_more: retained_previous ? current.has_more : page.has_more,
      is_loading_previous: current.is_loading_previous,
      previous_request: current.previous_request,
      load_error: current.load_error,
    });
  }
}

function historyFromPage(page: ConversationPage): ConversationHistoryProjection {
  return {
    owner: page.owner,
    generation: page.generation,
    items: page.items,
    previous_cursor: page.previous_cursor,
    has_more: page.has_more,
    is_loading_previous: false,
    load_error: null,
  };
}

function isMainOwner(owner: ConversationOwner, session_id: SessionId): boolean {
  return owner.type === "main_session" && owner.session_id === session_id;
}

function isChildOwner(
  owner: ConversationOwner,
  session_id: SessionId,
  child_task_id: ChildTaskId,
): boolean {
  return owner.type === "child_task"
    && owner.session_id === session_id
    && owner.child_task_id === child_task_id;
}

export function conversationItemId(item: ConversationItem): string {
  return item.message_id;
}

function mergeConversationItems(
  first: readonly ConversationItem[],
  second: readonly ConversationItem[],
): readonly ConversationItem[] {
  const merged = new Map<string, ConversationItem>();
  for (const item of first) {
    merged.set(conversationItemId(item), item);
  }
  for (const item of second) {
    merged.set(conversationItemId(item), item);
  }
  return [...merged.values()];
}
