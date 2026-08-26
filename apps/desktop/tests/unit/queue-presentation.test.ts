import { describe, expect, it } from "vitest";
import type { QueueSnapshot } from "../../src/generated/assistant-protocol";
import { queuePresentation } from "../../src/features/composer/ComposerDock/queuePresentation";

const queue: QueueSnapshot = {
  revision: 1,
  state: "automatic",
  items: [
    { input_id: "head", text_preview: "正在交接", submitted_at_ms: 1, position: 1, is_prioritized: false, held_by_goal: false, source: { type: "user" } },
    { input_id: "next", text_preview: "继续排队", submitted_at_ms: 2, position: 2, is_prioritized: false, held_by_goal: false, source: { type: "user" } },
  ],
};

describe("queuePresentation", () => {
  it("hides only an automatically dispatching head from every queue surface", () => {
    const presentation = queuePresentation(queue, null);
    expect(presentation.items.map((item) => item.input_id)).toEqual(["next"]);
    expect(presentation.count).toBe(1);
    expect(presentation.visible).toBe(true);
  });

  it("keeps the full queue when a run is active or the head is goal-held", () => {
    expect(queuePresentation(queue, "run-1").count).toBe(2);
    expect(queuePresentation({ ...queue, items: [{ ...queue.items[0]!, held_by_goal: true }] }, null).count).toBe(1);
  });
});
