import type {
  ConversationOwner,
  MessageId,
  QuotedTextSnapshot,
  QuotedTextSourceRoleSnapshot,
} from "../../generated/assistant-protocol";

const MAX_EXACT_BYTES = 8 * 1024;
const MAX_CONTEXT_CODE_POINTS = 128;
const QUOTE_CONTENT_SELECTOR = "[data-quote-content='true']";
const EXCLUDED_SELECTOR = [
  "button",
  "input",
  "textarea",
  "select",
  "option",
  "[contenteditable='true']",
  "[aria-hidden='true']",
  "[data-quote-exclude='true']",
].join(",");
const BLOCK_SELECTOR = "p,li,pre,blockquote,h1,h2,h3,h4,h5,h6";

type ProjectionEntry = Readonly<{
  node: Text;
  start: number;
  end: number;
}>;

type QuoteTextProjection = Readonly<{
  text: string;
  entries: readonly ProjectionEntry[];
}>;

export type QuoteSelectionMetadata = Readonly<{
  quote_id: string;
  source_owner: ConversationOwner;
  source_generation: number;
  source_message_id: MessageId;
  source_role: QuotedTextSourceRoleSnapshot;
  source_label: string;
  source_created_at_ms?: number | null;
}>;

/** 将同一消息根内的 DOM Range 冻结为可提交引用；失败表示选区不属于可引用正文。 */
export function createQuotedTextSnapshot(
  root: HTMLElement,
  range: Range,
  metadata: QuoteSelectionMetadata,
): QuotedTextSnapshot | null {
  if (!containsBoundary(root, range.startContainer) || !containsBoundary(root, range.endContainer)) {
    return null;
  }
  if (rangeIntersectsExcludedContent(root, range)) return null;

  const projection = buildQuoteTextProjection(root);
  const text_start_utf16 = projectionOffset(range.startContainer, range.startOffset, projection);
  const text_end_utf16 = projectionOffset(range.endContainer, range.endOffset, projection);
  if (text_start_utf16 === null || text_end_utf16 === null || text_start_utf16 >= text_end_utf16) {
    return null;
  }
  const exact = projection.text.slice(text_start_utf16, text_end_utf16);
  if (!exact.trim() || new TextEncoder().encode(exact).length > MAX_EXACT_BYTES) return null;

  return {
    quote_id: metadata.quote_id,
    exact,
    prefix: takeLastCodePoints(projection.text.slice(0, text_start_utf16), MAX_CONTEXT_CODE_POINTS),
    suffix: takeFirstCodePoints(projection.text.slice(text_end_utf16), MAX_CONTEXT_CODE_POINTS),
    source_owner: metadata.source_owner,
    source_generation: metadata.source_generation,
    source_message_id: metadata.source_message_id,
    text_start_utf16,
    text_end_utf16,
    source_role: metadata.source_role,
    source_label: metadata.source_label,
    ...(metadata.source_created_at_ms == null
      ? {}
      : { source_created_at_ms: metadata.source_created_at_ms }),
    source_available: true,
  };
}

/** 用持久化 UTF-16 locator 恢复非侵入式高亮 Range，并核对冻结 exact 防止错位。 */
export function quoteSourceRange(
  root: HTMLElement,
  quote: Pick<QuotedTextSnapshot, "exact" | "text_start_utf16" | "text_end_utf16">,
): Range | null {
  const projection = buildQuoteTextProjection(root);
  if (projection.text.slice(quote.text_start_utf16, quote.text_end_utf16) !== quote.exact) {
    return null;
  }
  const start = boundaryAtOffset(projection, quote.text_start_utf16, false);
  const end = boundaryAtOffset(projection, quote.text_end_utf16, true);
  if (!start || !end) return null;
  const range = document.createRange();
  range.setStart(start.node, start.offset);
  range.setEnd(end.node, end.offset);
  return range;
}

function buildQuoteTextProjection(root: HTMLElement): QuoteTextProjection {
  const entries: ProjectionEntry[] = [];
  let text = "";
  let previous_block: Element | null = null;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      if (!(node instanceof Text) || node.data.length === 0 || !isIncludedText(root, node)) {
        return NodeFilter.FILTER_REJECT;
      }
      return NodeFilter.FILTER_ACCEPT;
    },
  });
  let current = walker.nextNode();
  while (current) {
    const node = current as Text;
    const block = node.parentElement?.closest(BLOCK_SELECTOR) ?? null;
    if (text && block && previous_block && block !== previous_block && !text.endsWith("\n")) {
      text += "\n";
    }
    const start = text.length;
    text += node.data;
    entries.push({ node, start, end: text.length });
    previous_block = block ?? previous_block;
    current = walker.nextNode();
  }
  return { text, entries };
}

function isIncludedText(root: HTMLElement, node: Text): boolean {
  const parent = node.parentElement;
  if (!parent || parent.closest(EXCLUDED_SELECTOR)) return false;
  const content = parent.closest<HTMLElement>(QUOTE_CONTENT_SELECTOR);
  return Boolean(content && (content === root || root.contains(content)));
}

function projectionOffset(
  container: Node,
  offset: number,
  projection: QuoteTextProjection,
): number | null {
  if (container instanceof Text) {
    const entry = projection.entries.find((candidate) => candidate.node === container);
    if (!entry || offset < 0 || offset > container.data.length) return null;
    return entry.start + offset;
  }
  if (!(container instanceof Element || container instanceof DocumentFragment)) return null;
  const child = container.childNodes[offset] ?? null;
  if (child) {
    const next = projection.entries.find((entry) => child === entry.node || child.contains(entry.node));
    if (next) return next.start;
  }
  for (let index = projection.entries.length - 1; index >= 0; index -= 1) {
    const entry = projection.entries[index];
    if (entry && (container === entry.node.parentNode || container.contains(entry.node))) {
      return entry.end;
    }
  }
  return null;
}

function boundaryAtOffset(
  projection: QuoteTextProjection,
  offset: number,
  prefer_previous: boolean,
): { node: Text; offset: number } | null {
  const exact = projection.entries.find((entry) => offset >= entry.start && offset <= entry.end);
  if (exact) return { node: exact.node, offset: offset - exact.start };
  if (prefer_previous) {
    let previous: ProjectionEntry | undefined;
    for (const entry of projection.entries) {
      if (entry.end < offset) previous = entry;
      else break;
    }
    return previous ? { node: previous.node, offset: previous.node.data.length } : null;
  }
  const next = projection.entries.find((entry) => entry.start > offset);
  return next ? { node: next.node, offset: 0 } : null;
}

function rangeIntersectsExcludedContent(root: HTMLElement, range: Range): boolean {
  return Array.from(root.querySelectorAll(EXCLUDED_SELECTOR)).some((element) => {
    try {
      return range.intersectsNode(element);
    } catch {
      return true;
    }
  });
}

function containsBoundary(root: HTMLElement, node: Node): boolean {
  return node === root || root.contains(node);
}

function takeFirstCodePoints(value: string, count: number): string {
  return Array.from(value).slice(0, count).join("");
}

function takeLastCodePoints(value: string, count: number): string {
  return Array.from(value).slice(-count).join("");
}
