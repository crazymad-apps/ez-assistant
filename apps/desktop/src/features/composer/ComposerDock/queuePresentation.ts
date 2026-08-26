import type { QueueSnapshot, QueuedInputSnapshot, RunId } from "../../../generated/assistant-protocol";

export type QueuePresentation = Readonly<{
  items: readonly QueuedInputSnapshot[];
  count: number;
  visible: boolean;
}>;

/**
 * Hides only the queue head that Runtime has already selected for automatic dispatch.
 * The selector is shared by drawer visibility, badge count and rendered rows so the
 * optimistic hand-off cannot flash as a pending item in one UI surface only.
 */
export function queuePresentation(
  queue: QueueSnapshot,
  active_run_id: RunId | null,
): QueuePresentation {
  const head_is_dispatching = queue.state === "automatic"
    && active_run_id === null
    && queue.items[0]?.held_by_goal === false;
  const items = head_is_dispatching ? queue.items.slice(1) : queue.items;
  return { items, count: items.length, visible: items.length > 0 };
}
