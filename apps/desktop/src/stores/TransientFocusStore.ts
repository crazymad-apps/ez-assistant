import { action, makeObservable, observable } from "mobx";
import type { ConversationOwner, MessageId, QuotedTextSnapshot } from "../generated/assistant-protocol";

export type TransientFocusTarget = Readonly<{
  owner: ConversationOwner;
  generation: number;
  message_id: MessageId;
  exact: string;
  text_start_utf16: number;
  text_end_utf16: number;
  nonce: number;
}>;

/** 一次性来源定位状态；不修改消息正文，也不进入持久化投影。 */
export class TransientFocusStore {
  target: TransientFocusTarget | null = null;
  #timer: number | null = null;
  #nonce = 0;

  constructor() {
    makeObservable(this, { target: observable, focus: action, clear: action });
  }

  focus(quote: QuotedTextSnapshot): void {
    this.clear();
    this.target = {
      owner: quote.source_owner,
      generation: quote.source_generation,
      message_id: quote.source_message_id,
      exact: quote.exact,
      text_start_utf16: quote.text_start_utf16,
      text_end_utf16: quote.text_end_utf16,
      nonce: ++this.#nonce,
    };
    this.#timer = window.setTimeout(() => this.clear(), 1600);
  }

  clear(): void {
    if (this.#timer !== null) window.clearTimeout(this.#timer);
    this.#timer = null;
    this.target = null;
  }
}
