import { describe, expect, it } from "vitest";
import type { ChildTaskTreeItemSnapshot } from "@ez-assistant/protocol";
import type { LiveRunProjection } from "../../src/stores/LiveExecutionStore";
import { mergeChildTaskItems } from "../../src/features/conversation/childTaskPresentation";

function child(id: string, used: number, window: number): ChildTaskTreeItemSnapshot {
  return { task: { child_task_id: id, session_id: "s", parent_run_id: "r", parent_tool_call_id: id,
    title: id, status: "running", variant: "build", cancel_requested: false, final_text: "", error: null,
    created_at_ms: 0, started_at_ms: 0, finished_at_ms: null }, can_cancel: true, pending_approval_count: 0,
    usage: { accumulated: null, context: { used_tokens: used, window_tokens: window, usage_basis_points: 1000 } } };
}
describe("child context projection", () => {
  it("keeps each reliable compressed window separate from live cumulative usage and siblings", () => {
    const a = child("a", 100, 1000), b = child("b", 500, 5000);
    const result = mergeChildTaskItems([a, b], [a.task, b.task], () => ({ usage: {
      input_tokens: 1900, output_tokens: 100, total_tokens: 2000, cached_input_tokens: null,
    } } as LiveRunProjection));
    expect(result[0].usage.context).toEqual(a.usage.context);
    expect(result[1].usage.context).toEqual(b.usage.context);
    expect(result[0].usage.accumulated?.total_tokens).toBe(2000);
    expect(mergeChildTaskItems([], [a.task], () => null)[0].usage.context).toBeUndefined();
  });
});
