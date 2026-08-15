import type {
  ChildTaskId,
  ChildTaskSnapshot,
  ChildTaskStatus,
  ChildTaskTreeItemSnapshot,
} from "../../generated/assistant-protocol";
import type { LiveRunProjection } from "../../stores/LiveExecutionStore";

export function mergeChildTaskItems(
  reliable_items: readonly ChildTaskTreeItemSnapshot[],
  live_tasks: readonly ChildTaskSnapshot[],
  live_run_for: (child_task_id: ChildTaskId) => LiveRunProjection | null,
): ChildTaskTreeItemSnapshot[] {
  const merged = new Map(reliable_items.map((item) => [item.task.child_task_id, item]));
  for (const live_task of live_tasks) {
    const reliable = merged.get(live_task.child_task_id);
    const live_usage = live_run_for(live_task.child_task_id)?.usage;
    merged.set(live_task.child_task_id, {
      task: reliable
        ? {
            ...reliable.task,
            ...live_task,
            final_text: live_task.final_text || reliable.task.final_text,
            error: live_task.error ?? reliable.task.error,
          }
        : live_task,
      usage: live_usage
        ? {
            accumulated: {
              input_tokens: live_usage.input_tokens,
              output_tokens: live_usage.output_tokens,
              total_tokens: live_usage.total_tokens,
              cached_input_tokens: live_usage.cached_input_tokens,
            },
          }
        : reliable?.usage ?? { accumulated: null },
      pending_approval_count: reliable?.pending_approval_count ?? 0,
      can_cancel: !isTerminalChildTask(live_task.status),
    });
  }
  return [...merged.values()].sort((left, right) => (
    left.task.created_at_ms - right.task.created_at_ms
      || left.task.child_task_id.localeCompare(right.task.child_task_id)
  ));
}

export function childTaskStatusLabel(status: ChildTaskStatus): string {
  return {
    accepted: "等待执行",
    running: "执行中",
    completed: "已完成",
    failed: "失败",
    cancelled: "已取消",
    interrupted: "已中断",
  }[status];
}

export function formatCompactTokens(value: number): string {
  return value >= 1_000
    ? `${(value / 1_000).toFixed(value >= 10_000 ? 0 : 1)}K Token`
    : `${value} Token`;
}

function isTerminalChildTask(status: ChildTaskStatus): boolean {
  return status === "completed"
    || status === "failed"
    || status === "cancelled"
    || status === "interrupted";
}
