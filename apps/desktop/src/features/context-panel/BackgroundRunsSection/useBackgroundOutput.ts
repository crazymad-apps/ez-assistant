import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ReadBackgroundOutputRequest, ReadBackgroundOutputResult, SessionId } from "@ez-assistant/protocol";
import { RuntimeClientError, type RuntimeClient } from "../../../runtime-client/RuntimeClient";

const PAGE_CHARACTERS = 2_000;
const MAX_BUFFER_CHARACTERS = 128_000;
const MAX_CHUNKS = 128;
const POLL_MS = 750;

type Direction = "initial" | "older" | "newer";
type OutputChunk = Pick<ReadBackgroundOutputResult, "cursor" | "next_cursor" | "content">;
type OutputView = Readonly<{
  chunks: readonly OutputChunk[];
  loading: boolean;
  ready: boolean;
  error: string | null;
  dropped: number;
}>;
type Placement = { type: "bottom" } | { type: "anchor"; element: Element; top: number };
const EMPTY: OutputView = { chunks: [], loading: true, ready: false, error: null, dropped: 0 };

export function useBackgroundOutput(
  client: RuntimeClient,
  session_id: SessionId,
  background_id: string,
  on_end: () => void,
  schedule_execution_id?: string,
) {
  const viewport = useRef<HTMLDivElement>(null);
  const placement = useRef<Placement | null>(null);
  const scroll_request = useRef<(upward?: boolean) => void>(() => {});
  const retry_request = useRef<() => void>(() => {});
  const align_tail = useRef<() => void>(() => {});
  const onReflow = useCallback(() => align_tail.current(), []);
  const end_callback = useRef(on_end);
  end_callback.current = on_end;
  const [view, setView] = useState<OutputView>(EMPTY);

  useLayoutEffect(() => {
    const area = viewport.current;
    const next = placement.current;
    placement.current = null;
    if (!area || !next) return;
    if (next.type === "bottom") area.scrollTop = area.scrollHeight;
    else if (area.contains(next.element)) {
      area.scrollTop += next.element.getBoundingClientRect().top - next.top;
    }
  }, [view]);

  useEffect(() => {
    const lifetime = new AbortController();
    let current = EMPTY;
    let floor = 0;
    let following = true;
    let pending = false;
    let timer: number | undefined;
    let failed_direction: Direction = "initial";

    const update = (next: Partial<OutputView>) => {
      current = { ...current, ...next };
      setView(current);
    };
    const schedule = () => {
      window.clearTimeout(timer);
      if (following && !current.error && !lifetime.signal.aborted) {
        timer = window.setTimeout(() => { void read("newer"); }, POLL_MS);
      }
    };
    const applyPage = (direction: Direction, page: ReadBackgroundOutputResult) => {
      const first = current.chunks[0];
      const last = current.chunks.at(-1);
      if (page.dropped_chars > 0) floor = Math.max(floor, page.cursor);
      const dropped = page.dropped_chars || current.dropped;
      if (direction === "older" && first) {
        const end = Math.min(first.cursor, page.next_cursor);
        if (end <= page.cursor || following) {
          update({ dropped });
          return;
        }
        const area = viewport.current;
        const anchor = area?.querySelector("[data-output-cursor]");
        if (area && anchor) {
          const top = anchor.getBoundingClientRect().top;
          if (top <= area.getBoundingClientRect().top + 48) placement.current = { type: "anchor", element: anchor, top };
        }
        const prefix = end === page.next_cursor ? page.content : [...page.content].slice(0, end - page.cursor).join("");
        update({ chunks: boundWindow([{ cursor: page.cursor, next_cursor: end, content: prefix }, ...current.chunks], "older"), dropped });
      } else if (following) {
        if (direction === "newer" && last?.next_cursor === page.next_cursor) return;
        placement.current = { type: "bottom" };
        const chunk = { cursor: page.cursor, next_cursor: page.next_cursor, content: page.content };
        const base = direction === "initial" || !last?.content || last.next_cursor !== page.cursor ? [] : current.chunks;
        update({ chunks: boundWindow([...base, chunk], "newer"), ready: true, dropped });
      }
    };

    async function read(direction: Direction) {
      if (lifetime.signal.aborted || pending || (direction === "newer" && !following)) return;
      const first = current.chunks[0];
      if (direction === "older" && (!first || first.cursor <= floor)) return;
      const span = Math.min(first?.cursor ?? 0, PAGE_CHARACTERS);
      const request: ReadBackgroundOutputRequest = { session_id, background_id, limit_chars: PAGE_CHARACTERS };
      if (direction === "older" && first) {
        request.cursor = first.cursor;
        request.offset_chars = -span;
        request.limit_chars = span;
      } else if (direction === "newer") {
        request.cursor = current.chunks.at(-1)?.next_cursor;
      }
      window.clearTimeout(timer);
      pending = true;
      if (direction !== "newer" || current.error) update({ error: null, loading: direction !== "newer" });
      try {
        const page = schedule_execution_id
          ? (await client.command({ type: "read_schedule_background_output", payload: {
            execution_id: schedule_execution_id, background_id, cursor: request.cursor,
            offset_chars: request.offset_chars, limit_chars: request.limit_chars,
          } }, { signal: lifetime.signal })).payload
          : await client.readBackgroundOutput(request, lifetime.signal);
        if (!lifetime.signal.aborted) applyPage(direction, page);
      } catch (error) {
        if (lifetime.signal.aborted) return;
        if (error instanceof RuntimeClientError && error.code === "background_run_not_found") {
          lifetime.abort();
          end_callback.current();
          return;
        }
        failed_direction = direction;
        update({ error: error instanceof Error ? error.message : "无法读取后台输出。" });
      } finally {
        pending = false;
        if (!lifetime.signal.aborted) {
          if (current.loading) update({ loading: false });
          schedule();
        }
      }
    }

    scroll_request.current = (upward = false) => {
      const area = viewport.current;
      if (!area || !current.ready) return;
      const was_following = following;
      following = !upward && area.scrollHeight - area.clientHeight - area.scrollTop <= 12;
      if (!following) window.clearTimeout(timer);
      if (current.error) return;
      if (following && !was_following) void read("newer");
      else if (!following && area.scrollTop <= 48) void read("older");
    };
    retry_request.current = () => {
      if (failed_direction === "newer" && !following) update({ error: null });
      else void read(failed_direction);
    };
    align_tail.current = () => {
      const area = viewport.current;
      if (following && current.ready && area) area.scrollTop = area.scrollHeight;
    };
    const resize = new ResizeObserver(onReflow);
    if (viewport.current) resize.observe(viewport.current);
    void read("initial");
    return () => { lifetime.abort(); window.clearTimeout(timer); resize.disconnect(); };
  }, [client, session_id, background_id, schedule_execution_id, onReflow]);

  return {
    view,
    viewport,
    onScroll: () => scroll_request.current(),
    onScrollUp: () => scroll_request.current(true),
    retry: () => retry_request.current(),
    onReflow,
  };
}

function boundWindow(chunks: OutputChunk[], direction: "older" | "newer"): OutputChunk[] {
  while (chunks.length > 1 && (
    chunks.length > MAX_CHUNKS || chunks.at(-1)!.next_cursor - chunks[0].cursor > MAX_BUFFER_CHARACTERS
  )) {
    if (direction === "older") chunks.pop();
    else chunks.shift();
  }
  return chunks;
}
