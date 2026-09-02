import { afterEach, describe, expect, it, vi } from "vitest";
import type { QuotedTextSnapshot } from "../../src/generated/assistant-protocol";
import { ComposerQuoteStore } from "../../src/stores/ComposerQuoteStore";
import { TransientFocusStore } from "../../src/stores/TransientFocusStore";

afterEach(() => {
  vi.useRealTimers();
});

describe("ComposerQuoteStore", () => {
  it("keeps quotes isolated per session and enforces the Composer limit", () => {
    const store = new ComposerQuoteStore();
    for (let index = 0; index < 17; index += 1) {
      store.add("session-1", quote(`quote-${index}`));
    }
    store.add("session-2", quote("other"));

    expect(store.get("session-1")).toHaveLength(16);
    expect(store.get("session-2").map((item) => item.quote_id)).toEqual(["other"]);

    store.remove("session-1", "quote-0");
    expect(store.get("session-1")).toHaveLength(15);
    store.clear("session-1");
    expect(store.get("session-1")).toEqual([]);
    expect(store.get("session-2")).toHaveLength(1);
  });
});

describe("TransientFocusStore", () => {
  it("replaces an earlier target and always expires the current focus", () => {
    vi.useFakeTimers();
    const store = new TransientFocusStore();
    store.focus(quote("message-1"));
    const first_nonce = store.target?.nonce;
    store.focus(quote("message-2"));

    expect(store.target?.message_id).toBe("message-2");
    expect(store.target?.nonce).toBeGreaterThan(first_nonce ?? 0);
    vi.advanceTimersByTime(1600);
    expect(store.target).toBeNull();
  });
});

function quote(quote_id: string): QuotedTextSnapshot {
  return {
    quote_id,
    exact: quote_id,
    prefix: "before",
    suffix: "after",
    source_owner: { type: "main_session", session_id: "session-1" },
    source_generation: 1,
    source_message_id: quote_id,
    text_start_utf16: 0,
    text_end_utf16: quote_id.length,
    source_role: "assistant",
    source_label: "来源会话",
    source_available: true,
  };
}
