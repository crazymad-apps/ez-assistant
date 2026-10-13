import { describe, expect, it } from "vitest";
import type { ChildTaskTreeItemSnapshot } from "@ez-assistant/protocol";
import { mergeChildTaskItems } from "../../src/features/conversation/childTaskPresentation";

function child(id: string, used: number, window: number): ChildTaskTreeItemSnapshot {
  return { task: { child_task_id: id, session_id: "s", parent_run_id: "r", parent_tool_call_id: id,
    title: id, status: "running", variant: "build", cancel_requested: false, final_text: "", error: null,
    created_at_ms: 0, started_at_ms: 0, finished_at_ms: null }, can_cancel: true, pending_approval_count: 0,
    usage: { accumulated: null, context: { accuracy: "estimated", mode: "local_summary", used_tokens: used, window_tokens: window, usage_basis_points: 1000 } } };
}

describe("child context projection", () => {
  it("keeps reliable cumulative usage and compressed windows separate for each running child", () => {
    const a = child("a", 100, 1000), b = child("b", 500, 5000);
    a.usage.accumulated = { input_tokens: 3900, output_tokens: 100, total_tokens: 4000, cached_input_tokens: null };
    const result = mergeChildTaskItems([a, b], [a.task, b.task]);
    expect(result[0].usage.context).toEqual(a.usage.context);
    expect(result[1].usage.context).toEqual(b.usage.context);
    expect(result[0].usage.accumulated?.total_tokens).toBe(4000);
    expect(result[1].usage.accumulated).toBeNull();
  });

  it("updates cumulative usage across requests and preserves it when the child finishes", () => {
    const item = child("a", 100_000, 258_000);
    item.usage.accumulated = { input_tokens: 99_000, output_tokens: 1000, total_tokens: 100_000, cached_input_tokens: null };
    const running = { ...item.task, status: "running" as const };
    expect(mergeChildTaskItems([item], [running])[0].usage.accumulated?.total_tokens).toBe(100_000);

    item.usage.accumulated = { input_tokens: 217_000, output_tokens: 2000, total_tokens: 219_000, cached_input_tokens: null };
    item.usage.context = { accuracy: "estimated", mode: "local_summary", used_tokens: 120_000, window_tokens: 258_000, usage_basis_points: 4651 };
    expect(mergeChildTaskItems([item], [running])[0].usage).toEqual(item.usage);

    const finished = { ...running, status: "completed" as const };
    expect(mergeChildTaskItems([item], [finished])[0].usage).toEqual(item.usage);
    expect(mergeChildTaskItems([item], [])[0].usage).toEqual(item.usage);
  });

  it("does not invent usage before the first reliable snapshot", () => {
    const item = child("a", 100, 1000);
    expect(mergeChildTaskItems([], [item.task])[0].usage).toEqual({ accumulated: null });
  });
});
