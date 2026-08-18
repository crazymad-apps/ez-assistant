import { action, makeObservable, observable, observableRef } from "mobx";
import type {
  ConversationHistoryHit,
  ConversationHistoryScope,
  GetConversationRecallWindowResult,
} from "../generated/assistant-protocol";

export type ConversationSearchStatus = "idle" | "loading" | "ready" | "error";

/** Desktop 历史搜索的临时 UI 状态；搜索结果不进入 Runtime 权威会话状态。 */
export class ConversationSearchStore {
  query = "";
  scope: ConversationHistoryScope = "session";
  status: ConversationSearchStatus = "idle";
  readonly items = observable.array<ConversationHistoryHit>([], { deep: false });
  next_offset: number | null = null;
  partial = false;
  error: string | null = null;
  selected_hit: ConversationHistoryHit | null = null;
  recall_window: GetConversationRecallWindowResult | null = null;
  recall_loading = false;
  recall_error: string | null = null;

  constructor() {
    makeObservable(this, {
      query: observable,
      scope: observable,
      status: observable,
      next_offset: observable,
      partial: observable,
      error: observable,
      selected_hit: observableRef,
      recall_window: observableRef,
      recall_loading: observable,
      recall_error: observable,
      setQuery: action,
      setScope: action,
      beginSearch: action,
      applySearch: action,
      failSearch: action,
      selectHit: action,
      beginRecall: action,
      applyRecall: action,
      failRecall: action,
      closeRecall: action,
      reset: action,
    });
  }

  setQuery(query: string): void {
    this.query = query;
    if (!query.trim()) {
      this.status = "idle";
      this.items.clear();
      this.next_offset = null;
      this.partial = false;
      this.error = null;
    }
  }

  setScope(scope: ConversationHistoryScope): void {
    if (this.scope === scope) {
      return;
    }
    this.scope = scope;
    // 切换检索范围后，旧结果已经不再对应当前条件，立即清除以避免短暂误导。
    this.status = "idle";
    this.items.clear();
    this.next_offset = null;
    this.partial = false;
    this.error = null;
  }

  beginSearch(reset: boolean): void {
    this.status = "loading";
    this.error = null;
    if (reset) {
      this.items.clear();
      this.next_offset = null;
      this.partial = false;
    }
  }

  applySearch(
    items: readonly ConversationHistoryHit[],
    next_offset: number | null,
    partial: boolean,
    reset: boolean,
  ): void {
    if (reset) {
      this.items.replace([...items]);
    } else {
      this.items.push(...items);
    }
    this.next_offset = next_offset;
    this.partial = partial;
    this.status = "ready";
    this.error = null;
  }

  failSearch(message: string): void {
    this.status = "error";
    this.error = message;
  }

  selectHit(hit: ConversationHistoryHit): void {
    this.selected_hit = hit;
    this.recall_window = null;
    this.recall_error = null;
  }

  beginRecall(): void {
    this.recall_loading = true;
    this.recall_error = null;
  }

  applyRecall(result: GetConversationRecallWindowResult): void {
    this.recall_window = result;
    this.recall_loading = false;
    this.recall_error = null;
  }

  failRecall(message: string): void {
    this.recall_loading = false;
    this.recall_error = message;
  }

  closeRecall(): void {
    this.selected_hit = null;
    this.recall_window = null;
    this.recall_loading = false;
    this.recall_error = null;
  }

  reset(): void {
    this.query = "";
    this.scope = "session";
    this.status = "idle";
    this.items.clear();
    this.next_offset = null;
    this.partial = false;
    this.error = null;
    this.closeRecall();
  }
}
