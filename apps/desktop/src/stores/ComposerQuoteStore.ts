import { action, makeObservable, observable } from "mobx";
import type { QuotedTextSnapshot, SessionId } from "../generated/assistant-protocol";

/** WebView 生命周期内的 Session Composer 引用草稿；发送成功前不进入 Runtime 权威状态。 */
export class ComposerQuoteStore {
  readonly by_session = observable.map<SessionId, readonly QuotedTextSnapshot[]>(undefined, { deep: false });

  constructor() {
    makeObservable(this, {
      by_session: observable,
      add: action,
      remove: action,
      clear: action,
    });
  }

  get(session_id: SessionId | null): readonly QuotedTextSnapshot[] {
    return session_id ? this.by_session.get(session_id) ?? [] : [];
  }

  add(session_id: SessionId, quote: QuotedTextSnapshot): void {
    const current = this.get(session_id);
    if (current.length >= 16) return;
    this.by_session.set(session_id, [...current, quote]);
  }

  remove(session_id: SessionId, quote_id: string): void {
    const remaining = this.get(session_id).filter((quote) => quote.quote_id !== quote_id);
    if (remaining.length > 0) this.by_session.set(session_id, remaining);
    else this.by_session.delete(session_id);
  }

  clear(session_id: SessionId): void {
    this.by_session.delete(session_id);
  }
}
