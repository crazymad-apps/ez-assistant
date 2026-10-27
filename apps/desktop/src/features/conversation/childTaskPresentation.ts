import type {
  ChildTaskSnapshot,
  ChildTaskStatus,
  ChildTaskTreeItemSnapshot,
} from "@ez-assistant/protocol";
import { childKey } from "../../stores/liveExecutionProjection";

export function mergeChildTaskItems(
  reliable_items: readonly ChildTaskTreeItemSnapshot[],
  live_tasks: readonly ChildTaskSnapshot[],
): ChildTaskTreeItemSnapshot[] {
  const merged = new Map(reliable_items.map((item) => [childKey(item.task.session_id, item.task.child_task_id), item]));
  for (const live_task of live_tasks) {
    const key = childKey(live_task.session_id, live_task.child_task_id);
    const reliable = merged.get(key);
    if (reliable?.task.inherited) continue;
    merged.set(key, {
      task: reliable
        ? {
            ...reliable.task,
            ...live_task,
            final_text: live_task.final_text || reliable.task.final_text,
            error: live_task.error ?? reliable.task.error,
          }
        : live_task,
      // 实时 usage 只属于单个 Step；窗口与累计均沿用 Runtime 刷新后的可靠投影。
      usage: reliable?.usage ?? { accumulated: null },
      pending_approval_count: reliable?.pending_approval_count ?? 0,
      can_cancel: !live_task.inherited && !live_task.body_unavailable && !isTerminalChildTask(live_task.status),
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
