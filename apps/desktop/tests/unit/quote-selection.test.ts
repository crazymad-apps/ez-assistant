import { describe, expect, it } from "vitest";
import {
  createQuotedTextSnapshot,
  quoteSourceRange,
} from "../../src/features/conversation/quoteSelection";

describe("conversation quote selection", () => {
  it("freezes visible text across inline code, links, and emphasis", () => {
    const root = document.createElement("div");
    root.dataset.quoteRoot = "true";
    root.dataset.quoteContent = "true";
    root.innerHTML = "<p>紧凑 schema: <code>computer_use</code> 用<a href='https://example.com'>单个</a> <strong>function</strong> 完成</p>";
    document.body.append(root);
    const paragraph = root.querySelector("p")!;
    const start = paragraph.firstChild as Text;
    const end = paragraph.lastChild as Text;
    const range = document.createRange();
    range.setStart(start, 0);
    range.setEnd(end, 3);

    const quote = createQuotedTextSnapshot(root, range, {
      quote_id: "quote-inline",
      source_owner: { type: "main_session", session_id: "session-1" },
      source_generation: 2,
      source_message_id: "message-1",
      source_role: "assistant",
      source_label: "来源会话",
    });

    expect(quote?.exact).toBe("紧凑 schema: computer_use 用单个 function 完成");
    expect(quote?.text_end_utf16).toBe(quote?.exact.length);
    expect(quote && quoteSourceRange(root, quote)?.toString()).toBe(quote?.exact);
  });

  it("rejects a range crossing an interactive element and refuses stale highlight text", () => {
    const root = document.createElement("div");
    root.dataset.quoteRoot = "true";
    root.dataset.quoteContent = "true";
    root.innerHTML = "<span>before</span><button>tool</button><span>after</span>";
    document.body.append(root);
    const range = document.createRange();
    range.setStart(root.querySelector("span")!.firstChild!, 0);
    range.setEnd(root.querySelectorAll("span")[1]!.firstChild!, 5);

    expect(createQuotedTextSnapshot(root, range, {
      quote_id: "quote-cross-control",
      source_owner: { type: "main_session", session_id: "session-1" },
      source_generation: 1,
      source_message_id: "message-1",
      source_role: "assistant",
      source_label: "来源会话",
    })).toBeNull();

    const safe = document.createRange();
    safe.selectNodeContents(root.querySelector("span")!);
    const quote = createQuotedTextSnapshot(root, safe, {
      quote_id: "quote-safe",
      source_owner: { type: "main_session", session_id: "session-1" },
      source_generation: 1,
      source_message_id: "message-1",
      source_role: "assistant",
      source_label: "来源会话",
    });
    expect(quote).not.toBeNull();
    expect(quote && quoteSourceRange(root, { ...quote, exact: "changed" })).toBeNull();
  });

  it("keeps list boundaries and UTF-16 emoji ranges", () => {
    const root = document.createElement("div");
    root.dataset.quoteRoot = "true";
    root.dataset.quoteContent = "true";
    root.innerHTML = "<ul><li>第一项😀</li><li><code>第二项</code></li></ul>";
    document.body.append(root);
    const items = root.querySelectorAll("li");
    const range = document.createRange();
    range.setStart(items[0]!.firstChild!, 0);
    range.setEnd(items[1]!.querySelector("code")!.firstChild!, 3);
    const quote = createQuotedTextSnapshot(root, range, {
      quote_id: "quote-list",
      source_owner: { type: "main_session", session_id: "session-1" },
      source_generation: 1,
      source_message_id: "message-list",
      source_role: "assistant",
      source_label: "来源会话",
    });
    expect(quote?.exact).toBe("第一项😀\n第二项");
    expect(quote?.text_end_utf16).toBe(quote?.exact.length);
  });

  it("limits frozen context to 128 Unicode code points on each side", () => {
    const root = document.createElement("div");
    root.dataset.quoteRoot = "true";
    root.dataset.quoteContent = "true";
    root.textContent = `${"前".repeat(140)}目标${"后".repeat(140)}`;
    document.body.append(root);
    const text = root.firstChild!;
    const range = document.createRange();
    range.setStart(text, 140);
    range.setEnd(text, 142);

    const quote = createQuotedTextSnapshot(root, range, {
      quote_id: "quote-context-limit",
      source_owner: { type: "main_session", session_id: "session-1" },
      source_generation: 1,
      source_message_id: "message-context-limit",
      source_role: "assistant",
      source_label: "来源会话",
    });

    expect(Array.from(quote?.prefix ?? "")).toHaveLength(128);
    expect(Array.from(quote?.suffix ?? "")).toHaveLength(128);
  });
});
