import { useLayoutEffect, useMemo, useRef, useState, type RefObject } from "react";
import type { ConversationNavigationItem, MessageId } from "@ez-assistant/protocol";
import type { ConversationNavigationStore, NavigationDirection } from "../../../../stores/ConversationNavigationStore";

export const ROW_HEIGHT = 12;
const BUFFER = 8;
const PAGE_THRESHOLD = 96;
const FOLLOW_THRESHOLD = 24;

type VisibleRange = Readonly<{ first: number; last: number; start: number; end: number }>;
type Bookmark = Readonly<{ id: MessageId; offset: number }>;

function visibleRange(top: number, height: number, inset: number, count: number): VisibleRange {
  const first = Math.max(0, Math.min(count - 1, Math.floor((top - inset) / ROW_HEIGHT)));
  const last = Math.min(count, Math.max(first + 1, Math.ceil((top + height - inset) / ROW_HEIGHT)));
  return { first, last, start: Math.max(0, first - BUFFER), end: Math.min(count, last + BUFFER) };
}

/** 保留首个可见 ID 和像素偏移；方向只由用户滚动改变。 */
export function useNavigationWindow(
  store: ConversationNavigationStore,
  scroll_ref: RefObject<HTMLElement | null>,
  available_height: number,
) {
  const pages = store.pages;
  const { items, indices } = useMemo(() => {
    const items: ConversationNavigationItem[] = [];
    const indices = new Map<MessageId, number>();
    for (const page of pages) for (const item of page.value.items) {
      if (!indices.has(item.user_message_id)) {
        indices.set(item.user_message_id, items.length);
        items.push(item);
      }
    }
    return { items, indices };
  }, [pages]);
  const has_older = Boolean(pages[0]?.value.previous_cursor);
  const has_newer = Boolean(pages.at(-1)?.value.next_cursor);
  const boundary_status = store.loading_page || Boolean(store.page_error);
  const before = boundary_status && store.page_direction === "older" ? ROW_HEIGHT : 0;
  const after = boundary_status && store.page_direction !== "older" ? ROW_HEIGHT : 0;
  const total = before + items.length * ROW_HEIGHT + after;
  const height = Math.min(total, available_height);
  const [top, setTop] = useState(0);
  const bookmark = useRef<Bookmark | null>(null);
  const following = useRef(true);
  const expected_top = useRef<number | null>(null);
  const last_top = useRef(0);
  const range = visibleRange(top, height, before, items.length);

  function reportWindow(value: number) {
    const visible = visibleRange(value, height, before, items.length);
    const item = items[visible.first];
    bookmark.current = item ? { id: item.user_message_id, offset: before + visible.first * ROW_HEIGHT - value } : null;
    last_top.current = value;
    setTop(value);
    const cursors = pages.filter(page => page.value.items.some(entry => {
      const index = indices.get(entry.user_message_id);
      return index !== undefined && index >= visible.start && index < visible.end;
    })).map(page => page.cursor);
    // 已跟随时，末页重验后可能附带新增 ID 的游标。
    store.setVisiblePages(cursors, following.current, item?.user_message_id ?? null);
  }

  function align(value: number) {
    const node = scroll_ref.current;
    if (!node) return;
    node.scrollTop = value;
    expected_top.current = node.scrollTop;
    reportWindow(node.scrollTop);
  }

  useLayoutEffect(() => {
    const node = scroll_ref.current;
    if (!node) return;
    const previous = bookmark.current;
    const index = previous ? indices.get(previous.id) : undefined;
    align(following.current ? total - height
      : previous && index !== undefined ? before + index * ROW_HEIGHT - previous.offset : last_top.current);
  }, [height, indices, before, total, pages]);

  function readBoundary(direction: NavigationDirection) {
    if (store.page_error) return Promise.resolve(false);
    return store.loadPage(direction);
  }

  function followLatest() {
    following.current = true;
    align(total - height);
  }

  function pauseFollow() {
    const node = scroll_ref.current;
    if (!node || !following.current) return;
    following.current = false;
    reportWindow(node.scrollTop);
  }

  function reveal(index: number, towards_latest: boolean) {
    const node = scroll_ref.current;
    if (!node) return;
    const y = before + index * ROW_HEIGHT;
    if (y < node.scrollTop) align(y);
    else if (y + ROW_HEIGHT > node.scrollTop + height) align(y + ROW_HEIGHT - height);
    if (towards_latest && !has_newer && total - height - node.scrollTop <= FOLLOW_THRESHOLD && !following.current) {
      following.current = true;
      reportWindow(node.scrollTop);
    }
  }

  function browse(delta: number) {
    const node = scroll_ref.current;
    if (!node || delta === 0) return;
    const bottom = total - height - node.scrollTop;
    if (delta < 0) following.current = false;
    else if (!has_newer && bottom <= FOLLOW_THRESHOLD) following.current = true;
    reportWindow(node.scrollTop);
    if (delta < 0 && has_older && node.scrollTop <= PAGE_THRESHOLD) void readBoundary("older");
    if (delta > 0 && has_newer && bottom <= PAGE_THRESHOLD) void readBoundary("newer");
  }

  function onScroll() {
    const node = scroll_ref.current;
    if (!node) return false;
    const expected = expected_top.current;
    expected_top.current = null;
    const delta = node.scrollTop - last_top.current;
    if (expected !== null && Math.abs(expected - node.scrollTop) < 1 || delta === 0) return false;
    browse(delta);
    return true;
  }

  return { items, indices, range, height, total, before, has_older, has_newer,
    browse, onScroll, reveal, followLatest, pauseFollow, readBoundary };
}
