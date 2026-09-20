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
  SessionId,
  SessionSummary,
  SessionViewSnapshot,
} from "@ez-assistant/protocol";

export type ConversationHistoryProjection = Readonly<{
  owner: ConversationOwner;
  generation: number;
  items: readonly ConversationItem[];
  previous_cursor: string | null;
  has_more: boolean;
  is_loading_previous: boolean;
  load_error: string | null;
}>;

export class RuntimeProjectionStore {
  application: ApplicationSnapshot | null = null;
  readonly session_views = observable.map<SessionId, SessionViewSnapshot>(undefined, {
    deep: false,
  });
  readonly conversation_histories = observable.map<SessionId, ConversationHistoryProjection>(
    undefined,
    { deep: false },
  );
  readonly child_task_views = observable.map<ChildTaskId, ChildTaskViewSnapshot>(undefined, {
    deep: false,
  });
  readonly child_conversation_histories = observable.map<ChildTaskId, ConversationHistoryProjection>(
    undefined,
    { deep: false },
  );
  #application_snapshot_sequence = 0;
  // 复用 Runtime 已有事件水位，只隔离同一 owner 的并发读取返回顺序，不建立新的协议 revision。
  #session_snapshot_sequences = new Map<SessionId, number>();
  #child_snapshot_sequences = new Map<ChildTaskId, number>();
  observed_sequence = 0;
  is_stale = true;

  constructor() {
    makeObservable(this, {
      application: observableRef,
      observed_sequence: observable,
      is_stale: observable,
      applyApplicationSnapshot: action,
      appendSessionPage: action,
      refreshApplicationSnapshot: action,
      applySessionSnapshot: action,
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
    });
  }

  applyApplicationSnapshot(snapshot: ObservedSnapshot<ApplicationSnapshot>): void {
    this.application = snapshot.value;
    this.#application_snapshot_sequence = snapshot.observed_sequence;
    this.observed_sequence = snapshot.observed_sequence;
    this.is_stale = false;
  }

  refreshApplicationSnapshot(snapshot: ObservedSnapshot<ApplicationSnapshot>): void {
    if (snapshot.observed_sequence < this.#application_snapshot_sequence) {
      return;
    }
    this.application = snapshot.value;
    this.#application_snapshot_sequence = snapshot.observed_sequence;
    this.is_stale = false;
  }

  appendSessionPage(filter: "active" | "archived", sessions: readonly SessionSummary[], next_offset: number | null): void {
    const application = this.application;
    if (!application) return;
    const key = filter === "active" ? "active_sessions" : "archived_sessions";
    const offset_key = filter === "active" ? "active_sessions_next_offset" : "archived_sessions_next_offset";
    const merged = new Map(application[key].map((session) => [session.session_id, session]));
    for (const session of sessions) merged.set(session.session_id, session);
    this.application = { ...application, [key]: [...merged.values()], [offset_key]: next_offset };
  }

  applySessionSnapshot(snapshot: ObservedSnapshot<SessionViewSnapshot>): boolean {
    const session_id = snapshot.value.session.session_id;
    if (!isMainOwner(snapshot.value.conversation.owner, session_id)) {
      return false;
    }
    const previous_sequence = this.#session_snapshot_sequences.get(session_id);
    if (previous_sequence !== undefined && snapshot.observed_sequence < previous_sequence) {
      return false;
    }
    this.#session_snapshot_sequences.set(session_id, snapshot.observed_sequence);
    this.session_views.set(session_id, snapshot.value);
    this.#applyLatestConversationPage(session_id, snapshot.value.conversation);
    return true;
  }

  applyChildTaskSnapshot(snapshot: ObservedSnapshot<ChildTaskViewSnapshot>): boolean {
    const session_id = snapshot.value.task.task.session_id;
    const child_task_id = snapshot.value.task.task.child_task_id;
    if (!isChildOwner(snapshot.value.conversation.owner, session_id, child_task_id)) {
      return false;
    }
    const previous_sequence = this.#child_snapshot_sequences.get(child_task_id);
    if (previous_sequence !== undefined && snapshot.observed_sequence < previous_sequence) {
      return false;
    }
    this.#child_snapshot_sequences.set(child_task_id, snapshot.observed_sequence);
    this.child_task_views.set(child_task_id, snapshot.value);
    this.#applyLatestChildConversationPage(child_task_id, snapshot.value.conversation);
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
      load_error: null,
    });
    return true;
  }

  beginLoadingPreviousChild(child_task_id: ChildTaskId): boolean {
    const history = this.child_conversation_histories.get(child_task_id);
    if (!history || history.is_loading_previous || !history.has_more || !history.previous_cursor) {
      return false;
    }
    this.child_conversation_histories.set(child_task_id, {
      ...history,
      is_loading_previous: true,
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
    child_task_id: ChildTaskId,
    snapshot: ObservedSnapshot<ConversationPage>,
  ): boolean {
    const current = this.child_conversation_histories.get(child_task_id);
    const page = snapshot.value;
    if (
      !current
      || current.generation !== page.generation
      || page.owner.type !== "child_task"
      || page.owner.child_task_id !== child_task_id
    ) {
      return false;
    }
    this.child_conversation_histories.set(child_task_id, {
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

  failLoadingPrevious(session_id: SessionId, message: string): void {
    const current = this.conversation_histories.get(session_id);
    if (!current) {
      return;
    }
    this.conversation_histories.set(session_id, {
      ...current,
      is_loading_previous: false,
      load_error: message,
    });
  }

  failLoadingPreviousChild(child_task_id: ChildTaskId, message: string): void {
    const current = this.child_conversation_histories.get(child_task_id);
    if (!current) {
      return;
    }
    this.child_conversation_histories.set(child_task_id, {
      ...current,
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
    this.application = null;
    this.#application_snapshot_sequence = 0;
    this.#session_snapshot_sequences.clear();
    this.#child_snapshot_sequences.clear();
    this.session_views.clear();
    this.conversation_histories.clear();
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
      is_loading_previous: false,
      load_error: null,
    });
  }

  #applyLatestChildConversationPage(child_task_id: ChildTaskId, page: ConversationPage): void {
    const current = this.child_conversation_histories.get(child_task_id);
    if (!current
      || current.generation !== page.generation
      || current.owner.type !== "child_task"
      || current.owner.session_id !== page.owner.session_id
      || current.owner.child_task_id !== child_task_id) {
      this.child_conversation_histories.set(child_task_id, historyFromPage(page));
      return;
    }
    const first_latest_id = page.items[0] ? conversationItemId(page.items[0]) : null;
    const retained_previous = first_latest_id
      ? current.items.some((item) => conversationItemId(item) === first_latest_id)
      : current.items.length === 0;
    if (!retained_previous) {
      this.child_conversation_histories.set(child_task_id, historyFromPage(page));
      return;
    }
    this.child_conversation_histories.set(child_task_id, {
      owner: page.owner,
      generation: page.generation,
      items: mergeConversationItems(current.items, page.items),
      previous_cursor: retained_previous ? current.previous_cursor : page.previous_cursor,
      has_more: retained_previous ? current.has_more : page.has_more,
      is_loading_previous: false,
      load_error: null,
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
